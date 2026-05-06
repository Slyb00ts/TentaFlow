// =============================================================================
// Plik: flow_engine/dispatchers_impl/memory_impl.rs
// Opis: MemoryStoreImpl — wrapper nad memory engine'em rozmawiającym po QUIC
//       (rkyv `MemoryPayload`). Adapter `memory` (stage 1c) widzi tylko
//       narrow trait `MemoryStore::recall/store`. QuicClientFinder oddziela
//       wrapper od `ServiceManager` (D4 invariant).
// =============================================================================

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use std::sync::Arc;

use super::quic_finder::QuicClientFinder;
use crate::flow_engine::dispatchers::{
    MemoryHit, MemoryQuery, MemoryRecall, MemoryRecord, MemoryStore, MemoryStoreReceipt,
};
use tentaflow_protocol::*;

/// Domyślna nazwa serwisu memory engine w katalogu — zgodne z
/// `adapters/memory.rs:34` (`find_quic_client_for_model("memory")`).
const MEMORY_SERVICE_NAME: &str = "memory";

pub struct MemoryStoreImpl {
    finder: Arc<dyn QuicClientFinder>,
}

impl MemoryStoreImpl {
    pub fn new(finder: Arc<dyn QuicClientFinder>) -> Self {
        Self { finder }
    }

    async fn client(&self) -> Result<Arc<crate::net::quic::QuicClient>> {
        self.finder
            .find(MEMORY_SERVICE_NAME)
            .await
            .ok_or_else(|| anyhow!("MemoryStore: no connected memory service"))
    }
}

#[async_trait]
impl MemoryStore for MemoryStoreImpl {
    async fn recall(&self, query: MemoryQuery) -> Result<MemoryRecall> {
        let client = self.client().await?;
        let session_id = query
            .session_id
            .clone()
            .or_else(|| query.person_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let request_id = uuid::Uuid::new_v4().to_string();
        let request = ModelRequest {
            request_id,
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Query {
                    session_id: session_id.clone(),
                    query: query.query_text,
                    query_embedding: None,
                    query_type: MemoryQueryType::What,
                    max_depth: Some(3),
                    top_k: Some(query.top_k.max(1)),
                    include_reasoning: Some(false),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id),
        };

        let response = client.send_request(request).await?;
        match response.result {
            ModelResult::Memory(MemoryResult { result_type }) => match result_type {
                MemoryResultType::Query(q) => Ok(MemoryRecall {
                    hits: q
                        .answers
                        .into_iter()
                        .map(|a| MemoryHit {
                            content: a.label,
                            score: a.score,
                            source_id: Some(a.node_id.to_string()),
                        })
                        .collect(),
                }),
                _ => Ok(MemoryRecall::default()),
            },
            ModelResult::Error(err) => bail!(
                "MemoryStore recall error: {:?} - {}",
                err.error_type,
                err.message
            ),
            _ => Ok(MemoryRecall::default()),
        }
    }

    async fn store(&self, record: MemoryRecord) -> Result<MemoryStoreReceipt> {
        let client = self.client().await?;
        let session_id = record
            .session_id
            .clone()
            .or_else(|| record.person_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Stage-1 mapping: cały content trafia jako jeden fakt
        // (subject="context", relation="contains", object=content). Tagi
        // przechodzą do `metadata` jako pary ("tag", value), żeby memory engine
        // mógł je później indeksować.
        let metadata = if record.tags.is_empty() {
            None
        } else {
            Some(
                record
                    .tags
                    .iter()
                    .map(|t| ("tag".to_string(), t.clone()))
                    .collect::<Vec<_>>(),
            )
        };

        let fact = MemoryFact {
            subject: "context".to_string(),
            relation: "contains".to_string(),
            object: record.content,
            confidence: 1.0,
            source: Some("flow_engine".to_string()),
            metadata,
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let request = ModelRequest {
            request_id,
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Store {
                    session_id: session_id.clone(),
                    facts: vec![fact],
                    context_embedding: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.clone()),
        };

        let response = client.send_request(request).await?;
        match response.result {
            ModelResult::Memory(MemoryResult { result_type }) => match result_type {
                MemoryResultType::Store(s) => Ok(MemoryStoreReceipt {
                    stored: s.facts_stored > 0,
                    record_id: Some(s.session_id),
                }),
                _ => Ok(MemoryStoreReceipt {
                    stored: false,
                    record_id: None,
                }),
            },
            ModelResult::Error(err) => bail!(
                "MemoryStore store error: {:?} - {}",
                err.error_type,
                err.message
            ),
            _ => Ok(MemoryStoreReceipt {
                stored: false,
                record_id: None,
            }),
        }
    }
}
