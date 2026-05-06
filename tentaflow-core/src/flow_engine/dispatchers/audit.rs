// =============================================================================
// Plik: flow_engine/dispatchers/audit.rs
// Opis: AuditSink — narrow trait nad repository::log_audit. Adapter LLM /
//       memory / pii_filter pisze event po istotnym rozstrzygnięciu (np.
//       PII redaction, cache hit/miss, model fallback).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub action: String,
    pub actor: Option<String>,
    pub target: Option<String>,
    pub metadata: serde_json::Value,
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<()>;
}
