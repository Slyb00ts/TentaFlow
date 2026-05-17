// ============ File: services/rbac/middleware.rs — per-request OrgContext resolver ============
//
// `OrgContext` is the per-request snapshot of (user_id, org_id, role_id,
// permissions) that handlers consume to scope their work. It is materialised
// once at request entry (`resolve_org_context`) and threaded through the
// dispatch / host-fn boundary so the matrix is not re-queried on every
// permission check inside a single request.
//
// Resolution rules:
//   * `requested_org_id = Some(org)` — verify the user is a member of `org`,
//     emit `NotMemberOfOrg` otherwise. This is the case for browser sessions
//     that pin an org via `X-Org-Id` header (or the WS subprotocol payload).
//   * `requested_org_id = None` — pick the user's first / default org. When
//     a user belongs to several orgs and does not pin one explicitly, the
//     binding is non-deterministic from the caller's perspective; the
//     dashboard is responsible for sending `X-Org-Id` once it knows which
//     org the user picked in the org-switcher UI. Backward-compat: most
//     existing nodes are single-tenant (every user belongs to `org-default`
//     only) so this branch returns the only membership.
//   * No memberships at all → `NoMembership`. Handler maps to HTTP 403.

use std::collections::HashSet;

use thiserror::Error;

use crate::db::DbPool;
use crate::services::org::repo;

#[derive(Debug, Clone)]
pub struct OrgContext {
    pub user_id: String,
    pub org_id: String,
    pub role_id: String,
    /// Snapshot of the role's permission list captured at resolve time. Stays
    /// stable for the duration of the request — a role grant change concurrent
    /// with an in-flight handler does not flip an Allow into a Deny mid-call.
    pub permissions: HashSet<String>,
}

impl OrgContext {
    pub fn has(&self, perm: &str) -> bool {
        self.permissions.contains(perm)
    }
}

#[derive(Debug, Error)]
pub enum OrgContextError {
    #[error("missing or invalid session")]
    NoSession,

    #[error("user '{0}' has no organization membership")]
    NoMembership(String),

    #[error("invalid org_id header: {0}")]
    InvalidOrgHeader(String),

    #[error("user '{user_id}' is not a member of org '{org_id}'")]
    NotMemberOfOrg { user_id: String, org_id: String },

    #[error("rbac db error: {0}")]
    Db(String),
}

/// Resolve the org context for a single request. The caller is responsible for
/// providing the authenticated `user_id` (extracted from the session by the
/// WS handshake or HTTP auth layer); this function does not authenticate.
///
/// `requested_org_id` carries the org pinned by the client (header /
/// subprotocol). When it is `None`, the user's only / first membership is
/// selected. Empty strings in `requested_org_id` are treated as malformed and
/// rejected — a downstream caller that sees an empty header must surface
/// `InvalidOrgHeader` rather than silently fall back to the default org.
pub fn resolve_org_context(
    db: &DbPool,
    user_id: &str,
    requested_org_id: Option<&str>,
) -> Result<OrgContext, OrgContextError> {
    if user_id.is_empty() {
        return Err(OrgContextError::NoSession);
    }
    if let Some(req) = requested_org_id {
        if req.is_empty() {
            return Err(OrgContextError::InvalidOrgHeader(
                "empty org_id".to_string(),
            ));
        }
    }

    let memberships = repo::list_memberships_for_user(db, user_id)
        .map_err(|e| OrgContextError::Db(e.to_string()))?;
    if memberships.is_empty() {
        return Err(OrgContextError::NoMembership(user_id.to_string()));
    }

    let chosen = match requested_org_id {
        Some(req) => memberships
            .into_iter()
            .find(|(org, _)| org.org_id == req)
            .ok_or_else(|| OrgContextError::NotMemberOfOrg {
                user_id: user_id.to_string(),
                org_id: req.to_string(),
            })?,
        // `list_memberships_for_user` orders by `created_at ASC, org_id ASC`,
        // so the default branch is deterministic for a given user.
        None => memberships.into_iter().next().expect("non-empty checked"),
    };

    let (org, role) = chosen;
    let permissions: HashSet<String> = role.permissions.into_iter().collect();
    Ok(OrgContext {
        user_id: user_id.to_string(),
        org_id: org.org_id,
        role_id: role.role_id,
        permissions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::org::repo as org_repo;
    use tempfile::TempDir;

    fn open_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("rbac_mw_test.db");
        let pool = crate::db::init(&path).expect("init DB");
        (dir, pool)
    }

    fn role_id(pool: &DbPool, name: &str) -> String {
        org_repo::list_roles(pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == name)
            .unwrap()
            .role_id
    }

    #[test]
    fn resolve_org_context_with_requested_org_member() {
        let (_d, pool) = open_pool();
        let org_a = org_repo::create_organization(&pool, "A", "a", None, None, None, None).unwrap();
        let admin = role_id(&pool, "org_admin");
        org_repo::add_membership(&pool, &org_a.org_id, "u-1", &admin, "boot").unwrap();

        let ctx = resolve_org_context(&pool, "u-1", Some(&org_a.org_id)).unwrap();
        assert_eq!(ctx.user_id, "u-1");
        assert_eq!(ctx.org_id, org_a.org_id);
        assert_eq!(ctx.role_id, admin);
        assert!(ctx.has("org.admin"));
    }

    #[test]
    fn resolve_org_context_with_requested_org_non_member() {
        let (_d, pool) = open_pool();
        let org_a = org_repo::create_organization(&pool, "A", "a", None, None, None, None).unwrap();
        let org_b = org_repo::create_organization(&pool, "B", "b", None, None, None, None).unwrap();
        let admin = role_id(&pool, "org_admin");
        org_repo::add_membership(&pool, &org_a.org_id, "u-1", &admin, "boot").unwrap();

        let err = resolve_org_context(&pool, "u-1", Some(&org_b.org_id)).unwrap_err();
        match err {
            OrgContextError::NotMemberOfOrg { user_id, org_id } => {
                assert_eq!(user_id, "u-1");
                assert_eq!(org_id, org_b.org_id);
            }
            other => panic!("expected NotMemberOfOrg, got {:?}", other),
        }
    }

    #[test]
    fn resolve_org_context_default_org_when_unspecified() {
        let (_d, pool) = open_pool();
        let org_a = org_repo::create_organization(&pool, "Only", "only", None, None, None, None).unwrap();
        let viewer = role_id(&pool, "org_viewer");
        org_repo::add_membership(&pool, &org_a.org_id, "u-1", &viewer, "boot").unwrap();

        let ctx = resolve_org_context(&pool, "u-1", None).unwrap();
        assert_eq!(ctx.org_id, org_a.org_id);
        assert_eq!(ctx.role_id, viewer);
    }

    #[test]
    fn resolve_org_context_user_no_memberships() {
        let (_d, pool) = open_pool();
        let err = resolve_org_context(&pool, "ghost", None).unwrap_err();
        assert!(matches!(err, OrgContextError::NoMembership(u) if u == "ghost"));
    }

    #[test]
    fn resolve_org_context_rejects_empty_session() {
        let (_d, pool) = open_pool();
        let err = resolve_org_context(&pool, "", None).unwrap_err();
        assert!(matches!(err, OrgContextError::NoSession));
    }

    #[test]
    fn resolve_org_context_rejects_empty_org_header() {
        let (_d, pool) = open_pool();
        let err = resolve_org_context(&pool, "u-1", Some("")).unwrap_err();
        assert!(matches!(err, OrgContextError::InvalidOrgHeader(_)));
    }
}
