// ===== File: project_studio/activity.rs — per-project activity log + org security audit =====
//
// Every mutation records a row in the project's own `activity_log` (the feed
// on the overview screen). Org-level security events (project create/delete,
// membership changes) ADDITIONALLY go to the core hash-chained `audit_log`
// via the existing `db::repository::log_audit` mechanism.

use crate::db::DbPool;

/// Appends one entry to the project's `activity_log`. Best-effort: a failed
/// activity write must never fail the mutation it describes, so errors are
/// logged and swallowed.
pub fn record(
    project_pool: &DbPool,
    actor_user_id: &str,
    actor_kind: &str,
    action: &str,
    object_type: &str,
    object_id: &str,
    details_json: &str,
) {
    if let Err(e) = super::repository::insert_activity(
        project_pool,
        actor_user_id,
        actor_kind,
        action,
        object_type,
        object_id,
        details_json,
    ) {
        tracing::warn!(action, "project activity write failed: {e}");
    }
}

/// Records an org-level security event in the CORE `audit_log` (hash-chained,
/// same mechanism as every other platform mutation). Best-effort like the
/// core `dispatch::handlers::audit` helper.
pub fn record_org_security(
    core_db: &DbPool,
    node_id: &str,
    user_id: &str,
    action: &str,
    resource: &str,
    details: &str,
) {
    let _ = crate::db::repository::log_audit(
        core_db,
        Some(user_id),
        None,
        action,
        Some(resource),
        Some(details),
        None,
        Some(node_id),
    );
}
