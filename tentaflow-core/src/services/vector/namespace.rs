// ============ File: services/vector/namespace.rs — per-addon namespace registry (F1c P3) ============
//
// Owns a process-wide `(addon_id, namespace) -> Arc<UsearchBackend>` cache
// keyed by `dashmap` for lock-free reads on hot lookup paths. Each open
// namespace corresponds to a row in `addon_vector_namespaces` (DB v27) and
// a `.usearch` file under `<HOME>/.tentaflow/addons/<addon_id>/vectors/`.
//
// Quotas (F1c hard-coded, F2 makes them configurable):
//   * 10 namespaces per addon
//   * 1_000_000 vectors total per addon (summed across all its namespaces)
//
// Cross-addon isolation: lookup is always by (addon_id, namespace) — there is
// no API surface that lets addon A reach addon B's namespace. A namespace
// name collision ("faces" used by two different addons) maps to two different
// files in two different `<addon_id>/` directories.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;

use super::backend::{Metric, VectorBackend};
use super::error::{Result, VectorError};
use super::usearch_backend::UsearchBackend;
use crate::db::DbPool;

/// Hard cap on namespaces per addon. Each open namespace holds a usearch
/// handle (mmap + connectivity graph), so we keep this modest in F1c.
pub const MAX_NAMESPACES_PER_ADDON: u32 = 10;

/// Hard cap on total vectors per addon (summed across all namespaces).
/// HNSW memory scales ~linearly with vector count; at 1 M × 512 dim × f32
/// the raw vector tape alone is ~2 GiB which is the budget ceiling we are
/// willing to hand a single addon in F1c.
pub const MAX_VECTORS_PER_ADDON: u64 = 1_000_000;

/// Validates a namespace name. Names appear in file paths (so we must reject
/// `..`, `/`, control chars) and in DB primary keys; the allowed shape is
/// `[a-z0-9_-]{1,64}` which keeps the same charset as alias / camera ids.
pub fn validate_namespace_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(VectorError::InvalidNamespaceName(name.to_string()));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(VectorError::InvalidNamespaceName(name.to_string()));
    }
    Ok(())
}

/// Validates an addon id used as a path component. Same charset as namespace,
/// but allows uppercase to match the existing `addon_id` style used elsewhere
/// (e.g. `Tentaflow.Vision.Adr`). 128-char cap.
pub fn validate_addon_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    Ok(())
}

/// Returns `<HOME>/.tentaflow/addons/<addon_id>/vectors/<namespace>.usearch`.
fn namespace_file_path(addon_id: &str, namespace: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| VectorError::Io {
        path: None,
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"),
    })?;
    Ok(home
        .join(".tentaflow")
        .join("addons")
        .join(addon_id)
        .join("vectors")
        .join(format!("{namespace}.usearch")))
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct NamespaceKey {
    addon_id: String,
    namespace: String,
}

pub struct NamespaceManager {
    pool: DbPool,
    backends: DashMap<NamespaceKey, Arc<dyn VectorBackend>>,
    /// Override for the on-disk root — production uses `dirs::home_dir()`;
    /// tests inject a tempdir to avoid polluting `~`.
    root_override: Option<PathBuf>,
}

impl NamespaceManager {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            backends: DashMap::new(),
            root_override: None,
        }
    }

    /// Constructor that pins the on-disk root under `root` instead of
    /// `~/.tentaflow`. Used by integration tests + future CLI workflows
    /// that need to operate on a sandboxed vectors tree.
    pub fn with_root(pool: DbPool, root: PathBuf) -> Self {
        Self {
            pool,
            backends: DashMap::new(),
            root_override: Some(root),
        }
    }

    fn file_path_for(&self, addon_id: &str, namespace: &str) -> Result<PathBuf> {
        if let Some(root) = &self.root_override {
            Ok(root
                .join(addon_id)
                .join("vectors")
                .join(format!("{namespace}.usearch")))
        } else {
            namespace_file_path(addon_id, namespace)
        }
    }

    /// Returns the namespace handle, opening (or creating) the backing index
    /// on first access. If a DB row for `(addon_id, namespace)` exists, its
    /// dim/metric must match the caller-supplied values — mismatch is an
    /// `Err` (an addon is not allowed to silently reshape an existing index).
    pub fn get_or_create(
        &self,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
    ) -> Result<Arc<dyn VectorBackend>> {
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }

        let key = NamespaceKey {
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };

        if let Some(be) = self.backends.get(&key) {
            // Already open — verify the caller's geometry matches the live
            // index. This catches the "manifest says 512 but addon passes
            // 768" coding error before we touch usearch.
            let be = be.clone();
            if be.dim() != dim {
                return Err(VectorError::DimMismatch {
                    expected: be.dim(),
                    actual: dim,
                });
            }
            if be.metric() != metric {
                return Err(VectorError::MetricMismatch {
                    expected: be.metric().as_str(),
                    actual: metric.as_str().to_string(),
                });
            }
            return Ok(be);
        }

        // Not in cache — consult DB. If the row exists, reopen using the
        // stored geometry (and reject mismatching caller input). If it does
        // not exist, check the per-addon namespace quota, then create both
        // the DB row and the on-disk file.
        let existing = self.load_row(addon_id, namespace)?;
        let (resolved_dim, resolved_metric, file_path) = match existing {
            Some((existing_dim, existing_metric, existing_path)) => {
                if existing_dim != dim {
                    return Err(VectorError::DimMismatch {
                        expected: existing_dim,
                        actual: dim,
                    });
                }
                if existing_metric != metric {
                    return Err(VectorError::MetricMismatch {
                        expected: existing_metric.as_str(),
                        actual: metric.as_str().to_string(),
                    });
                }
                (existing_dim, existing_metric, existing_path)
            }
            None => {
                self.check_namespace_quota(addon_id)?;
                let path = self.file_path_for(addon_id, namespace)?;
                self.insert_row(addon_id, namespace, dim, metric, &path)?;
                (dim, metric, path)
            }
        };

        let backend: Arc<dyn VectorBackend> = Arc::new(UsearchBackend::open_or_create(
            file_path,
            resolved_dim,
            resolved_metric,
        )?);

        // Race: two concurrent get_or_create() calls for the same key may
        // both reach this point. dashmap entry() resolves the race — first
        // writer wins, the second drops its freshly built backend and
        // returns the established one.
        let entry = self.backends.entry(key).or_insert(backend);
        Ok(entry.value().clone())
    }

    /// Lookup without creation — used by `vector_search_v1` / `vector_delete_v1`.
    pub fn get(&self, addon_id: &str, namespace: &str) -> Result<Arc<dyn VectorBackend>> {
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };
        if let Some(be) = self.backends.get(&key) {
            return Ok(be.clone());
        }
        // Cache miss — try to reopen from DB row.
        let row = self.load_row(addon_id, namespace)?;
        let Some((dim, metric, file_path)) = row else {
            return Err(VectorError::NamespaceNotFound {
                addon_id: addon_id.to_string(),
                namespace: namespace.to_string(),
            });
        };
        let backend: Arc<dyn VectorBackend> =
            Arc::new(UsearchBackend::open_or_create(file_path, dim, metric)?);
        let entry = self.backends.entry(key).or_insert(backend);
        Ok(entry.value().clone())
    }

    /// Wraps the per-call enforcement: verifies adding one more vector would
    /// not break `MAX_VECTORS_PER_ADDON`. Called from the upsert host fn
    /// before delegating to the backend. We sum `addon_vector_namespaces.count`
    /// instead of polling every live backend to keep the check O(1) over
    /// namespaces — the slight staleness vs the live `count()` is acceptable
    /// for a soft quota.
    pub fn check_vector_quota(&self, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM addon_vector_namespaces WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .map_err(|e| VectorError::Db(e.to_string()))?;
        if total as u64 >= MAX_VECTORS_PER_ADDON {
            return Err(VectorError::VectorQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: total as u64,
                max: MAX_VECTORS_PER_ADDON,
            });
        }
        Ok(())
    }

    fn check_namespace_quota(&self, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_vector_namespaces WHERE addon_id = ?1",
                rusqlite::params![addon_id],
                |r| r.get(0),
            )
            .map_err(|e| VectorError::Db(e.to_string()))?;
        if count as u32 >= MAX_NAMESPACES_PER_ADDON {
            return Err(VectorError::NamespaceQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: count as u32,
                max: MAX_NAMESPACES_PER_ADDON,
            });
        }
        Ok(())
    }

    fn load_row(
        &self,
        addon_id: &str,
        namespace: &str,
    ) -> Result<Option<(u32, Metric, PathBuf)>> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let row = conn
            .query_row(
                "SELECT dim, metric, file_path FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND namespace = ?2",
                rusqlite::params![addon_id, namespace],
                |r| {
                    let dim: i64 = r.get(0)?;
                    let metric: String = r.get(1)?;
                    let path: String = r.get(2)?;
                    Ok((dim as u32, metric, PathBuf::from(path)))
                },
            )
            .ok();
        let Some((dim, metric_str, path)) = row else {
            return Ok(None);
        };
        let metric = Metric::parse(&metric_str).ok_or_else(|| {
            VectorError::Db(format!("invalid metric '{metric_str}' in DB row"))
        })?;
        Ok(Some((dim, metric, path)))
    }

    fn insert_row(
        &self,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
        file_path: &PathBuf,
    ) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "INSERT INTO addon_vector_namespaces \
             (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
            rusqlite::params![
                addon_id,
                namespace,
                dim as i64,
                metric.as_str(),
                file_path.to_string_lossy().to_string(),
                now,
            ],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Refreshes the cached `count` column after an upsert/delete. Done as a
    /// separate UPDATE to keep the per-write critical path short (the heavy
    /// usearch save() already happened by the time we get here).
    pub fn update_count(&self, addon_id: &str, namespace: &str, new_count: u64) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4",
            rusqlite::params![new_count as i64, now, addon_id, namespace],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Admin op — drops both the DB row and the on-disk file. Not exposed to
    /// addons (no host function); reached from the CLI in a later phase.
    /// Idempotent: missing row / missing file are both treated as success so
    /// the operation can be retried after a partial failure.
    pub fn delete_namespace(&self, addon_id: &str, namespace: &str) -> Result<()> {
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };
        self.backends.remove(&key);

        let path = {
            let conn = self
                .pool
                .lock()
                .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
            let path: Option<String> = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces \
                     WHERE addon_id = ?1 AND namespace = ?2",
                    rusqlite::params![addon_id, namespace],
                    |r| r.get(0),
                )
                .ok();
            conn.execute(
                "DELETE FROM addon_vector_namespaces WHERE addon_id = ?1 AND namespace = ?2",
                rusqlite::params![addon_id, namespace],
            )
            .map_err(|e| VectorError::Db(e.to_string()))?;
            path.map(PathBuf::from)
        };

        if let Some(p) = path {
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| VectorError::Io {
                    path: Some(p),
                    source: e,
                })?;
            }
        }
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn in_memory_db_with_v27() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn mgr() -> (TempDir, NamespaceManager) {
        let dir = TempDir::new().unwrap();
        let pool = in_memory_db_with_v27();
        let mgr = NamespaceManager::with_root(pool, dir.path().to_path_buf());
        (dir, mgr)
    }

    #[test]
    fn test_get_or_create_first_call_creates_row() {
        let (_dir, mgr) = mgr();
        let be = mgr
            .get_or_create("addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        assert_eq!(be.count(), 0);
        be.upsert(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(be.count(), 1);
    }

    #[test]
    fn test_get_or_create_idempotent() {
        let (_dir, mgr) = mgr();
        let a = mgr
            .get_or_create("addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        let b = mgr
            .get_or_create("addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        // Same Arc instance: dashmap cache hit.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_dim_mismatch_on_reopen_rejected() {
        let (_dir, mgr) = mgr();
        mgr.get_or_create("addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        let res = mgr.get_or_create("addon_a", "faces", 8, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::DimMismatch { .. })));
    }

    #[test]
    fn test_quota_exceeded_at_max_namespaces() {
        let (_dir, mgr) = mgr();
        for i in 0..MAX_NAMESPACES_PER_ADDON {
            mgr.get_or_create("addon_a", &format!("ns{i}"), 4, Metric::Cosine)
                .unwrap();
        }
        let res = mgr.get_or_create("addon_a", "overflow", 4, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::NamespaceQuotaExceeded { .. })));
    }

    #[test]
    fn test_delete_namespace_removes_file_and_db_row() {
        let (_dir, mgr) = mgr();
        let be = mgr
            .get_or_create("addon_a", "faces", 3, Metric::Cosine)
            .unwrap();
        be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        be.save().unwrap();
        // Sanity: file exists.
        let file_path = {
            let conn = mgr.pool.lock().unwrap();
            let p: String = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces WHERE addon_id='addon_a' AND namespace='faces'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            PathBuf::from(p)
        };
        assert!(file_path.exists());

        mgr.delete_namespace("addon_a", "faces").unwrap();
        assert!(!file_path.exists());
        // DB row is gone.
        let row: Option<i64> = {
            let conn = mgr.pool.lock().unwrap();
            conn.query_row(
                "SELECT 1 FROM addon_vector_namespaces WHERE addon_id='addon_a' AND namespace='faces'",
                [],
                |r| r.get(0),
            )
            .ok()
        };
        assert!(row.is_none());
    }

    #[test]
    fn test_cross_addon_namespace_isolation() {
        let (_dir, mgr) = mgr();
        let a = mgr
            .get_or_create("addon_a", "faces", 3, Metric::Cosine)
            .unwrap();
        let b = mgr
            .get_or_create("addon_b", "faces", 3, Metric::Cosine)
            .unwrap();
        a.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(a.count(), 1);
        // addon_b sees its own (empty) namespace, not addon_a's.
        assert_eq!(b.count(), 0);
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_get_missing_namespace_returns_not_found() {
        let (_dir, mgr) = mgr();
        let res = mgr.get("addon_x", "missing");
        assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
    }

    #[test]
    fn test_invalid_namespace_name_rejected() {
        let (_dir, mgr) = mgr();
        let res = mgr.get_or_create("addon_a", "bad/name", 3, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::InvalidNamespaceName(_))));
    }
}
