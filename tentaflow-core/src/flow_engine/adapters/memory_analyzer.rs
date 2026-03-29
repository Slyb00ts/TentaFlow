// =============================================================================
// Plik: flow_engine/adapters/memory_analyzer.rs
// Opis: Adapter analizy zapytan do pamieci - uzywa bielik-1.5b do decyzji
//       czy odpytac baze wiedzy. Zwraca should_query i search_terms.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::memory_analyzer::MemoryAnalyzer;
use crate::routing::service_manager::ServiceManager;

pub struct MemoryAnalyzerAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl MemoryAnalyzerAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }
}

impl NodeAdapter for MemoryAnalyzerAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let mode = node_config
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("query_analysis");

        let analyzer = MemoryAnalyzer::new(self.service_manager.clone(), None);

        // Zbierz kontekst sesji z node_results
        let session_type = ctx.node_results.values()
            .find_map(|v| v.get("session_type").and_then(|s| s.as_str()))
            .unwrap_or("unknown")
            .to_string();

        let person_id = ctx.person_id.clone();

        info!(
            mode = mode,
            input_len = ctx.input.len(),
            "MemoryAnalyzer: analizuje zapytanie"
        );

        match mode {
            "query_analysis" => {
                match analyzer.analyze_query(&ctx.input, Some(&session_type), person_id.as_deref()).await {
                    Ok(decision) => {
                        let search_terms: Vec<Value> = decision.search_terms
                            .iter()
                            .map(|t| Value::String(t.clone()))
                            .collect();

                        let query_type = format!("{:?}", decision.query_type);

                        debug!(
                            should_query = decision.should_query,
                            query_type = %query_type,
                            terms_count = search_terms.len(),
                            "MemoryAnalyzer: decyzja"
                        );

                        Ok(serde_json::json!({
                            "text": ctx.input,
                            "should_query": decision.should_query,
                            "query_type": query_type,
                            "search_terms": search_terms,
                        }))
                    }
                    Err(e) => {
                        warn!("MemoryAnalyzer: blad analizy: {}, domyslnie skip", e);
                        Ok(serde_json::json!({
                            "text": ctx.input,
                            "should_query": false,
                            "query_type": "None",
                            "search_terms": [],
                            "error": e.to_string(),
                        }))
                    }
                }
            }
            _ => {
                Ok(serde_json::json!({
                    "text": ctx.input,
                    "should_query": false,
                    "query_type": "None",
                    "search_terms": [],
                }))
            }
        }
    }

    fn node_type(&self) -> &'static str {
        "memory_analyzer"
    }
}
