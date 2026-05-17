// ============ File: services/policy/error.rs — PolicyError enum ============
//
// Errors surfaced by the policy/claims engine. The engine never panics on a
// DB connection issue — those bubble up as `DbError` and the caller (host fn
// or CLI) decides whether to deny or short-circuit with a 5xx.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("claim not found: {0}")]
    ClaimNotFound(String),

    #[error("claim revoked: {claim_id} (reason: {reason})")]
    ClaimRevoked { claim_id: String, reason: String },

    #[error("claim outside validity window: {claim_id} (now={now}, valid_from={valid_from}, valid_until={valid_until})")]
    ClaimNotInValidityPeriod {
        claim_id: String,
        now: String,
        valid_from: String,
        valid_until: String,
    },

    #[error("claim type mismatch: expected '{expected}', found '{actual}'")]
    ClaimTypeMismatch { expected: String, actual: String },

    #[error("claim scope mismatch: {detail}")]
    ClaimScopeMismatch { detail: String },

    #[error("missing required signer role: {role}")]
    MissingRequiredSigner { role: String },

    #[error("policy DB error: {0}")]
    DbError(String),
}

pub type Result<T> = std::result::Result<T, PolicyError>;
