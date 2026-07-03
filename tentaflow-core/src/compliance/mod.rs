// =============================================================================
// Plik: compliance/mod.rs
// Opis: Compliance Core scala RODO, AI audit, retencje i rejestry zgodności.
// =============================================================================

pub mod ai_gateway;
pub mod audit_worker;
pub mod models;
pub mod repository;

pub const MINIMUM_AI_AUDIT_RETENTION_DAYS: i64 = 183;
