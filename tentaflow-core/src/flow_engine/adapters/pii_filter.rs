// =============================================================================
// Plik: flow_engine/adapters/pii_filter.rs
// Opis: Adapter wezla pii_filter — pobiera aktywne reguly regex z DB
//       (tabela pii_rules) i aplikuje je na tekscie z kontekstu flow.
//       Aktualizuje rowniez ostatnia wiadomosc user w ctx.messages.
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

/// Aplikuje wszystkie aktywne reguly PII na tekscie wejsciowym node'a i
/// zwraca output JSON z przefiltrowanym tekstem. Modyfikuje rowniez
/// `ctx.input` oraz ostatnia wiadomosc user w `ctx.messages`, zeby kolejne
/// wezly LLM widzialy juz przefiltrowany tekst.
pub async fn apply_pii_filter(
    db: &DbPool,
    node: &FlowNode,
    ctx: &mut FlowContext,
) -> Result<Value> {
    let db_clone = db.clone();
    let rules =
        tokio::task::spawn_blocking(move || repository::list_pii_rules_active(&db_clone)).await??;
    let mut text = resolve_passthrough_text(node, ctx);

    for rule in &rules {
        match RegexBuilder::new(&rule.pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
        {
            Ok(re) => {
                let replaced = re.replace_all(&text, rule.replacement.as_str());
                if let std::borrow::Cow::Owned(new_text) = replaced {
                    text = new_text;
                    debug!(
                        rule_name = %rule.name,
                        category = %rule.category,
                        "PII filter: zastosowano regule"
                    );
                }
            }
            Err(e) => {
                warn!(
                    rule_id = rule.id,
                    rule_name = %rule.name,
                    pattern = %rule.pattern,
                    error = %e,
                    "PII filter: niepoprawny regex w regule"
                );
            }
        }
    }

    if !ctx.messages.is_empty() {
        if let Some(last_user_idx) = ctx
            .messages
            .iter()
            .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        {
            ctx.messages[last_user_idx] = serde_json::json!({
                "role": "user",
                "content": &text,
            });
        }
    }

    ctx.input = text.clone();

    Ok(serde_json::json!({
        "type": "pii_filtered",
        "text": text,
        "rules_applied": rules.len(),
    }))
}

pub struct PiiFilterNodeAdapter {
    db: DbPool,
}

impl PiiFilterNodeAdapter {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl NodeAdapter for PiiFilterNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let node = FlowNode {
            id: String::new(),
            node_type: "pii_filter".to_string(),
            config: node_config.clone(),
            position: None,
            label: None,
        };
        apply_pii_filter(&self.db, &node, ctx).await
    }

    fn node_type(&self) -> &'static str {
        "pii_filter"
    }
}
