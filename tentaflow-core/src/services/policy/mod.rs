// ============ File: services/policy/mod.rs — F1c P4 policy/claims engine ============
//
// Verifies DPIA / FRIA / legal-grant / consent claims issued by an admin
// against the requirements declared by an addon manifest `[[gate]]` block.
// Addon-facing API: `engine::verify_claim` (called by gate_check_v1 host
// fn + by vector_search_v1 when a namespace declares a gate).
// Admin-facing API: `repo::{issue, revoke, list, get}` (called by CLI
// `tentaflow-cli policy ...`).

pub mod engine;
pub mod error;
pub mod repo;

pub use engine::{verify_claim, ClaimContext, ClaimVerified, SignerEntry};
pub use error::PolicyError;
pub use repo::{
    delete_signature, get_claim, insert_claim, insert_signature, list_claims, list_signatures,
    revoke_claim, ClaimRow, ClaimSignatureRow, ListFilter, NewClaim, NewSignature,
};
