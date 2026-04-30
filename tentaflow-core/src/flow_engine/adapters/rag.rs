// =============================================================================
// Plik: flow_engine/adapters/rag.rs
// Opis: Adapter wezla RAG - deleguje wyszukiwanie kontekstu do backendu RAG
//       przez QUIC. Obsluguje konfiguracje collection, top_k, threshold
//       i search modes z definicji wezla.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;
use tentaflow_protocol::*;

/// Adapter wezla RAG - wyszukiwanie kontekstu w bazie wiedzy
pub struct RagNodeAdapter {
    service_manager: Arc<ServiceManager>,
    /// Trzymany dla zachowania sygnatury konstruktora (callerzy migruja
    /// w kroku N7.3); aliasy modeli pochodza z DB, nie z config.toml.
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl RagNodeAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Rozwiazuje alias modelu na nazwe kanoniczna. Config-driven aliasy
    /// zostaly skasowane (krok N7.1a); DB `service_aliases` jest rozwiazywany
    /// przez middleware route resolver przed wejsciem do flow.
    fn resolve_model_alias(&self, model: &str) -> String {
        model.to_string()
    }

    /// Rozwiazuje tekst wejsciowy z kontekstu flow
    fn resolve_input_text(&self, node_config: &Value, ctx: &FlowContext) -> String {
        if let Some(input_from) = node_config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = ctx.node_results.get(input_from) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
                if let Some(content) = prev_result.get("content").and_then(|v| v.as_str()) {
                    return content.to_string();
                }
                return prev_result.to_string();
            }
        }

        if let Some(last_log) = ctx.execution_log.last() {
            if let Some(prev_result) = ctx.node_results.get(&last_log.node_id) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
            }
        }

        ctx.input.clone()
    }

    /// Parsuje search modes z konfiguracji wezla
    fn parse_search_modes(&self, node_config: &Value) -> Vec<SearchMode> {
        if let Some(modes) = node_config.get("search_modes").and_then(|v| v.as_array()) {
            let parsed: Vec<SearchMode> = modes
                .iter()
                .filter_map(|m| {
                    m.as_str().and_then(|s| match s {
                        "FullTextSearch" => Some(SearchMode::FullTextSearch),
                        "VectorSearch" => Some(SearchMode::VectorSearch),
                        "HiRAG" => Some(SearchMode::HiRAG),
                        "GSW" => Some(SearchMode::GSW),
                        _ => {
                            warn!("RAG adapter: nieznany search mode: '{}'", s);
                            None
                        }
                    })
                })
                .collect();

            if !parsed.is_empty() {
                return parsed;
            }
        }

        vec![SearchMode::VectorSearch, SearchMode::FullTextSearch]
    }
}

impl NodeAdapter for RagNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let engine_name = node_config
            .get("engine_name")
            .or_else(|| node_config.get("collection_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        let engine_name = self.resolve_model_alias(engine_name);

        let top_k = node_config
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as u32;

        let min_similarity = node_config
            .get("threshold")
            .or_else(|| node_config.get("min_similarity"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.7) as f32;

        let use_reranking = node_config.get("use_reranking").and_then(|v| v.as_bool());

        let search_modes = self.parse_search_modes(node_config);

        let query = self.resolve_input_text(node_config, ctx);

        info!(
            engine = %engine_name,
            query_len = query.len(),
            top_k = top_k,
            "RAG adapter: wyszukiwanie kontekstu"
        );

        // Pobierz handle serwisu RAG
        let rag_handle = self
            .service_manager
            .rag_services
            .get(&engine_name)
            .map(|r| r.value().clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RAG adapter: serwis '{}' nie jest skonfigurowany",
                    engine_name
                )
            })?;

        let rag_client = rag_handle.get_client().await.ok_or_else(|| {
            anyhow::anyhow!("RAG adapter: serwis '{}' nie jest polaczony", engine_name)
        })?;

        // Zbuduj RAGPayload
        let rag_payload = RAGPayload {
            query: query.clone(),
            context: None,
            params: RAGParams {
                top_k,
                min_similarity,
                use_reranking,
            },
            requires_llm_processing: false,
            requires_audio_output: false,
            search_modes,
        };

        debug!("RAG adapter: wysylam zapytanie do '{}'", engine_name);

        let rag_result = rag_client.send_request(rag_payload).await?;

        // Zbierz zrodla z metadanych
        let sources: Vec<Value> = rag_result
            .metadata
            .iter()
            .map(|chunk| {
                serde_json::json!({
                    "text": chunk.chunk_text,
                    "score": chunk.similarity_score,
                    "source": chunk.source_file,
                    "chunk_id": chunk.chunk_id,
                    "rank": chunk.rank,
                })
            })
            .collect();

        let avg_score = if !rag_result.metadata.is_empty() {
            rag_result
                .metadata
                .iter()
                .map(|c| c.similarity_score)
                .sum::<f32>()
                / rag_result.metadata.len() as f32
        } else {
            0.0
        };

        debug!(
            "RAG adapter: otrzymano {} chunkow, avg_score={:.3}",
            rag_result.metadata.len(),
            avg_score
        );

        Ok(serde_json::json!({
            "context": rag_result.context_text,
            "sources": sources,
            "score": avg_score,
            "text": rag_result.context_text,
            "chunks_count": rag_result.metadata.len(),
        }))
    }

    fn node_type(&self) -> &'static str {
        "rag"
    }
}
