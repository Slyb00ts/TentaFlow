// =============================================================================
// Plik: flow_engine/blob_store.rs
// Opis: BlobStore trait + dwie implementacje (FileBlobStore sharded path,
//       InMemoryBlobStore for tests). Trzyma duze payloady (audio/image/video)
//       poza FlowEnvelope — envelope niesie tylko BlobRef (id + metadata).
// =============================================================================

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Reference to a blob stored outside FlowEnvelope. Cloning is cheap; bytes
/// stay in BlobStore. id is uuid v4 for the row, sha256 is content hash for
/// dedup/integrity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobRef {
    pub id: String,
    pub size_bytes: u64,
    pub mime: String,
    pub sha256: String,
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, bytes: Vec<u8>, mime: &str) -> Result<BlobRef>;
    async fn get(&self, blob_ref: &BlobRef) -> Result<Vec<u8>>;
    /// GC orphan blobs older than `retention`. Returns count removed.
    /// Stub in stage 1 — scheduler + orphan tracking dochodzi w stage 2.
    /// Per-ref delete jest świadomie pominięty: dedup-by-sha sprawia że dwa
    /// `BlobRef` mogą wskazywać na ten sam plik i naiwne usunięcie po jednym
    /// rozsadza drugi. GC z refcount/orphan registry rozwiązuje to w E2.
    async fn gc(&self, retention: Duration) -> Result<u64>;
}

/// Filesystem-backed blob store with sharded layout to keep directories small:
/// `<root>/<sha[0:2]>/<sha[2:4]>/<full_sha>.bin`
///
/// Audio/video to GB — SQLite BLOB ma write perf issues > 1MB, fs page cache
/// for free, GC = `rm -rf orphans`, backup = `rsync`.
pub struct FileBlobStore {
    root: PathBuf,
}

impl FileBlobStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn sharded_path(&self, sha256: &str) -> PathBuf {
        let mut path = self.root.clone();
        path.push(&sha256[0..2]);
        path.push(&sha256[2..4]);
        path.push(format!("{sha256}.bin"));
        path
    }
}

#[async_trait]
impl BlobStore for FileBlobStore {
    async fn put(&self, bytes: Vec<u8>, mime: &str) -> Result<BlobRef> {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha256 = format!("{:x}", hasher.finalize());

        let path = self.sharded_path(&sha256);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        // Dedup: jeśli plik istnieje, weryfikujemy zawartość zanim go reuse.
        // Crash mid-write z poprzedniego put-a mógł zostawić uszkodzony plik
        // pod finalną nazwą — wtedy traktujemy go jak nieobecny i nadpisujemy
        // przez normalną ścieżkę temp+rename. Transient I/O error (permission,
        // sharing) propaguje się jako Err — bez kasowania pliku.
        if fs::try_exists(&path).await.unwrap_or(false) {
            match verify_sha_on_disk(&path, &sha256).await {
                Ok(true) => {
                    return Ok(BlobRef {
                        id: uuid::Uuid::new_v4().to_string(),
                        size_bytes: bytes.len() as u64,
                        mime: mime.to_string(),
                        sha256,
                    });
                }
                Ok(false) => {
                    // Realnie corrupted blob — usuwamy i przepisujemy.
                    let _ = fs::remove_file(&path).await;
                }
                Err(e) => {
                    return Err(anyhow!(
                        "verify dedup target {}: {e}",
                        path.display()
                    ));
                }
            }
        }

        // Atomic write: temp file w tym samym katalogu (żeby rename był same-fs)
        // → fsync danych → rename na docelową nazwę. Crash przed rename = sierota
        // w temp; crash po rename = zdrowy plik. Brak corrupted-blob window
        // pod finalną nazwą.
        let tmp_path = path.with_extension(format!(
            "tmp-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = fs::File::create(&tmp_path)
            .await
            .with_context(|| format!("create temp {}", tmp_path.display()))?;
        file.write_all(&bytes)
            .await
            .with_context(|| format!("write {}", tmp_path.display()))?;
        file.sync_all()
            .await
            .with_context(|| format!("fsync {}", tmp_path.display()))?;
        drop(file);

        if let Err(e) = fs::rename(&tmp_path, &path).await {
            // Race: równoległy put tej samej zawartości mógł już dograć target.
            // Na Windows rename do istniejącego pliku failuje z "target exists";
            // na Unix nadpisuje. W obu przypadkach jeśli docelowy plik ma
            // poprawne sha, to jest zdrowy i nasz temp jest redundantny —
            // sprzątamy temp i zwracamy success.
            let target_ok = matches!(verify_sha_on_disk(&path, &sha256).await, Ok(true));
            let _ = fs::remove_file(&tmp_path).await;
            if !target_ok {
                return Err(anyhow!(
                    "rename {} -> {}: {e}",
                    tmp_path.display(),
                    path.display()
                ));
            }
        }

        Ok(BlobRef {
            id: uuid::Uuid::new_v4().to_string(),
            size_bytes: bytes.len() as u64,
            mime: mime.to_string(),
            sha256,
        })
    }

    async fn get(&self, blob_ref: &BlobRef) -> Result<Vec<u8>> {
        let path = self.sharded_path(&blob_ref.sha256);
        let bytes = fs::read(&path)
            .await
            .with_context(|| format!("read blob {}", path.display()))?;

        // Integrity check — corrupted file would silently propagate otherwise.
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = format!("{:x}", hasher.finalize());
        if actual != blob_ref.sha256 {
            return Err(anyhow!(
                "blob sha256 mismatch: expected {}, got {}",
                blob_ref.sha256,
                actual
            ));
        }

        Ok(bytes)
    }

    async fn gc(&self, retention: Duration) -> Result<u64> {
        // Etap 2: walka po sharded layout `<root>/<sha[0:2]>/<sha[2:4]>/*.bin`.
        // Każdy plik z mtime starszym niż `retention` usuwany. Race vs
        // concurrent put tego samego content: get-after-delete dostaje
        // NotFound, caller (TTS adapter) potraktuje jako transient błąd.
        // Refcount/orphan registry wraz z multi-process scenariuszami
        // wraca w stage 3.
        let root = self.root.clone();
        let cutoff = std::time::SystemTime::now()
            .checked_sub(retention)
            .unwrap_or(std::time::UNIX_EPOCH);
        let removed = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            let mut count = 0u64;
            let shards = match std::fs::read_dir(&root) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(e),
            };
            for shard in shards.flatten() {
                if !shard.file_type()?.is_dir() {
                    continue;
                }
                for sub in std::fs::read_dir(shard.path())?.flatten() {
                    if !sub.file_type()?.is_dir() {
                        continue;
                    }
                    for blob in std::fs::read_dir(sub.path())?.flatten() {
                        let meta = match blob.metadata() {
                            Ok(m) => m,
                            Err(_) => continue,
                        };
                        if !meta.is_file() {
                            continue;
                        }
                        // Skip *.tmp-* (in-progress put — atomic write rename
                        // jeszcze się nie wydarzyło).
                        if let Some(ext) = blob.path().extension().and_then(|s| s.to_str()) {
                            if ext.starts_with("tmp-") {
                                continue;
                            }
                        }
                        let modified = meta.modified().unwrap_or_else(|_| std::time::SystemTime::now());
                        if modified < cutoff {
                            if std::fs::remove_file(blob.path()).is_ok() {
                                count += 1;
                            }
                        }
                    }
                }
            }
            Ok(count)
        })
        .await
        .map_err(|e| anyhow!("blob gc join: {e}"))??;
        Ok(removed)
    }
}

/// Sprawdza czy plik na dysku rzeczywiście ma deklarowany sha256. Używane
/// przy dedup żeby nie reuse uszkodzonego pliku po crashu mid-write z poprzedniej
/// sesji.
async fn verify_sha_on_disk(path: &std::path::Path, expected_sha: &str) -> Result<bool> {
    let bytes = fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = format!("{:x}", hasher.finalize());
    Ok(actual == expected_sha)
}

/// In-memory store for tests. Stores bytes by sha256.
pub struct InMemoryBlobStore {
    inner: RwLock<HashMap<String, Vec<u8>>>,
}

impl InMemoryBlobStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.len()).unwrap_or(0)
    }
}

impl Default for InMemoryBlobStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BlobStore for InMemoryBlobStore {
    async fn put(&self, bytes: Vec<u8>, mime: &str) -> Result<BlobRef> {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha256 = format!("{:x}", hasher.finalize());

        let size_bytes = bytes.len() as u64;
        self.inner
            .write()
            .map_err(|_| anyhow!("InMemoryBlobStore poisoned"))?
            .insert(sha256.clone(), bytes);

        Ok(BlobRef {
            id: uuid::Uuid::new_v4().to_string(),
            size_bytes,
            mime: mime.to_string(),
            sha256,
        })
    }

    async fn get(&self, blob_ref: &BlobRef) -> Result<Vec<u8>> {
        self.inner
            .read()
            .map_err(|_| anyhow!("InMemoryBlobStore poisoned"))?
            .get(&blob_ref.sha256)
            .cloned()
            .ok_or_else(|| anyhow!("blob not found: {}", blob_ref.sha256))
    }

    async fn gc(&self, _retention: Duration) -> Result<u64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn in_memory_round_trip() {
        let store = InMemoryBlobStore::new();
        let bytes = b"hello world".to_vec();
        let blob = store.put(bytes.clone(), "text/plain").await.unwrap();
        assert_eq!(blob.size_bytes, 11);
        assert_eq!(blob.mime, "text/plain");
        let got = store.get(&blob).await.unwrap();
        assert_eq!(got, bytes);
    }

    #[tokio::test]
    async fn in_memory_dedup_by_sha() {
        let store = InMemoryBlobStore::new();
        let blob_a = store.put(b"same".to_vec(), "text/plain").await.unwrap();
        let blob_b = store.put(b"same".to_vec(), "text/plain").await.unwrap();
        assert_eq!(blob_a.sha256, blob_b.sha256);
        assert_ne!(blob_a.id, blob_b.id, "ids unique per put");
        assert_eq!(store.len(), 1, "deduplicated by sha");
    }

    #[tokio::test]
    async fn in_memory_get_missing() {
        let store = InMemoryBlobStore::new();
        let bogus = BlobRef {
            id: "x".into(),
            size_bytes: 0,
            mime: "text/plain".into(),
            sha256: "deadbeef".into(),
        };
        assert!(store.get(&bogus).await.is_err());
    }

    #[tokio::test]
    async fn file_round_trip_sharded_path() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        let bytes = b"file blob content".to_vec();
        let blob = store.put(bytes.clone(), "application/octet-stream").await.unwrap();

        let on_disk = dir
            .path()
            .join(&blob.sha256[0..2])
            .join(&blob.sha256[2..4])
            .join(format!("{}.bin", blob.sha256));
        assert!(on_disk.exists(), "expected file at {}", on_disk.display());

        let got = store.get(&blob).await.unwrap();
        assert_eq!(got, bytes);
    }

    #[tokio::test]
    async fn file_dedup_does_not_rewrite() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        let _a = store.put(b"same".to_vec(), "text/plain").await.unwrap();
        let _b = store.put(b"same".to_vec(), "text/plain").await.unwrap();
        let mut count = 0u32;
        let mut walker = walkdir(&dir.path().to_path_buf());
        while let Some(e) = walker.next() {
            if e.is_file() {
                count += 1;
            }
        }
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn file_get_detects_corruption() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        let blob = store.put(b"original".to_vec(), "text/plain").await.unwrap();
        let on_disk = dir
            .path()
            .join(&blob.sha256[0..2])
            .join(&blob.sha256[2..4])
            .join(format!("{}.bin", blob.sha256));
        // Tamper with bytes — sha256 in BlobRef won't match anymore.
        std::fs::write(&on_disk, b"corrupted").unwrap();
        let err = store.get(&blob).await.unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    }

    #[tokio::test]
    async fn file_put_recovers_from_corrupted_dedup_target() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        let blob = store.put(b"good".to_vec(), "text/plain").await.unwrap();
        let on_disk = dir
            .path()
            .join(&blob.sha256[0..2])
            .join(&blob.sha256[2..4])
            .join(format!("{}.bin", blob.sha256));
        // Symuluj crashed mid-write z poprzedniej sesji — plik istnieje pod
        // finalną nazwą, ale sha się nie zgadza.
        std::fs::write(&on_disk, b"corrupted").unwrap();
        // Kolejny put tej samej zawartości powinien wykryć korupcję, usunąć
        // plik i zapisać poprawnie przez temp+rename.
        let blob2 = store.put(b"good".to_vec(), "text/plain").await.unwrap();
        assert_eq!(blob2.sha256, blob.sha256);
        let got = store.get(&blob2).await.unwrap();
        assert_eq!(got, b"good");
    }

    #[tokio::test]
    async fn file_put_atomic_no_temp_leftovers() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        let _ = store.put(b"x".to_vec(), "text/plain").await.unwrap();
        // Po udanym put-cie żaden plik tmp nie powinien zostać.
        let mut leftovers = 0u32;
        for p in walkdir(&dir.path().to_path_buf()) {
            if p.extension().and_then(|s| s.to_str()).map(|s| s.starts_with("tmp-")).unwrap_or(false)
                || p.to_string_lossy().contains(".tmp-")
            {
                leftovers += 1;
            }
        }
        assert_eq!(leftovers, 0);
    }

    #[tokio::test]
    async fn file_gc_removes_old_blobs_keeps_fresh() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());

        // Put two blobs. Backdate one so that mtime jest poza retention.
        let old = store.put(b"old".to_vec(), "text/plain").await.unwrap();
        let fresh = store.put(b"fresh".to_vec(), "text/plain").await.unwrap();

        let old_path = dir
            .path()
            .join(&old.sha256[0..2])
            .join(&old.sha256[2..4])
            .join(format!("{}.bin", old.sha256));

        // Set mtime na 24h wstecz.
        let backdated = std::time::SystemTime::now() - Duration::from_secs(24 * 60 * 60);
        let f = std::fs::File::open(&old_path).unwrap();
        f.set_modified(backdated).unwrap();
        drop(f);

        // gc retention=1h: old blob (>24h stary) usunięty, fresh (świeżutki)
        // zostaje.
        let removed = store
            .gc(Duration::from_secs(60 * 60))
            .await
            .expect("gc ok");
        assert_eq!(removed, 1, "exactly old blob removed");
        assert!(!old_path.exists(), "old blob file deleted");
        // Fresh nadal czytalny przez get (integrity check przechodzi).
        let got_fresh = store.get(&fresh).await.expect("fresh still present");
        assert_eq!(got_fresh, b"fresh");
    }

    #[tokio::test]
    async fn file_gc_skips_in_progress_temp_files() {
        let dir = tempdir().unwrap();
        let store = FileBlobStore::new(dir.path().to_path_buf());
        // Symuluj crashed mid-write — plik tmp pod "tmp-*" nie powinien być
        // dotknięty przez gc (in-progress put).
        let shard = dir.path().join("aa").join("bb");
        std::fs::create_dir_all(&shard).unwrap();
        let tmp = shard.join("aabbccdd.tmp-12345");
        std::fs::write(&tmp, b"in-progress").unwrap();
        let backdated = std::time::SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        std::fs::File::open(&tmp)
            .unwrap()
            .set_modified(backdated)
            .unwrap();

        let removed = store.gc(Duration::from_secs(60 * 60)).await.unwrap();
        assert_eq!(removed, 0);
        assert!(tmp.exists(), "tmp file untouched by gc");

        // Jednocześnie real .bin starszy niż retention idzie do śmieci.
        let bin = shard.join("aabbccdd0011.bin");
        std::fs::write(&bin, b"x").unwrap();
        std::fs::File::open(&bin)
            .unwrap()
            .set_modified(backdated)
            .unwrap();
        let removed2 = store.gc(Duration::from_secs(60 * 60)).await.unwrap();
        assert_eq!(removed2, 1);
        assert!(!bin.exists());
    }

    // Minimal recursive iterator to avoid pulling walkdir as a dev-dep just for one test.
    struct Walker {
        stack: Vec<PathBuf>,
    }
    impl Walker {
        fn new(root: &PathBuf) -> Self {
            Self { stack: vec![root.clone()] }
        }
    }
    impl Iterator for Walker {
        type Item = PathBuf;
        fn next(&mut self) -> Option<Self::Item> {
            while let Some(p) = self.stack.pop() {
                if p.is_dir() {
                    if let Ok(rd) = std::fs::read_dir(&p) {
                        for entry in rd.flatten() {
                            self.stack.push(entry.path());
                        }
                    }
                } else {
                    return Some(p);
                }
            }
            None
        }
    }
    fn walkdir(root: &PathBuf) -> Walker {
        Walker::new(root)
    }
}
