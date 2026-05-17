// ============ File: services/vector/usearch_backend.rs — usearch HNSW backend (F1c P3) ============
//
// Thin wrapper over `usearch::Index` providing the `VectorBackend` trait. The
// underlying native index is already `Send + Sync` (C++ side carries its own
// locks for concurrent reads) but `add`/`remove`/`save` mutate the graph and
// we want a single writer at a time, so we wrap the handle in a parking_lot
// `RwLock`: reads (`search`, `count`) take the read guard, writers take the
// write guard.
//
// Persistence model:
//   * Reopen path uses `Index::view(path)` — usearch mmaps the file lazily so
//     a 3 GiB index does not balloon the RSS at open. Only the pages actually
//     touched by inserts / searches are paged in.
//   * Every successful upsert / delete calls `save(path)` before returning so
//     that a successful return implies durability. On Unix we follow up with
//     `chmod 0o600` to keep embeddings (PII for face vectors) from leaking to
//     other local users via the default 0644 umask.

use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;

use parking_lot::RwLock;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::backend::{Metric, SearchHit, VectorBackend};
use super::error::{Result, VectorError};

pub struct UsearchBackend {
    inner: RwLock<Index>,
    file_path: PathBuf,
    dim: u32,
    metric: Metric,
}

impl UsearchBackend {
    /// Open the on-disk index at `file_path` if it exists (mmap via
    /// `Index::view`), otherwise create a fresh empty one and persist a
    /// header file so subsequent reopens follow the view() path.
    pub fn open_or_create(file_path: PathBuf, dim: u32, metric: Metric) -> Result<Self> {
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }

        let options = IndexOptions {
            dimensions: dim as usize,
            metric: metric_to_usearch(metric),
            quantization: ScalarKind::F32,
            // 0 = pick library default — usearch uses 16 for HNSW connectivity
            // and 128/64 for expansion, which is a sane tradeoff for the
            // <=1M vector workloads F1c targets.
            connectivity: 0,
            expansion_add: 0,
            expansion_search: 0,
            multi: false,
        };
        let index =
            Index::new(&options).map_err(|e| VectorError::Backend(format!("Index::new: {e}")))?;

        if file_path.exists() {
            // view() = mmap the on-disk index, paging in only what is touched
            // by searches and inserts. Avoids loading a multi-GB graph into
            // RAM at reopen time.
            let path_str = file_path.to_string_lossy().to_string();
            index
                .view(&path_str)
                .map_err(|e| VectorError::Backend(format!("Index::view({path_str}): {e}")))?;
        } else {
            // Ensure parent dir exists with 0o700 (only owner can list the
            // per-addon vector directory). The first save() below then writes
            // the empty index header so future reopens hit the view() path.
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| VectorError::Io {
                    path: Some(parent.to_path_buf()),
                    source: e,
                })?;
                tighten_dir_mode(parent)?;
            }
            let path_str = file_path.to_string_lossy().to_string();
            index
                .save(&path_str)
                .map_err(|e| VectorError::Backend(format!("Index::save({path_str}): {e}")))?;
            tighten_file_mode(&file_path)?;
        }

        Ok(Self {
            inner: RwLock::new(index),
            file_path,
            dim,
            metric,
        })
    }

    /// Internal: save + chmod 0600. Called after every mutation so that a
    /// successful upsert/delete return implies a durable, owner-only file.
    fn persist(&self, index: &Index) -> Result<()> {
        let path_str = self.file_path.to_string_lossy().to_string();
        index
            .save(&path_str)
            .map_err(|e| VectorError::Backend(format!("save({path_str}): {e}")))?;
        tighten_file_mode(&self.file_path)?;
        Ok(())
    }

    /// Test-only helper — returns the on-disk file path of the backing index.
    #[cfg(test)]
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }
}

fn metric_to_usearch(m: Metric) -> MetricKind {
    match m {
        Metric::Cosine => MetricKind::Cos,
        Metric::Euclidean => MetricKind::L2sq,
        Metric::Dot => MetricKind::IP,
    }
}

/// Enforce mode 0600 on the index file. Embeddings of regulated data
/// (face vectors, person attributes) qualify as PII; the default umask 0022
/// would leave them at 0644 and readable by every local user.
fn tighten_file_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            VectorError::Io {
                path: Some(path.to_path_buf()),
                source: e,
            }
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Enforce mode 0700 on the per-addon vectors directory so directory listing
/// is restricted to the owning process user. Matches the pattern in
/// `services::key_storage`.
fn tighten_dir_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            VectorError::Io {
                path: Some(path.to_path_buf()),
                source: e,
            }
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

impl VectorBackend for UsearchBackend {
    fn upsert(&self, ref_id: u64, vector: &[f32]) -> Result<()> {
        if ref_id == 0 {
            return Err(VectorError::InvalidRefId);
        }
        if vector.is_empty() {
            return Err(VectorError::EmptyVector);
        }
        if vector.len() as u32 != self.dim {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: vector.len() as u32,
            });
        }

        let guard = self.inner.write();
        // usearch needs capacity reserved before add — reserve in 1024-vector
        // chunks so we are not paying realloc cost on every insert but also
        // not pre-allocating huge graphs for sparsely-used namespaces.
        let size = guard.size();
        let capacity_chunk = 1024usize;
        let needed = size + 1;
        let current_capacity = guard.capacity();
        if needed > current_capacity {
            let new_cap = needed.div_ceil(capacity_chunk) * capacity_chunk;
            guard
                .reserve(new_cap)
                .map_err(|e| VectorError::Backend(format!("reserve({new_cap}): {e}")))?;
        }

        // usearch with multi=false rejects a second add() under the same
        // key ("Duplicate keys not allowed"). To honour upsert semantics we
        // remove the existing entry first when present.
        if guard.contains(ref_id) {
            guard
                .remove(ref_id)
                .map_err(|e| VectorError::Backend(format!("remove(pre-upsert {ref_id}): {e}")))?;
        }
        guard
            .add(ref_id, vector)
            .map_err(|e| VectorError::Backend(format!("add({ref_id}): {e}")))?;
        self.persist(&guard)?;
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchHit>> {
        if query.is_empty() {
            return Err(VectorError::EmptyVector);
        }
        if query.len() as u32 != self.dim {
            return Err(VectorError::DimMismatch {
                expected: self.dim,
                actual: query.len() as u32,
            });
        }
        let guard = self.inner.read();
        let matches = guard
            .search(query, k)
            .map_err(|e| VectorError::Backend(format!("search: {e}")))?;
        let hits: Vec<SearchHit> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(k, d)| SearchHit {
                ref_id: *k,
                score: *d,
            })
            .collect();
        Ok(hits)
    }

    fn delete(&self, ref_id: u64) -> Result<bool> {
        if ref_id == 0 {
            return Err(VectorError::InvalidRefId);
        }
        let guard = self.inner.write();
        let removed = guard
            .remove(ref_id)
            .map_err(|e| VectorError::Backend(format!("remove({ref_id}): {e}")))?;
        if removed > 0 {
            // Only persist on a real removal — no point rewriting the file
            // when the key was already absent.
            self.persist(&guard)?;
        }
        Ok(removed > 0)
    }

    fn has_ref(&self, ref_id: u64) -> bool {
        if ref_id == 0 {
            return false;
        }
        self.inner.read().contains(ref_id)
    }

    fn count(&self) -> u64 {
        self.inner.read().size() as u64
    }

    fn save(&self) -> Result<()> {
        let guard = self.inner.read();
        self.persist(&guard)
    }

    fn dim(&self) -> u32 {
        self.dim
    }

    fn metric(&self) -> Metric {
        self.metric
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn tmp_backend(dim: u32, metric: Metric) -> (TempDir, Arc<UsearchBackend>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ns.usearch");
        let be = UsearchBackend::open_or_create(path, dim, metric).unwrap();
        (dir, Arc::new(be))
    }

    #[test]
    fn test_open_create_and_upsert() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        assert_eq!(be.count(), 0);
        be.upsert(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        be.upsert(2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(be.count(), 2);
    }

    #[test]
    fn test_search_returns_nearest_top_k() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        be.upsert(10, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        be.upsert(20, &[0.0, 1.0, 0.0, 0.0]).unwrap();
        be.upsert(30, &[0.0, 0.0, 1.0, 0.0]).unwrap();
        let hits = be.search(&[0.99, 0.01, 0.0, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].ref_id, 10);
    }

    #[test]
    fn test_delete_removes_vector_and_persists() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ns.usearch");
        {
            let be = UsearchBackend::open_or_create(path.clone(), 3, Metric::Cosine).unwrap();
            be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
            be.upsert(2, &[0.0, 1.0, 0.0]).unwrap();
            assert!(be.delete(1).unwrap());
            // Second delete returns false (no key).
            assert!(!be.delete(1).unwrap());
        }
        // Reopen — the delete must have been persisted before delete()
        // returned, so the count after reopen is 1, not 2.
        let be2 = UsearchBackend::open_or_create(path, 3, Metric::Cosine).unwrap();
        assert_eq!(be2.count(), 1);
    }

    #[test]
    fn test_cosine_metric_distance() {
        let (_dir, be) = tmp_backend(2, Metric::Cosine);
        be.upsert(1, &[1.0, 0.0]).unwrap();
        let hits = be.search(&[1.0, 0.0], 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score.abs() < 1e-3, "score={}", hits[0].score);
    }

    #[test]
    fn test_dim_mismatch_rejected() {
        let (_dir, be) = tmp_backend(4, Metric::Cosine);
        let err = be.upsert(1, &[1.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::DimMismatch { .. }));
    }

    #[test]
    fn test_invalid_ref_id_rejected() {
        let (_dir, be) = tmp_backend(2, Metric::Cosine);
        assert!(matches!(
            be.upsert(0, &[1.0, 0.0]).unwrap_err(),
            VectorError::InvalidRefId
        ));
    }

    #[test]
    fn test_persist_and_reopen_uses_view() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("persist.usearch");
        {
            let be = UsearchBackend::open_or_create(path.clone(), 3, Metric::Cosine).unwrap();
            be.upsert(7, &[1.0, 0.0, 0.0]).unwrap();
            be.upsert(8, &[0.0, 1.0, 0.0]).unwrap();
            // No explicit save() — upsert persists internally.
        }
        // Reopen — open_or_create takes the view() branch (file exists);
        // data must be visible and search must still work.
        let be2 = UsearchBackend::open_or_create(path, 3, Metric::Cosine).unwrap();
        assert_eq!(be2.count(), 2);
        let hits = be2.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].ref_id, 7);
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let (_dir, be) = tmp_backend(3, Metric::Cosine);
        be.upsert(42, &[1.0, 0.0, 0.0]).unwrap();
        be.upsert(42, &[0.0, 1.0, 0.0]).unwrap();
        assert_eq!(be.count(), 1);
        let hits = be.search(&[0.0, 1.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].ref_id, 42);
    }

    #[test]
    fn test_has_ref_reflects_membership() {
        let (_dir, be) = tmp_backend(2, Metric::Cosine);
        assert!(!be.has_ref(1));
        be.upsert(1, &[1.0, 0.0]).unwrap();
        assert!(be.has_ref(1));
        assert!(!be.has_ref(2));
        // ref_id 0 is invalid by contract, never present.
        assert!(!be.has_ref(0));
    }

    #[cfg(unix)]
    #[test]
    fn test_save_enforces_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, be) = tmp_backend(3, Metric::Cosine);
        be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        let meta = std::fs::metadata(be.file_path()).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn test_vector_dir_mode_0700_after_create() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        // Nest one level deeper so open_or_create has to mkdir the parent.
        let nested = dir.path().join("sub").join("ns.usearch");
        let _be = UsearchBackend::open_or_create(nested.clone(), 3, Metric::Cosine).unwrap();
        let parent = nested.parent().unwrap();
        let meta = std::fs::metadata(parent).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected 0700, got {:o}", mode);
    }
}
