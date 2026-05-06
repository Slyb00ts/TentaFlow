// =============================================================================
// Plik: flow_engine/node_adapters/memory.rs
// Opis: MemoryNodeAdapter — query/store w grafie wiedzy przez MemoryStore.
//       Tryb "query" (default): pobiera kontekst dla payload.Text i dopisuje
//       jako System message do envelope.context.system_prompts. Tryb "store":
//       zapisuje payload.Text do memory engine'u. Plan v4.2 D1: brak
//       cross-node lookupu — query/store budowane wyłącznie z inputs[0]
//       envelope + node config + ctx.session_id.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::{MemoryQuery, MemoryRecord};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const NODE_TYPE: &str = "memory";
const DEFAULT_TOP_K: u32 = 10;
const MIN_RELEVANCE: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Query,
    Store,
}

pub struct MemoryNodeAdapter;

impl MemoryNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_mode(node: &FlowNode) -> Mode {
        match node.config.get("mode").and_then(|v| v.as_str()) {
            Some("store") => Mode::Store,
            _ => Mode::Query,
        }
    }

    fn pick_session(node: &FlowNode, ctx: &ExecutionContext) -> Result<String> {
        if let Some(s) = node
            .config
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(s.to_string());
        }
        ctx.session_id
            .clone()
            .ok_or_else(|| anyhow!("memory adapter: no session_id (node config nor ctx.session_id)"))
    }

    fn payload_text(envelope: &FlowEnvelope) -> Result<String> {
        match &envelope.payload {
            FlowValue::Text(t) if !t.is_empty() => Ok(t.clone()),
            _ => Err(anyhow!(
                "memory adapter: payload must be non-empty Text, got {}",
                envelope.payload.kind()
            )),
        }
    }
}

impl Default for MemoryNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for MemoryNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn supported_input_ports(&self) -> &[&'static str] {
        &["in"]
    }
    fn supported_output_ports(&self) -> &[&'static str] {
        &["full"]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("memory adapter: missing input edge"))?;
        let envelope = &input.envelope;
        let mode = Self::pick_mode(node);
        let session = Self::pick_session(node, ctx)?;
        // person_id: node config > envelope.meta > None. Speaker/STT
        // pipeline wstrzykuje rozpoznanego mówcę do meta['person_id'],
        // więc fallback z meta jest tym, co pozwala memory engine'owi
        // partycjonować recall/store po faktycznej osobie zamiast po
        // statycznej konfiguracji.
        let person_id = node
            .config
            .get("person_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                envelope
                    .meta
                    .get("person_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });

        let query_text = Self::payload_text(envelope)?;

        match mode {
            Mode::Query => {
                let top_k = node
                    .config
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .unwrap_or(DEFAULT_TOP_K);
                let q = MemoryQuery {
                    session_id: Some(session),
                    person_id,
                    query_text,
                    top_k,
                };
                let recall = ctx.memory.recall(q).await?;

                // Wstrzykujemy do system_prompts agregowane facts (label
                // każdego hit'a powyżej MIN_RELEVANCE). Format: prosta
                // lista "- {label}". Adapter LLM zamieni system_prompts
                // na osobne System messages.
                let context_lines: Vec<String> = recall
                    .hits
                    .iter()
                    .filter(|h| h.score >= MIN_RELEVANCE)
                    .map(|h| format!("- {}", h.content))
                    .collect();

                let mut out: FlowEnvelope = (**envelope).clone();
                if !context_lines.is_empty() {
                    let prefix = node
                        .config
                        .get("context_prefix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Kontekst z pamięci:");
                    let block = format!("{prefix}\n{}", context_lines.join("\n"));
                    out.context.system_prompts.push(block);
                }
                Ok(out)
            }
            Mode::Store => {
                let tags: Vec<String> = node
                    .config
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let record = MemoryRecord {
                    session_id: Some(session),
                    person_id,
                    content: query_text,
                    tags,
                };
                let _receipt = ctx.memory.store(record).await?;
                // Store mode: passthrough envelope. Outcome może iść w meta
                // gdyby adapter chciał raportować — dziś tylko side effect.
                let out: FlowEnvelope = (**envelope).clone();
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{
        MemoryHit, MemoryRecall, MemoryStore, MemoryStoreReceipt,
    };
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "m1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    struct FakeMemory {
        hits: Vec<MemoryHit>,
        stored: Mutex<Vec<MemoryRecord>>,
    }

    #[async_trait]
    impl MemoryStore for FakeMemory {
        async fn recall(&self, _q: MemoryQuery) -> Result<MemoryRecall> {
            Ok(MemoryRecall {
                hits: self.hits.clone(),
            })
        }
        async fn store(&self, r: MemoryRecord) -> Result<MemoryStoreReceipt> {
            self.stored.lock().unwrap().push(r);
            Ok(MemoryStoreReceipt {
                stored: true,
                record_id: Some("rec1".into()),
            })
        }
    }

    #[tokio::test]
    async fn query_mode_appends_relevant_hits_as_system_prompt() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("co ja lubie".into());
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s1".into());
        ctx.memory = Arc::new(FakeMemory {
            hits: vec![
                MemoryHit {
                    content: "kawa".into(),
                    score: 0.9,
                    source_id: None,
                },
                MemoryHit {
                    content: "low".into(),
                    score: 0.2,
                    source_id: None,
                },
            ],
            stored: Mutex::new(Vec::new()),
        });

        let out = MemoryNodeAdapter::new()
            .execute(&node(json!({"mode": "query"})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts.len(), 1);
        let block = &out.context.system_prompts[0];
        assert!(block.contains("kawa"));
        assert!(!block.contains("low"));
    }

    #[tokio::test]
    async fn store_mode_calls_dispatcher_and_passes_through() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nowa preferencja".into());
        let mut ctx = stub_ctx();
        ctx.session_id = Some("s2".into());
        let fake = Arc::new(FakeMemory {
            hits: vec![],
            stored: Mutex::new(Vec::new()),
        });
        ctx.memory = fake.clone();

        let out = MemoryNodeAdapter::new()
            .execute(
                &node(json!({"mode": "store", "tags": ["pref"]})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.context.system_prompts.is_empty());
        let stored = fake.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].content, "nowa preferencja");
        assert_eq!(stored[0].tags, vec!["pref".to_string()]);
    }

    #[tokio::test]
    async fn missing_session_id_errors() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("x".into());
        let mut ctx = stub_ctx();
        ctx.session_id = None;
        ctx.memory = Arc::new(FakeMemory {
            hits: vec![],
            stored: Mutex::new(Vec::new()),
        });
        let err = MemoryNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("session_id"));
    }
}
