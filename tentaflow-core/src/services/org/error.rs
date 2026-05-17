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
}

pub type Result<T> = std::result::Result<T, OrgError>;
