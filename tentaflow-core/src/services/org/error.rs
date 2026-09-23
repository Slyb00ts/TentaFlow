// ============ File: services/org/error.rs — OrgError enum ============
//
// Errors surfaced by the multi-tenant repository. DB connection issues bubble
// up as `DbError`; the caller (HTTP handler, CLI, host fn) decides whether to
// deny or short-circuit with 5xx. The `SlugConflict` variant maps to HTTP 409
// when a user tries to create an org whose slug already exists.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OrgError {
    #[error("organization not found: {0}")]
    NotFound(String),

    #[error("organization slug already in use: {0}")]
    SlugConflict(String),

    #[error("role not found: {0}")]
    RoleNotFound(String),

    #[error("membership already exists for (org={org_id}, user={user_id})")]
    MembershipExists { org_id: String, user_id: String },

    #[error("org DB error: {0}")]
    DbError(String),

    /// GDPR/RODO erasure: `delete_organization` flipped the row to
    /// `status = 'deleted'` (that half committed and is NOT rolled back by
    /// this error), but `BusService::purge_org` failed on at least one
    /// running TentaBus instance holding this org's data. Every failure is
    /// also written to `audit_log` (`bus.org.purge_failed`) before this is
    /// returned, so the outcome is never silent — see
    /// `services::org::repo::purge_bus_data_for_org`'s doc for why a retry
    /// is safe to drive through the same call.
    #[error(
        "organization {org_id} soft-deleted, but BusService::purge_org failed on {failures:?}"
    )]
    BusPurgeFailed {
        org_id: String,
        failures: Vec<String>,
    },
}

pub type Result<T> = std::result::Result<T, OrgError>;
