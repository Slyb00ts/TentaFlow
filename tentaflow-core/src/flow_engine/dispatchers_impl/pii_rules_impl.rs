// =============================================================================
// Plik: flow_engine/dispatchers_impl/pii_rules_impl.rs
// Opis: PiiRulesStoreImpl — wrapper nad `repository::list_pii_rules_active`.
//       Pomija nieaktywne reguły, mapuje DbPiiRule na DTO PiiRule (5 pól
//       wymaganych przez adapter pii_filter).
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::{PiiRule, PiiRulesStore};

pub struct PiiRulesStoreImpl {
    db: DbPool,
}

impl PiiRulesStoreImpl {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PiiRulesStore for PiiRulesStoreImpl {
    async fn active_rules(&self) -> Result<Vec<PiiRule>> {
        let db = self.db.clone();
        let rows = tokio::task::spawn_blocking(move || repository::list_pii_rules_active(&db))
            .await??;
        Ok(rows
            .into_iter()
            .map(|r| PiiRule {
                id: r.id,
                name: r.name,
                category: r.category,
                pattern: r.pattern,
                replacement: r.replacement,
            })
            .collect())
    }
}
