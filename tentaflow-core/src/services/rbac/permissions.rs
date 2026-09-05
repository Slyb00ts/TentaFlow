// ============ File: services/rbac/permissions.rs — cached per-(user, org) permission matrix ============
//
// Lookup pattern:
//   1. `has_permission(db, user_id, org_id, perm)` checks the in-memory cache.
//   2. On miss, `load_for_user_org` queries `services::org::repo::get_user_role_in_org`
//      to materialize the role's permission list.
//   3. The set is stored under `(user_id, org_id)` and reused for subsequent
//      reads. Membership / role mutations call `invalidate(user_id, org_id)`
//      directly from `services::org::repo` so the cache cannot serve a stale
//      decision after a role change.
//
// The cache is process-global (`PermissionMatrix::global()`) so dispatch and
// host-fn paths share the same view. There is no TTL — invalidation is
// explicit. `invalidate_all()` is provided for the cold-start path (e.g.
// after a roles preseed change in a migration).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use thiserror::Error;

use crate::db::DbPool;
use crate::services::org::repo;

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error("permission '{0}' not granted for user '{1}' in org '{2}'")]
    NotGranted(String, String, String),

    #[error("user '{0}' has no membership in org '{1}'")]
    NoMembership(String, String),

    #[error("rbac db error: {0}")]
    Db(String),
}

/// Outcome of a `require` check. `Allow` is the success case; `Deny` wraps
/// the structured reason so the caller can surface it in an audit row or HTTP
/// response without re-deriving the failure mode.
#[derive(Debug)]
pub enum PermissionDecision {
    Allow,
    Deny(PermissionError),
}

pub struct PermissionMatrix {
    inner: RwLock<HashMap<(String, String), HashSet<String>>>,
    /// Monotonic counter bumped on every `invalidate*`. A reader that misses
    /// the cache snapshots the counter before the DB load and only writes the
    /// loaded set back when the counter is unchanged on completion. This
    /// closes the read-DB-write race where an invalidate happens between the
    /// miss and the write: without the guard the stale-cache reader would
    /// resurrect the invalidated entry.
    generation: AtomicU64,
}

impl PermissionMatrix {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    /// Process-wide singleton. Initialised lazily on first access so a node
    /// that never resolves a permission (e.g. pure mesh relay) never pays
    /// the allocation cost.
    pub fn global() -> &'static Arc<PermissionMatrix> {
        static GLOBAL: OnceLock<Arc<PermissionMatrix>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(PermissionMatrix::new()))
    }

    /// Returns `Ok(true)` when the user has the named permission in the org,
    /// `Ok(false)` when the membership exists but the permission is missing,
    /// `Err(NoMembership)` when the user is not a member of the org at all.
    pub fn has_permission(
        &self,
        db: &DbPool,
        user_id: &str,
        org_id: &str,
        perm: &str,
    ) -> Result<bool, PermissionError> {
        let key = (user_id.to_string(), org_id.to_string());
        if let Some(set) = self.inner.read().get(&key) {
            return Ok(set.contains(perm));
        }
        let gen_before = self.generation.load(Ordering::Acquire);
        let set = Self::load_for_user_org(db, user_id, org_id)?;
        let granted = set.contains(perm);
        let mut w = self.inner.write();
        // Only commit the freshly-loaded set if no `invalidate*` was observed
        // between the cache miss and the DB load. A racing invalidate bumps
        // `generation`; in that case we drop the stale set and let the next
        // reader re-load. Decision (granted/not) for THIS call still uses
        // the snapshot we just read — the new check is whether to cache it.
        if self.generation.load(Ordering::Acquire) == gen_before {
            w.insert(key, set);
        }
        Ok(granted)
    }

    /// Convenience wrapper that turns a boolean answer into a structured
    /// `PermissionDecision`. Callers that need to short-circuit on deny use
    /// this; raw `has_permission` is for code paths that need to branch on
    /// the boolean.
    pub fn require(
        &self,
        db: &DbPool,
        user_id: &str,
        org_id: &str,
        perm: &str,
    ) -> PermissionDecision {
        match self.has_permission(db, user_id, org_id, perm) {
            Ok(true) => PermissionDecision::Allow,
            Ok(false) => PermissionDecision::Deny(PermissionError::NotGranted(
                perm.to_string(),
                user_id.to_string(),
                org_id.to_string(),
            )),
            Err(e) => PermissionDecision::Deny(e),
        }
    }

    /// Drop the cached permission set for a single (user, org) pair. Called
    /// after `add_membership` / `remove_membership` mutates the underlying
    /// role assignment so the next read sees fresh state.
    pub fn invalidate(&self, user_id: &str, org_id: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.inner
            .write()
            .remove(&(user_id.to_string(), org_id.to_string()));
    }

    /// Flush every entry. Used by migrations that rewrite the roles preseed
    /// (a permission list change without a membership change would otherwise
    /// stay invisible until a process restart).
    pub fn invalidate_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.inner.write().clear();
        // Role preseed rewrites change which permissions a given (user, org)
        // pair holds, which can flip a gate decision. The gate-check cache
        // has no reverse index by role — flush everything.
        crate::services::policy::GateCheckCache::global().invalidate_all();
    }

    /// Number of cached entries — only useful for assertions in tests.
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.inner.read().len()
    }

    /// Snapshot of the monotonic invalidation counter — bumped by every
    /// `invalidate`/`invalidate_all` call, i.e. on every role/membership
    /// change anywhere in the process, not just for one (user, org) pair.
    /// Used by `services::bus_authorizer::RbacBusAuthorizer::generation` as
    /// (part of) the value `BusAuthorizer::generation` exposes to
    /// `bus::ConsumerHandle` — a coarser signal than the per-pair cache
    /// entry, but a false-positive re-check (this bumped for an unrelated
    /// user/org) is harmless, while a false negative (a real bus.read/
    /// write/admin change this counter misses) is exactly what would let a
    /// revoked permission keep working, which `BusAuthorizer::generation`'s
    /// own doc says must never happen.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn load_for_user_org(
        db: &DbPool,
        user_id: &str,
        org_id: &str,
    ) -> Result<HashSet<String>, PermissionError> {
        let role = repo::get_user_role_in_org(db, user_id, org_id)
            .map_err(|e| PermissionError::Db(e.to_string()))?;
        match role {
            Some(r) => Ok(r.permissions.into_iter().collect()),
            None => Err(PermissionError::NoMembership(
                user_id.to_string(),
                org_id.to_string(),
            )),
        }
    }
}

impl Default for PermissionMatrix {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::org::repo as org_repo;
    use tempfile::TempDir;

    fn open_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("rbac_perm_test.db");
        let pool = crate::db::init(&path).expect("init DB");
        (dir, pool)
    }

    fn seed_admin_membership(pool: &DbPool, user_id: &str) -> String {
        let org = org_repo::create_organization(
            pool,
            "Acme",
            &format!("acme-{user_id}"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let admin = org_repo::list_roles(pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_admin")
            .unwrap();
        org_repo::add_membership(pool, &org.org_id, user_id, &admin.role_id, "boot").unwrap();
        org.org_id
    }

    fn seed_viewer_membership(pool: &DbPool, user_id: &str) -> String {
        let org = org_repo::create_organization(
            pool,
            "Beta",
            &format!("beta-{user_id}"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let viewer = org_repo::list_roles(pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_viewer")
            .unwrap();
        org_repo::add_membership(pool, &org.org_id, user_id, &viewer.role_id, "boot").unwrap();
        org.org_id
    }

    #[test]
    fn permission_matrix_returns_true_for_granted_perm() {
        let (_d, pool) = open_pool();
        let org_id = seed_admin_membership(&pool, "u-1");
        let m = PermissionMatrix::new();
        assert!(m
            .has_permission(&pool, "u-1", &org_id, "org.admin")
            .unwrap());
        assert!(m
            .has_permission(&pool, "u-1", &org_id, "camera.write")
            .unwrap());
    }

    #[test]
    fn permission_matrix_returns_false_for_missing_perm() {
        let (_d, pool) = open_pool();
        let org_id = seed_viewer_membership(&pool, "u-2");
        let m = PermissionMatrix::new();
        // org_viewer has org.read but not org.write.
        assert!(m.has_permission(&pool, "u-2", &org_id, "org.read").unwrap());
        assert!(!m
            .has_permission(&pool, "u-2", &org_id, "org.write")
            .unwrap());
        assert!(!m
            .has_permission(&pool, "u-2", &org_id, "camera.write")
            .unwrap());
    }

    #[test]
    fn permission_matrix_caches_result() {
        let (_d, pool) = open_pool();
        let org_id = seed_admin_membership(&pool, "u-3");
        let m = PermissionMatrix::new();
        assert_eq!(m.cache_len(), 0);
        assert!(m
            .has_permission(&pool, "u-3", &org_id, "org.admin")
            .unwrap());
        assert_eq!(m.cache_len(), 1);
        // Second read does not insert a second entry — same key.
        assert!(m
            .has_permission(&pool, "u-3", &org_id, "camera.read")
            .unwrap());
        assert_eq!(m.cache_len(), 1);
    }

    #[test]
    fn permission_matrix_invalidate_clears_cache() {
        let (_d, pool) = open_pool();
        let org_id = seed_admin_membership(&pool, "u-4");
        let m = PermissionMatrix::new();
        assert!(m
            .has_permission(&pool, "u-4", &org_id, "org.admin")
            .unwrap());
        assert_eq!(m.cache_len(), 1);
        m.invalidate("u-4", &org_id);
        assert_eq!(m.cache_len(), 0);
        // Re-read repopulates.
        assert!(m
            .has_permission(&pool, "u-4", &org_id, "org.admin")
            .unwrap());
        assert_eq!(m.cache_len(), 1);
    }

    #[test]
    fn permission_matrix_no_membership_returns_no_membership_err() {
        let (_d, pool) = open_pool();
        let m = PermissionMatrix::new();
        let err = m
            .has_permission(&pool, "ghost", "org-default", "org.read")
            .unwrap_err();
        match err {
            PermissionError::NoMembership(u, o) => {
                assert_eq!(u, "ghost");
                assert_eq!(o, "org-default");
            }
            other => panic!("expected NoMembership, got {:?}", other),
        }
    }

    #[test]
    fn permission_matrix_require_returns_allow_or_deny() {
        let (_d, pool) = open_pool();
        let org_id = seed_viewer_membership(&pool, "u-5");
        let m = PermissionMatrix::new();
        assert!(matches!(
            m.require(&pool, "u-5", &org_id, "org.read"),
            PermissionDecision::Allow
        ));
        assert!(matches!(
            m.require(&pool, "u-5", &org_id, "org.admin"),
            PermissionDecision::Deny(PermissionError::NotGranted(p, _, _)) if p == "org.admin"
        ));
    }

    #[test]
    fn permission_matrix_invalidate_during_load_does_not_poison_cache() {
        // Simulates the race: thread A misses, snapshots generation, loads
        // permissions from DB. Before A writes, the role is mutated and
        // `invalidate` is called (bumping generation). A's write must NOT
        // resurrect the now-stale set; the next reader must re-load and
        // observe the new role's permissions.
        let (_d, pool) = open_pool();
        let org_id = seed_admin_membership(&pool, "u-race");
        let m = PermissionMatrix::new();

        // Prime: cache is empty.
        assert_eq!(m.cache_len(), 0);

        // Step 1: simulate A's miss by capturing generation manually.
        let gen_before = m.generation.load(Ordering::Acquire);
        let stale_set: HashSet<String> =
            PermissionMatrix::load_for_user_org(&pool, "u-race", &org_id).unwrap();
        assert!(stale_set.contains("org.admin"));

        // Step 2: external invalidation (e.g. role swap to viewer).
        m.invalidate("u-race", &org_id);
        assert!(m.generation.load(Ordering::Acquire) > gen_before);

        // Step 3: A finalizes — generation moved, so the insert must be
        // skipped. We reproduce the exact body of `has_permission` here.
        {
            let mut w = m.inner.write();
            if m.generation.load(Ordering::Acquire) == gen_before {
                w.insert(("u-race".to_string(), org_id.clone()), stale_set);
            }
        }
        assert_eq!(
            m.cache_len(),
            0,
            "stale write must not resurrect invalidated entry"
        );

        // Step 4: next reader re-loads from DB and caches the fresh set.
        assert!(m
            .has_permission(&pool, "u-race", &org_id, "org.admin")
            .unwrap());
        assert_eq!(m.cache_len(), 1);
    }

    #[test]
    fn permission_matrix_invalidate_all_flushes_every_entry() {
        let (_d, pool) = open_pool();
        let org_a = seed_admin_membership(&pool, "u-6");
        let org_b = seed_viewer_membership(&pool, "u-7");
        let m = PermissionMatrix::new();
        m.has_permission(&pool, "u-6", &org_a, "org.admin").unwrap();
        m.has_permission(&pool, "u-7", &org_b, "org.read").unwrap();
        assert_eq!(m.cache_len(), 2);
        m.invalidate_all();
        assert_eq!(m.cache_len(), 0);
    }
}
