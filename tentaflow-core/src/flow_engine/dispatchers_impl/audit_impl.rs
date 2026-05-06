// =============================================================================
// Plik: flow_engine/dispatchers_impl/audit_impl.rs
// Opis: AuditSinkImpl — wrapper nad `repository::log_audit`. Mapuje
//       `AuditEvent { action, actor, target, metadata }` na pola tabeli
//       `audit_log`: actor → `details.actor`, target → resource, metadata
//       → details JSON.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::db::{repository, DbPool};
use crate::flow_engine::dispatchers::{AuditEvent, AuditSink};

pub struct AuditSinkImpl {
    db: DbPool,
}

impl AuditSinkImpl {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AuditSink for AuditSinkImpl {
    async fn record(&self, event: AuditEvent) -> Result<()> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            // Wstrzykujemy actor do details.actor jeżeli był podany — pole `user_id`
            // w tabeli przyjmuje tylko i64, więc string actor (np. session_id) idzie
            // w JSON details.
            let mut details = if event.metadata.is_object() {
                event.metadata.clone()
            } else {
                serde_json::json!({ "value": event.metadata })
            };
            if let Some(a) = &event.actor {
                if let Some(obj) = details.as_object_mut() {
                    obj.insert("actor".into(), serde_json::Value::String(a.clone()));
                }
            }
            let details_str = details.to_string();
            repository::log_audit(
                &db,
                None,
                None,
                &event.action,
                event.target.as_deref(),
                Some(&details_str),
                None,
                None,
            )
        })
        .await??;
        Ok(())
    }
}
