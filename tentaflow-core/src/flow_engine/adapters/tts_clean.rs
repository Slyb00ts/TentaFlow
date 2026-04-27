// =============================================================================
// Plik: flow_engine/adapters/tts_clean.rs
// Opis: Adapter wezla tts_clean — czysci tekst przed synteza mowy. Pobiera
//       aktywne reguly z tabeli tts_cleaning_rules (skroty, emoji,
//       fonetyka) i aplikuje je sekwencyjnie wedlug priorytetu.
// =============================================================================

use anyhow::Result;
use serde_json::Value;

use crate::db::DbPool;
use crate::flow_engine::adapters::output::resolve_passthrough_text;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::{FlowContext, FlowNode};

/// Aplikuje reguly czyszczenia TTS na tekscie wejsciowym. Deleguje do
/// `crate::tts::clean_cache::clean` ktore: (1) stripuje emoji, (2) czyta
/// reguly z cache (lazy-load + refresh przy CRUD na tts_cleaning_rules).
/// Wczesniej kazdy run flow robil DB hit + kompilacje regexow per request.
pub async fn apply_tts_clean(db: &DbPool, node: &FlowNode, ctx: &FlowContext) -> Result<Value> {
    let raw = resolve_passthrough_text(node, ctx);
    let text = crate::tts::clean_cache::clean(&raw, db);

    Ok(serde_json::json!({
        "type": "tts_cleaned",
        "text": text,
    }))
}

pub struct TtsCleanNodeAdapter {
    db: DbPool,
}

impl TtsCleanNodeAdapter {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl NodeAdapter for TtsCleanNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let node = FlowNode {
            id: String::new(),
            node_type: "tts_clean".to_string(),
            config: node_config.clone(),
            position: None,
            label: None,
        };
        apply_tts_clean(&self.db, &node, ctx).await
    }

    fn node_type(&self) -> &'static str {
        "tts_clean"
    }
}
