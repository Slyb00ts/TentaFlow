// =============================================================================
// Plik: compliance/mod.rs
// Opis: Compliance Core scala RODO, AI audit, retencje i rejestry zgodności.
// =============================================================================

pub mod ai_gateway;
pub mod audit_worker;
pub mod models;
pub mod repository;

pub const MINIMUM_AI_AUDIT_RETENTION_DAYS: i64 = 183;

/// Default retention term of the event-log timeline (`events.db`), in days.
pub const EVENTS_RETENTION_DAYS: i64 = 30;

/// Stable base id of the per-org default event-log retention policy. Shared by
/// the org seed and the v129 fleet backfill so both write the same row.
pub const EVENTS_RETENTION_POLICY_BASE_ID: &str = "ret-core-events-default";

/// Display name of that policy. Every locale the dashboard ships must be here —
/// `compliance_retention_policies.name_translations` is surfaced verbatim.
pub const EVENTS_RETENTION_NAME_TRANSLATIONS: &str = concat!(
    r#"{"pl":"Dziennik zdarzeń","en":"Event log","de":"Ereignisprotokoll","#,
    r#""es":"Registro de eventos","fr":"Journal des événements"}"#
);
