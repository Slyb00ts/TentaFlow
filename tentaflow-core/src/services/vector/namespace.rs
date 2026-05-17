// ============ File: services/vector/namespace.rs — per-(org, addon) namespace registry (F1c P3 + F2 P1.c) ============
//
// Owns a process-wide `(org_id, addon_id, namespace) -> Arc<UsearchBackend>`
// cache keyed by `dashmap` for lock-free reads on hot lookup paths. Each open
// namespace corresponds to a row in `addon_vector_namespaces` (DB v27,
// org_id column added in v32) and a `.usearch` file under
// `<HOME>/.tentaflow/orgs/<org_id>/addons/<addon_id>/vectors/`.
//
// Quotas (F1c hard-coded, F2 makes them configurable):
//   * 10 namespaces per (org, addon)
//   * 1_000_000 vectors total per (org, addon) (summed across all namespaces)
//
// Cross-tenant isolation: lookup is always by (org_id, addon_id, namespace).
// There is no API surface that lets addon A in org X reach the same addon
// id's namespace in org Y; the org_id filters land on every SELECT / INSERT
// / UPDATE / DELETE and on every file-path resolution.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;

use super::backend::{Metric, VectorBackend};
use super::error::{Result, VectorError};
use super::usearch_backend::UsearchBackend;
use crate::db::DbPool;

/// Hard cap on namespaces per (org, addon). Each open namespace holds a
/// usearch handle (mmap + connectivity graph), so we keep this modest in F1c.
pub const MAX_NAMESPACES_PER_ADDON: u32 = 10;

/// Hard cap on total vectors per (org, addon) (summed across all namespaces).
/// HNSW memory scales ~linearly with vector count; at 1 M × 512 dim × f32
/// the raw vector tape alone is ~2 GiB which is the budget ceiling we are
/// willing to hand a single addon install in F1c.
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

/// Validates the org id used as a path component. Org ids are UUIDv4 in
/// production but the default seed (`org-default`) and tempdir-based tests
/// use hyphenated lowercase strings; accept both as long as the charset
/// stays path-safe.
fn validate_org_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(VectorError::InvalidNamespaceName(id.to_string()));
    }
    Ok(())
}

/// Returns `<HOME>/.tentaflow/orgs/<org_id>/addons/<addon_id>/vectors/<namespace>.usearch`.
/// The org segment ensures the same addon installed in two tenants writes to
/// physically separate directories.
fn namespace_file_path(org_id: &str, addon_id: &str, namespace: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| VectorError::Io {
        path: None,
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"),
    })?;
    Ok(home
        .join(".tentaflow")
        .join("orgs")
        .join(org_id)
        .join("addons")
        .join(addon_id)
        .join("vectors")
        .join(format!("{namespace}.usearch")))
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct NamespaceKey {
    org_id: String,
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

    fn file_path_for(&self, org_id: &str, addon_id: &str, namespace: &str) -> Result<PathBuf> {
        if let Some(root) = &self.root_override {
            Ok(root
                .join(org_id)
                .join(addon_id)
                .join("vectors")
                .join(format!("{namespace}.usearch")))
        } else {
            namespace_file_path(org_id, addon_id, namespace)
        }
    }

    /// Returns the namespace handle, opening (or creating) the backing index
    /// on first access. If a DB row for `(org_id, addon_id, namespace)`
    /// exists, its dim/metric must match the caller-supplied values —
    /// mismatch is an `Err` (an addon is not allowed to silently reshape an
    /// existing index).
    pub fn get_or_create(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        dim: u32,
        metric: Metric,
    ) -> Result<Arc<dyn VectorBackend>> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        if !(1..=4096).contains(&dim) {
            return Err(VectorError::InvalidDim(dim));
        }

        let key = NamespaceKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };

        if let Some(be) = self.backends.get(&key) {
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

        let existing = self.load_row(org_id, addon_id, namespace)?;
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
                self.check_namespace_quota(org_id, addon_id)?;
                let path = self.file_path_for(org_id, addon_id, namespace)?;
                self.insert_row(org_id, addon_id, namespace, dim, metric, &path)?;
                (dim, metric, path)
            }
        };

        let backend: Arc<dyn VectorBackend> = Arc::new(UsearchBackend::open_or_create(
            file_path,
            resolved_dim,
            resolved_metric,
        )?);

        let entry = self.backends.entry(key).or_insert(backend);
        Ok(entry.value().clone())
    }

    /// Lookup without creation — used by `vector_search_v1` / `vector_delete_v1`.
    pub fn get(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
    ) -> Result<Arc<dyn VectorBackend>> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            org_id: org_id.to_string(),
            addon_id: addon_id.to_string(),
            namespace: namespace.to_string(),
        };
        if let Some(be) = self.backends.get(&key) {
            return Ok(be.clone());
        }
        let row = self.load_row(org_id, addon_id, namespace)?;
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

    /// Transactional upsert scoped to `(org_id, addon_id)`. Checks the
    /// per-tenant quota, runs the backend upsert (which persists internally),
    /// and bumps the cached `count` row — all under a single `IMMEDIATE`
    /// SQLite transaction so two concurrent upserts at the cap cannot both
    /// succeed.
    ///
    /// Returns the new count for the namespace.
    pub fn upsert_with_quota(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        ref_id: u64,
        vector: &[f32],
        dim: u32,
        metric: Metric,
    ) -> Result<u64> {
        let backend = self.get_or_create(org_id, addon_id, namespace, dim, metric)?;
        let is_replace = backend.has_ref(ref_id);

        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;

        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND org_id = ?2",
                rusqlite::params![addon_id, org_id],
                |r| r.get(0),
            )
            .map_err(|e| {
                let _ = conn.execute("ROLLBACK", []);
                VectorError::Db(e.to_string())
            })?;

        if !is_replace && (total as u64) >= MAX_VECTORS_PER_ADDON {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::VectorQuotaExceeded {
                addon_id: addon_id.to_string(),
                current: total as u64,
                max: MAX_VECTORS_PER_ADDON,
            });
        }

        if let Err(e) = backend.upsert(ref_id, vector) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(e);
        }

        let new_count = backend.count();
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if let Err(e) = conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![new_count as i64, now, addon_id, namespace, org_id],
        ) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(VectorError::Db(e.to_string()));
        }
        conn.execute("COMMIT", [])
            .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(new_count)
    }

    fn check_namespace_quota(&self, org_id: &str, addon_id: &str) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND org_id = ?2",
                rusqlite::params![addon_id, org_id],
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
        org_id: &str,
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
                 WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                rusqlite::params![addon_id, namespace, org_id],
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
        org_id: &str,
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
             (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at, org_id) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6, ?7)",
            rusqlite::params![
                addon_id,
                namespace,
                dim as i64,
                metric.as_str(),
                file_path.to_string_lossy().to_string(),
                now,
                org_id,
            ],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Refreshes the cached `count` column after an upsert/delete. Done as a
    /// separate UPDATE to keep the per-write critical path short (the heavy
    /// usearch save() already happened by the time we get here).
    pub fn update_count(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
        new_count: u64,
    ) -> Result<()> {
        let conn = self
            .pool
            .lock()
            .map_err(|_| VectorError::Db("pool mutex poisoned".into()))?;
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1, updated_at = ?2 \
             WHERE addon_id = ?3 AND namespace = ?4 AND org_id = ?5",
            rusqlite::params![new_count as i64, now, addon_id, namespace, org_id],
        )
        .map_err(|e| VectorError::Db(e.to_string()))?;
        Ok(())
    }

    /// Admin op — drops both the DB row and the on-disk file. Not exposed to
    /// addons (no host function); reached from the CLI in a later phase.
    /// Idempotent: missing row / missing file are both treated as success so
    /// the operation can be retried after a partial failure.
    pub fn delete_namespace(
        &self,
        org_id: &str,
        addon_id: &str,
        namespace: &str,
    ) -> Result<()> {
        validate_org_id(org_id)?;
        validate_addon_id(addon_id)?;
        validate_namespace_name(namespace)?;
        let key = NamespaceKey {
            org_id: org_id.to_string(),
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
                     WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                    rusqlite::params![addon_id, namespace, org_id],
                    |r| r.get(0),
                )
                .ok();
            conn.execute(
                "DELETE FROM addon_vector_namespaces \
                 WHERE addon_id = ?1 AND namespace = ?2 AND org_id = ?3",
                rusqlite::params![addon_id, namespace, org_id],
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

    const ORG_A: &str = "org-a";
    const ORG_B: &str = "org-b";

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
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        assert_eq!(be.count(), 0);
        be.upsert(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(be.count(), 1);
    }

    #[test]
    fn test_get_or_create_idempotent() {
        let (_dir, mgr) = mgr();
        let a = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        let b = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_dim_mismatch_on_reopen_rejected() {
        let (_dir, mgr) = mgr();
        mgr.get_or_create(ORG_A, "addon_a", "faces", 4, Metric::Cosine)
            .unwrap();
        let res = mgr.get_or_create(ORG_A, "addon_a", "faces", 8, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::DimMismatch { .. })));
    }

    #[test]
    fn test_quota_exceeded_at_max_namespaces() {
        let (_dir, mgr) = mgr();
        for i in 0..MAX_NAMESPACES_PER_ADDON {
            mgr.get_or_create(ORG_A, "addon_a", &format!("ns{i}"), 4, Metric::Cosine)
                .unwrap();
        }
        let res = mgr.get_or_create(ORG_A, "addon_a", "overflow", 4, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::NamespaceQuotaExceeded { .. })));
    }

    #[test]
    fn test_delete_namespace_removes_file_and_db_row() {
        let (_dir, mgr) = mgr();
        let be = mgr
            .get_or_create(ORG_A, "addon_a", "faces", 3, Metric::Cosine)
            .unwrap();
        be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        be.save().unwrap();
        let file_path = {
            let conn = mgr.pool.lock().unwrap();
            let p: String = conn
                .query_row(
                    "SELECT file_path FROM addon_vector_namespaces \
                     WHERE addon_id='addon_a' AND namespace='faces' AND org_id='org-a'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            PathBuf::from(p)
        };
        assert!(file_path.exists());

        mgr.delete_namespace(ORG_A, "addon_a", "faces").unwrap();
        assert!(!file_path.exists());
        let row: Option<i64> = {
            let conn = mgr.pool.lock().unwrap();
            conn.query_row(
                "SELECT 1 FROM addon_vector_namespaces \
                 WHERE addon_id='addon_a' AND namespace='faces' AND org_id='org-a'",
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
            .get_or_create(ORG_A, "addon_a", "faces", 3, Metric::Cosine)
            .unwrap();
        let b = mgr
            .get_or_create(ORG_A, "addon_b", "faces", 3, Metric::Cosine)
            .unwrap();
        a.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 0);
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_cross_org_get_returns_not_found() {
        // F2 P1.c — same addon_id + namespace lookup under a different org
        // must NOT see the other tenant's namespace. The SQL WHERE clause is
        // `addon_id = ? AND namespace = ? AND org_id = ?`; the migration v32
        // PK `(addon_id, namespace)` still applies, so the production
        // assumption is that no two orgs share the same literal `addon_id`
        // (per-org install paths in P1.b enforce this — `addon_data_dir`
        // composes org_id into the path). The test below exercises the SQL
        // org_id filter directly: even with the row physically present in
        // org A, a lookup under org B sees nothing.
        let (_dir, mgr) = mgr();
        mgr.get_or_create(ORG_A, "addon_x_query", "faces", 3, Metric::Cosine)
            .unwrap();
        let res = mgr.get(ORG_B, "addon_x_query", "faces");
        assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
    }

    #[test]
    fn test_get_missing_namespace_returns_not_found() {
        let (_dir, mgr) = mgr();
        let res = mgr.get(ORG_A, "addon_x", "missing");
        assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
    }

    #[test]
    fn test_invalid_namespace_name_rejected() {
        let (_dir, mgr) = mgr();
        let res = mgr.get_or_create(ORG_A, "addon_a", "bad/name", 3, Metric::Cosine);
        assert!(matches!(res, Err(VectorError::InvalidNamespaceName(_))));
    }

    #[test]
    fn test_upsert_with_quota_replace_does_not_increment_count() {
        let (_dir, mgr) = mgr();
        let c1 = mgr
            .upsert_with_quota(ORG_A, "addon_a", "ns1", 1, &[1.0, 0.0, 0.0], 3, Metric::Cosine)
            .unwrap();
        assert_eq!(c1, 1);
        let c2 = mgr
            .upsert_with_quota(ORG_A, "addon_a", "ns1", 1, &[0.0, 1.0, 0.0], 3, Metric::Cosine)
            .unwrap();
        assert_eq!(c2, 1);
    }

    #[test]
    fn test_upsert_with_quota_blocks_new_insert_at_cap() {
        let (_dir, mgr) = mgr();
        mgr.upsert_with_quota(ORG_A, "addon_a", "ns1", 1, &[1.0, 0.0, 0.0], 3, Metric::Cosine)
            .unwrap();
        {
            let conn = mgr.pool.lock().unwrap();
            conn.execute(
                "UPDATE addon_vector_namespaces SET count = ?1 \
                 WHERE addon_id = 'addon_a' AND org_id = 'org-a'",
                rusqlite::params![MAX_VECTORS_PER_ADDON as i64],
            )
            .unwrap();
        }
        let err = mgr
            .upsert_with_quota(ORG_A, "addon_a", "ns1", 999, &[0.0, 0.0, 1.0], 3, Metric::Cosine)
            .unwrap_err();
        assert!(matches!(err, VectorError::VectorQuotaExceeded { .. }));
    }
}
