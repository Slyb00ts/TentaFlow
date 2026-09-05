// ===== File: tentaquant/cas.rs — the content store of one laboratory =====
//
// File bytes never live in `tentaquant.db`: they land in
// `<instance data dir>/files/<sha256>` (plan §9.4), so identical content stored
// twice costs one blob and the database rows stay small enough to read whole.
//
// Uploads arrive in 4 MiB chunks over the binary protocol, in order, under a
// client-chosen `upload_id`. The accumulator is keyed by (org, user, instance,
// project, upload_id): a stream belongs to ONE uploader in ONE lab, so nobody
// can append to or finish somebody else's transfer, and the part file is named
// after a hash of that whole key rather than after the id the client picked.
//
// Blobs are not reclaimed here. Deleting a file or a whole project removes the
// ROWS; the `files/<sha256>` bytes stay until the retention sweep of plan §9.4
// (unpinned run artefacts older than `retention_days`, default 180, are
// collected) lands — which plan §16 schedules for phase 7, "retencja i GC",
// together with laboratory export/import. `LabAdminSettings.retention_days` is
// stored and editable before then because it is an administrator's decision
// about their own laboratory, not a switch on code that exists. Nothing here
// pretends otherwise, and the whole store still goes with the instance at
// uninstall, so a lab's storage is bounded by its own lifetime, not forever.
//
// This is the lab's own store rather than a call into Project Studio's ingest
// accumulator: that one hands its finalized blobs to `SourceCreate` through an
// app-specific registry and enforces Project Studio's own file model. Sharing
// it would tie a lab's uninstall wipe to another application's lifetime.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use sha2::{Digest, Sha256};

/// Wire chunk ceiling, matching Project Studio's upload channel — the frame
/// budget is a property of the transport, not of the application.
pub const MAX_UPLOAD_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Per-file ceiling. A notebook, a program or a QASM circuit is kilobytes;
/// this is the headroom for a data file next to them.
pub const MAX_UPLOAD_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// How long a half-finished stream survives without a chunk.
const UPLOAD_TTL_MS: i64 = 30 * 60 * 1000;

/// (org_id, instance_id, user_id, project_id, upload_id).
type UploadKey = (String, String, String, String, String);

struct PendingUpload {
    total_chunks: u32,
    next_seq: u32,
    received_bytes: u64,
    part_path: PathBuf,
    last_touch_ms: i64,
}

fn pending_uploads() -> &'static DashMap<UploadKey, PendingUpload> {
    static MAP: OnceLock<DashMap<UploadKey, PendingUpload>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Drops streams older than the TTL and removes their part files. Called lazily
/// from [`accept_chunk`] — an abandoned upload must not need a dedicated task
/// to be reclaimed.
fn sweep_expired() {
    let cutoff = now_ms() - UPLOAD_TTL_MS;
    let stale: Vec<UploadKey> = pending_uploads()
        .iter()
        .filter(|e| e.value().last_touch_ms < cutoff)
        .map(|e| e.key().clone())
        .collect();
    for key in stale {
        if let Some((_, up)) = pending_uploads().remove(&key) {
            let _ = std::fs::remove_file(&up.part_path);
        }
    }
}

fn validate_upload_id(upload_id: &str) -> Result<()> {
    if upload_id.is_empty()
        || upload_id.len() > 128
        || !upload_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(anyhow!("invalid upload_id"));
    }
    Ok(())
}

/// Part-file name derived from the FULL upload key, so two uploaders who pick
/// the same `upload_id` cannot write into one another's partial file.
fn part_file_name(key: &UploadKey) -> String {
    let mut hasher = Sha256::new();
    for segment in [&key.0, &key.1, &key.2, &key.3, &key.4] {
        hasher.update(segment.as_bytes());
        hasher.update(b"|");
    }
    format!(".upload-{}.part", hex::encode(hasher.finalize()))
}

/// A project-relative file path that is safe to store and to show. Paths are
/// database values here (the blob is named by its hash, not by this), but they
/// travel back to clients, so traversal and absolute forms are refused rather
/// than normalized away.
pub fn validate_path(path: &str) -> Result<String> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() || trimmed.len() > 512 {
        return Err(anyhow!("invalid file path"));
    }
    if trimmed.contains('\\') || trimmed.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(anyhow!("invalid file path"));
    }
    Ok(trimmed.to_string())
}

/// The `kind` values the schema accepts (`files.kind` CHECK constraint).
pub fn validate_kind(kind: &str) -> Result<&str> {
    match kind {
        "notebook" | "py" | "qasm" | "data" | "md" => Ok(kind),
        _ => Err(anyhow!("unknown file kind '{kind}'")),
    }
}

/// The blob directory of one instance.
fn files_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("files")
}

/// Where a completed upload ends up.
pub fn blob_path(data_dir: &Path, sha256: &str) -> PathBuf {
    files_dir(data_dir).join(sha256)
}

/// What one accepted chunk did.
pub enum ChunkOutcome {
    Buffered {
        received_chunks: u32,
        received_bytes: u64,
    },
    Finalized {
        sha256: String,
        received_chunks: u32,
        size_bytes: u64,
    },
}

/// Accepts one upload chunk (blocking disk IO — call from `spawn_blocking`).
/// Chunks must arrive in order and `seq == 0` (re)starts the stream. The final
/// chunk hashes the part file and renames it to `files/<sha256>`; the hash is
/// computed from the bytes on disk, so the name always describes the content.
#[allow(clippy::too_many_arguments)]
pub fn accept_chunk(
    org_id: &str,
    instance_id: &str,
    user_id: &str,
    project_id: &str,
    data_dir: &Path,
    upload_id: &str,
    seq: u32,
    total_chunks: u32,
    bytes: &[u8],
) -> Result<ChunkOutcome> {
    sweep_expired();
    validate_upload_id(upload_id)?;
    if total_chunks == 0 {
        return Err(anyhow!("total_chunks must be >= 1"));
    }
    if seq >= total_chunks {
        return Err(anyhow!("seq out of range"));
    }
    if bytes.len() > MAX_UPLOAD_CHUNK_BYTES {
        return Err(anyhow!("chunk exceeds 4 MiB limit"));
    }

    let key: UploadKey = (
        org_id.to_string(),
        instance_id.to_string(),
        user_id.to_string(),
        project_id.to_string(),
        upload_id.to_string(),
    );
    let files = files_dir(data_dir);
    std::fs::create_dir_all(&files)?;
    let part_path = files.join(part_file_name(&key));

    if seq == 0 {
        // Restarting an upload id discards any previous partial stream.
        if let Some((_, old)) = pending_uploads().remove(&key) {
            let _ = std::fs::remove_file(&old.part_path);
        }
        if bytes.len() as u64 > MAX_UPLOAD_FILE_BYTES {
            return Err(anyhow!("file exceeds 64 MiB limit"));
        }
        std::fs::write(&part_path, bytes)?;
        pending_uploads().insert(
            key.clone(),
            PendingUpload {
                total_chunks,
                next_seq: 1,
                received_bytes: bytes.len() as u64,
                part_path: part_path.clone(),
                last_touch_ms: now_ms(),
            },
        );
    } else {
        let mut entry = pending_uploads()
            .get_mut(&key)
            .ok_or_else(|| anyhow!("unknown upload_id (expired or never started)"))?;
        if entry.total_chunks != total_chunks || seq != entry.next_seq {
            return Err(anyhow!("out-of-order upload chunk"));
        }
        if entry.received_bytes + bytes.len() as u64 > MAX_UPLOAD_FILE_BYTES {
            let part = entry.part_path.clone();
            drop(entry);
            pending_uploads().remove(&key);
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!("file exceeds 64 MiB limit"));
        }
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&entry.part_path)?;
        f.write_all(bytes)?;
        entry.next_seq = seq + 1;
        entry.received_bytes += bytes.len() as u64;
        entry.last_touch_ms = now_ms();
    }

    let (received_chunks, received_bytes, complete) = {
        let entry = pending_uploads()
            .get(&key)
            .ok_or_else(|| anyhow!("upload state lost"))?;
        (
            entry.next_seq,
            entry.received_bytes,
            entry.next_seq == entry.total_chunks,
        )
    };
    if !complete {
        return Ok(ChunkOutcome::Buffered {
            received_chunks,
            received_bytes,
        });
    }

    let sha256 = hash_file(&part_path)?;
    let blob = files.join(&sha256);
    if blob.exists() {
        // Same content already stored — keep the existing blob, drop the copy.
        let _ = std::fs::remove_file(&part_path);
    } else {
        std::fs::rename(&part_path, &blob)?;
    }
    pending_uploads().remove(&key);
    Ok(ChunkOutcome::Finalized {
        sha256,
        received_chunks,
        size_bytes: received_bytes,
    })
}

/// Writes one artifact into the lab's store and answers its content hash.
///
/// Run outputs are produced in one piece (counts, a state vector, the CBOR
/// evolution) rather than streamed in chunks, so they take this path instead of
/// the upload accumulator. The write goes to a temporary file next to the blob
/// and is renamed into place, so a crash mid-write can never leave a file
/// under a name that promises different bytes.
pub fn store_blob(data_dir: &Path, bytes: &[u8]) -> Result<String> {
    let files = files_dir(data_dir);
    std::fs::create_dir_all(&files)?;
    let sha256 = hex::encode(Sha256::digest(bytes));
    let blob = files.join(&sha256);
    if blob.exists() {
        return Ok(sha256);
    }
    let temp = files.join(format!(".write-{sha256}.part"));
    std::fs::write(&temp, bytes)?;
    if let Err(error) = std::fs::rename(&temp, &blob) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(sha256)
}

/// Reads one artifact back. The name IS the hash, so a caller that got the
/// hash from a run row cannot be pointed at another lab's bytes.
pub fn read_blob(data_dir: &Path, sha256: &str) -> Result<Vec<u8>> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid content hash"));
    }
    Ok(std::fs::read(blob_path(data_dir, sha256))?)
}

/// Size of one stored artifact without reading it. The gallery tile is named
/// by the run row rather than by an output row, so its size is not recorded
/// anywhere else and the file on disk is the only honest answer.
pub fn blob_size(data_dir: &Path, sha256: &str) -> Result<u64> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid content hash"));
    }
    Ok(std::fs::metadata(blob_path(data_dir, sha256))?.len())
}

/// Streaming SHA-256 of a file, so a 64 MiB upload never sits in memory twice.
fn hash_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(dir: &Path, upload: &str, seq: u32, total: u32, bytes: &[u8]) -> Result<ChunkOutcome> {
        accept_chunk("org", "lab-a", "u1", "p1", dir, upload, seq, total, bytes)
    }

    #[test]
    fn two_chunks_finalize_into_a_content_addressed_blob() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = chunk(dir.path(), "up-1", 0, 2, b"hello ").unwrap();
        assert!(matches!(outcome, ChunkOutcome::Buffered { .. }));
        let ChunkOutcome::Finalized {
            sha256, size_bytes, ..
        } = chunk(dir.path(), "up-1", 1, 2, b"world").unwrap()
        else {
            panic!("second chunk must finalize");
        };
        assert_eq!(size_bytes, 11);
        let stored = std::fs::read(blob_path(dir.path(), &sha256)).unwrap();
        assert_eq!(stored, b"hello world");
        // The blob name IS the digest of the stored bytes.
        assert_eq!(
            sha256,
            super::hash_file(&blob_path(dir.path(), &sha256)).unwrap()
        );
    }

    #[test]
    fn out_of_order_and_unknown_streams_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(chunk(dir.path(), "up-2", 1, 2, b"x").is_err());
        chunk(dir.path(), "up-2", 0, 3, b"x").unwrap();
        assert!(chunk(dir.path(), "up-2", 2, 3, b"y").is_err());
        assert!(chunk(dir.path(), "up-2", 0, 0, b"y").is_err());
        assert!(chunk(
            dir.path(),
            "up-2",
            0,
            1,
            &vec![0u8; MAX_UPLOAD_CHUNK_BYTES + 1]
        )
        .is_err());
    }

    /// Two uploaders may pick the same id; the part file is keyed by the whole
    /// tuple, so neither can corrupt the other's transfer.
    #[test]
    fn same_upload_id_from_two_users_stays_separate() {
        let dir = tempfile::tempdir().unwrap();
        accept_chunk("org", "lab-a", "u1", "p1", dir.path(), "same", 0, 2, b"AA").unwrap();
        accept_chunk("org", "lab-a", "u2", "p1", dir.path(), "same", 0, 2, b"BB").unwrap();
        let ChunkOutcome::Finalized { sha256: a, .. } =
            accept_chunk("org", "lab-a", "u1", "p1", dir.path(), "same", 1, 2, b"11").unwrap()
        else {
            panic!("finalize");
        };
        let ChunkOutcome::Finalized { sha256: b, .. } =
            accept_chunk("org", "lab-a", "u2", "p1", dir.path(), "same", 1, 2, b"22").unwrap()
        else {
            panic!("finalize");
        };
        assert_eq!(std::fs::read(blob_path(dir.path(), &a)).unwrap(), b"AA11");
        assert_eq!(std::fs::read(blob_path(dir.path(), &b)).unwrap(), b"BB22");
    }

    #[test]
    fn paths_and_kinds_are_validated() {
        assert_eq!(
            validate_path("/notebooks/a.ipynb").unwrap(),
            "notebooks/a.ipynb"
        );
        assert!(validate_path("../escape").is_err());
        assert!(validate_path("a//b").is_err());
        assert!(validate_path("a\\b").is_err());
        assert!(validate_path("   ").is_err());
        assert!(validate_kind("qasm").is_ok());
        assert!(validate_kind("exe").is_err());
    }
}
