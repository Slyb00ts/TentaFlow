// =============================================================================
// Plik: flow_engine/dispatchers/pii_rules.rs
// Opis: PiiRulesStore — narrow trait dla PII rules registry. Implementacja
//       opakowuje `repository::list_pii_rules_active`. CRUD reguł żyje dalej
//       w dashboard handlers (write side).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct PiiRule {
    pub id: i64,
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
}

#[async_trait]
pub trait PiiRulesStore: Send + Sync {
    async fn active_rules(&self) -> Result<Vec<PiiRule>>;
}
