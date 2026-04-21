// =============================================================================
// Plik: flow_engine/adapters/memory.rs
// Opis: Adapter wezla Memory - odpytuje lub zapisuje dane w grafie wiedzy
//       (TentaFlow.Memory) przez QUIC. Obsluguje dwa tryby: "query" i "store".
// =============================================================================

use anyhow::{bail, Result};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;
use tentaflow_protocol::*;

/// Adapter wezla Memory - query/store w grafie wiedzy
pub struct MemoryNodeAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl MemoryNodeAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Pobiera pierwszego dostepnego klienta QUIC Memory
    async fn get_memory_client(&self) -> Result<Arc<crate::net::quic::QuicClient>> {
        let handles: Vec<_> = self
            .service_manager
            .quic_memory_services
            .read()
            .values()
            .cloned()
            .collect();
        for handle in handles {
            if let Some(client) = handle.get_client().await {
                return Ok(client);
            }
        }

        bail!("Memory adapter: brak polaczonego serwisu Memory");
    }

    /// Rozwiazuje tekst wejsciowy z kontekstu flow - szuka wstecz w execution_log
    fn resolve_input_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
        if let Some(query) = node_config.get("query").and_then(|v| v.as_str()) {
            return query.to_string();
        }

        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
                if let Some(content) = prev_result.get("content").and_then(|v| v.as_str()) {
                    return content.to_string();
                }
            }
        }

        for step in ctx.execution_log.iter().rev() {
            if let Some(prev_result) = ctx.node_results.get(&step.node_id) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
            }
        }

        ctx.input.clone()
    }

    /// Rozwiazuje zapytanie do pamieci - preferuje search_terms z memory_analyzer
    fn resolve_query_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
        for result in ctx.node_results.values() {
            if result.get("should_query").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(terms) = result.get("search_terms").and_then(|v| v.as_array()) {
                    let terms_str: Vec<&str> = terms.iter().filter_map(|t| t.as_str()).collect();
                    if !terms_str.is_empty() {
                        debug!(
                            terms_count = terms_str.len(),
                            "Memory adapter: uzywam search_terms z memory_analyzer"
                        );
                        return terms_str.join(" ");
                    }
                }
            }
        }
        self.resolve_input_text(node_config, ctx)
    }

    /// Wykonuje zapytanie do Memory (tryb "query")
    async fn execute_query(&self, node_config: &Value, ctx: &FlowContext) -> Result<Value> {
        let quic_client = self.get_memory_client().await?;

        let query = self.resolve_query_text(node_config, ctx);
        let session_id = node_config
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.request_id);

        let max_depth = node_config
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|d| d as u32)
            .unwrap_or(3);

        let top_k = node_config
            .get("top_k")
            .and_then(|v| v.as_u64())
            .map(|k| k as u32)
            .unwrap_or(10);

        info!(
            query_len = query.len(),
            session_id = session_id,
            "Memory adapter: zapytanie do grafu wiedzy"
        );

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Query {
                    session_id: session_id.to_string(),
                    query: query.clone(),
                    query_embedding: None,
                    query_type: MemoryQueryType::What,
                    max_depth: Some(max_depth),
                    top_k: Some(top_k),
                    include_reasoning: Some(true),
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        let response = quic_client.send_request(model_request).await?;

        match response.result {
            ModelResult::Memory(memory_result) => match memory_result.result_type {
                MemoryResultType::Query(query_result) => {
                    let memories: Vec<Value> = query_result
                        .answers
                        .iter()
                        .map(|answer| {
                            serde_json::json!({
                                "id": answer.node_id,
                                "label": answer.label,
                                "node_type": answer.node_type,
                                "score": answer.score,
                            })
                        })
                        .collect();

                    let avg_relevance = if !query_result.answers.is_empty() {
                        query_result.answers.iter().map(|a| a.score).sum::<f32>()
                            / query_result.answers.len() as f32
                    } else {
                        0.0
                    };

                    let context_text: String = query_result
                        .answers
                        .iter()
                        .filter(|a| a.score >= 0.5)
                        .map(|a| a.label.clone())
                        .collect::<Vec<_>>()
                        .join("; ");

                    let reasoning_count = query_result
                        .reasoning_paths
                        .as_ref()
                        .map(|p| p.len())
                        .unwrap_or(0);

                    debug!(
                        "Memory adapter: otrzymano {} odpowiedzi, {} sciezek rozumowania",
                        query_result.answers.len(),
                        reasoning_count
                    );

                    Ok(serde_json::json!({
                        "memories": memories,
                        "relevance": avg_relevance,
                        "text": context_text,
                        "answers_count": query_result.answers.len(),
                        "reasoning_paths_count": reasoning_count,
                    }))
                }
                _ => {
                    warn!("Memory adapter: nieoczekiwany typ wyniku Memory");
                    Ok(serde_json::json!({
                        "memories": [],
                        "relevance": 0,
                        "text": "",
                    }))
                }
            },
            ModelResult::Error(err) => {
                bail!(
                    "Memory adapter query error: {:?} - {}",
                    err.error_type,
                    err.message
                );
            }
            _ => {
                warn!("Memory adapter: nieoczekiwany typ odpowiedzi");
                Ok(serde_json::json!({
                    "memories": [],
                    "relevance": 0,
                    "text": "",
                }))
            }
        }
    }

    /// Wstrzykuje kontekst z pamieci do system message w ctx.messages
    fn inject_memory_context(messages: &mut Vec<Value>, context: &str) {
        if messages.is_empty() || context.is_empty() {
            return;
        }
        if let Some(first_msg) = messages.first_mut() {
            if first_msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                if let Some(content) = first_msg.get("content").and_then(|c| c.as_str()) {
                    let new_content = format!("{}\n\n{}", content, context);
                    *first_msg = serde_json::json!({
                        "role": "system",
                        "content": new_content,
                    });
                }
            }
        }
    }

    /// Wykonuje zapis do Memory (tryb "store")
    async fn execute_store(&self, node_config: &Value, ctx: &FlowContext) -> Result<Value> {
        let quic_client = self.get_memory_client().await?;

        let session_id = node_config
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.request_id);

        let facts_json = node_config.get("facts");
        let content = self.resolve_input_text(node_config, ctx);

        info!(
            session_id = session_id,
            content_len = content.len(),
            "Memory adapter: zapis do grafu wiedzy"
        );

        let mut facts = Vec::new();

        if let Some(facts_arr) = facts_json.and_then(|v| v.as_array()) {
            for fact in facts_arr {
                let subject = fact
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let relation = fact
                    .get("relation")
                    .or_else(|| fact.get("predicate"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let object = fact
                    .get("object")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if !subject.is_empty() && !relation.is_empty() && !object.is_empty() {
                    facts.push(MemoryFact {
                        subject,
                        relation,
                        object,
                        confidence: fact
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .map(|c| c as f32)
                            .unwrap_or(1.0),
                        source: Some("flow_engine".to_string()),
                        metadata: None,
                    });
                }
            }
        }

        if facts.is_empty() && !content.is_empty() {
            facts.push(MemoryFact {
                subject: "context".to_string(),
                relation: "contains".to_string(),
                object: content,
                confidence: 1.0,
                source: Some("flow_engine".to_string()),
                metadata: None,
            });
        }

        if facts.is_empty() {
            debug!("Memory adapter: brak faktow do zapisu");
            return Ok(serde_json::json!({
                "stored": false,
            }));
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let model_request = ModelRequest {
            request_id: request_id.clone(),
            payload: ModelPayload::Memory(MemoryPayload {
                operation: MemoryOperation::Store {
                    session_id: session_id.to_string(),
                    facts: facts.clone(),
                    context_embedding: None,
                },
            }),
            stream: false,
            metadata: None,
            session_id: Some(session_id.to_string()),
        };

        match quic_client.send_request(model_request).await {
            Ok(response) => match response.result {
                ModelResult::Memory(_) => {
                    debug!("Memory adapter: zapisano {} faktow", facts.len());
                    Ok(serde_json::json!({
                        "stored": true,
                        "facts_count": facts.len(),
                    }))
                }
                ModelResult::Error(err) => {
                    bail!(
                        "Memory adapter store error: {:?} - {}",
                        err.error_type,
                        err.message
                    );
                }
                _ => {
                    warn!("Memory adapter: nieoczekiwany typ odpowiedzi store");
                    Ok(serde_json::json!({
                        "stored": false,
                    }))
                }
            },
            Err(e) => {
                bail!("Memory adapter: QUIC store request failed: {}", e);
            }
        }
    }
}

impl NodeAdapter for MemoryNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let mode = node_config
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("query");

        match mode {
            "query" => {
                let result = self.execute_query(node_config, ctx).await?;

                let inject = node_config
                    .get("inject_to_messages")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if inject {
                    if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let prompt_id = node_config
                                .get("context_prompt_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("memory_context_template");
                            let template = self
                                .service_manager
                                .prompt_registry
                                .get_content(prompt_id)
                                .map(|s| s.replace("{context}", text))
                                .unwrap_or_else(|| format!("Kontekst z pamieci:\n{}", text));
                            Self::inject_memory_context(&mut ctx.messages, &template);
                        }
                    }
                }

                Ok(result)
            }
            "store" => self.execute_store(node_config, ctx).await,
            other => {
                bail!(
                    "Memory adapter: nieznany tryb '{}' (oczekiwano 'query' lub 'store')",
                    other
                );
            }
        }
    }

    fn node_type(&self) -> &'static str {
        "memory"
    }
}
