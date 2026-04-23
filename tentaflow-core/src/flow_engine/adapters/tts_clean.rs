// =============================================================================
// Plik: flow_engine/adapters/tts_clean.rs
// Opis: Adapter wezla tts_clean — czysci tekst przed synteza mowy. Pobiera
//       aktywne reguly z tabeli tts_cleaning_rules (skroty, emoji,
//       fonetyka) i aplikuje je sekwencyjnie wedlug priorytetu.
// =============================================================================

use anyhow::Result;
use regex::RegexBuilder;
use serde_json::Value;
use tracing::{debug, warn};

use crate::db::{repository, DbPool};
use crate::flow_engine::adapters::output::resolve_passthrough_text;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::{FlowContext, FlowNode};

const REGEX_SIZE_LIMIT: usize = 1_000_000;

/// Aplikuje reguly czyszczenia TTS (abbreviation / regex_remove / emoji_range
/// / phonetic) na tekscie wejsciowym i zwraca JSON z wyczyszczonym tekstem.
pub async fn apply_tts_clean(db: &DbPool, node: &FlowNode, ctx: &FlowContext) -> Result<Value> {
    let db_clone = db.clone();
    let rules =
        tokio::task::spawn_blocking(move || repository::list_tts_cleaning_rules_active(&db_clone))
            .await??;
    let mut text = resolve_passthrough_text(node, ctx);

    for rule in &rules {
        match rule.rule_type.as_str() {
            "abbreviation" => {
                if let Some(ref replacement) = rule.replacement {
                    text = text.replace(&rule.pattern, replacement);
                }
            }
            "regex_remove" | "emoji_range" => {
                match RegexBuilder::new(&rule.pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .build()
                {
                    Ok(re) => {
                        let replacement = rule.replacement.as_deref().unwrap_or("");
                        text = re.replace_all(&text, replacement).to_string();
                    }
                    Err(e) => {
                        warn!(
                            rule_id = rule.id,
                            pattern = %rule.pattern,
                            error = %e,
                            "TTS clean: niepoprawny regex"
                        );
                    }
                }
            }
            "phonetic" => {
                if let Some(ref replacement) = rule.replacement {
                    text = text.replace(&rule.pattern, replacement);
                }
            }
            other => {
                debug!(rule_type = other, "TTS clean: nieznany typ reguly");
            }
        }
    }

    Ok(serde_json::json!({
        "type": "tts_cleaned",
        "text": text,
        "rules_applied": rules.len(),
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
