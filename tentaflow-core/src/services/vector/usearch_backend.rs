// ============ File: services/vector/usearch_backend.rs — usearch HNSW backend (F1c P3) ============
//
// Thin wrapper over `usearch::Index` providing the `VectorBackend` trait. The
// underlying native index is already `Send + Sync` (C++ side carries its own
// locks for concurrent reads) but `add`/`remove`/`save` mutate the graph and
// we want a single writer at a time, so we wrap the handle in a parking_lot
// `RwLock`: reads (`search`, `count`) take the read guard, writers take the
// write guard. usearch's `save(path)` writes the full mmap-backed index to
// disk; we call it after every successful mutation for crash durability.

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
    /// Open the on-disk index at `file_path` if it exists, otherwise create a
    /// fresh empty one with the supplied geometry and persist a 0-vector file
    /// so subsequent reopens follow the load path.
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

        // If a file exists, load its contents into the freshly built index.
        // usearch's load() needs the geometry to already match — that is why
        // we pass `dim`/`metric` from the manager (DB-resolved) and not from
        // the file header.
        if file_path.exists() {
            let path_str = file_path.to_string_lossy().to_string();
            index
                .load(&path_str)
                .map_err(|e| VectorError::Backend(format!("Index::load({path_str}): {e}")))?;
        } else {
            // Ensure the parent dir exists; the first save() below will then
            // write the empty index header so future reopens hit the load
            // path instead of repeatedly walking the create branch.
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| VectorError::Io {
                    path: Some(parent.to_path_buf()),
                    source: e,
                })?;
            }
            let path_str = file_path.to_string_lossy().to_string();
            index
                .save(&path_str)
                .map_err(|e| VectorError::Backend(format!("Index::save({path_str}): {e}")))?;
        }

        Ok(Self {
            inner: RwLock::new(index),
            file_path,
            dim,
            metric,
        })
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
            let new_cap = ((needed + capacity_chunk - 1) / capacity_chunk) * capacity_chunk;
            guard
                .reserve(new_cap)
                .map_err(|e| VectorError::Backend(format!("reserve({new_cap}): {e}")))?;
        }

        // usearch with multi=false rejects a second add() under the same
        // key ("Duplicate keys not allowed"). To honour upsert semantics we
        // remove the existing entry first when present. contains() is O(1)
        // on the high-level wrapper.
        if guard.contains(ref_id) {
            guard
                .remove(ref_id)
                .map_err(|e| VectorError::Backend(format!("remove(pre-upsert {ref_id}): {e}")))?;
        }
        guard
            .add(ref_id, vector)
            .map_err(|e| VectorError::Backend(format!("add({ref_id}): {e}")))?;
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
        Ok(removed > 0)
    }

    fn count(&self) -> u64 {
        self.inner.read().size() as u64
    }

    fn save(&self) -> Result<()> {
        let guard = self.inner.read();
        let path_str = self.file_path.to_string_lossy().to_string();
        guard
            .save(&path_str)
            .map_err(|e| VectorError::Backend(format!("save({path_str}): {e}")))?;
        Ok(())
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
        // Closest to [1,0,0,0] is key 10.
        assert_eq!(hits[0].ref_id, 10);
    }

    #[test]
    fn test_delete_removes_vector() {
        let (_dir, be) = tmp_backend(3, Metric::Cosine);
        be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        be.upsert(2, &[0.0, 1.0, 0.0]).unwrap();
        assert_eq!(be.count(), 2);
        assert!(be.delete(1).unwrap());
        assert_eq!(be.count(), 1);
        // Second delete returns false (no key).
        assert!(!be.delete(1).unwrap());
    }

    #[test]
    fn test_cosine_metric_distance() {
        // Identical vector has near-zero cosine distance.
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
    fn test_persist_and_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("persist.usearch");
        {
            let be = UsearchBackend::open_or_create(path.clone(), 3, Metric::Cosine).unwrap();
            be.upsert(7, &[1.0, 0.0, 0.0]).unwrap();
            be.upsert(8, &[0.0, 1.0, 0.0]).unwrap();
            be.save().unwrap();
        }
        // Reopen — data must survive.
        let be2 = UsearchBackend::open_or_create(path, 3, Metric::Cosine).unwrap();
        assert_eq!(be2.count(), 2);
        let hits = be2.search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].ref_id, 7);
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let (_dir, be) = tmp_backend(3, Metric::Cosine);
        be.upsert(42, &[1.0, 0.0, 0.0]).unwrap();
        // multi=false → second add with same key overwrites in place.
        be.upsert(42, &[0.0, 1.0, 0.0]).unwrap();
        assert_eq!(be.count(), 1);
        let hits = be.search(&[0.0, 1.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].ref_id, 42);
    }
}
