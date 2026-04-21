// =============================================================================
// Plik: flow_engine/adapters/session_context.rs
// Opis: Adapter kontekstu sesji - informuje LLM czy to poczatek rozmowy,
//       kontynuacja czy niezrozumiala wiadomosc. Dopisuje suffix do system prompt.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;

pub struct SessionContextAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl SessionContextAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Heurystyka: czy wiadomosc jest prawdopodobnie szumem/niezrozumiala
    fn is_likely_noise(text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.len() < 3 {
            return true;
        }
        if trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace())
        {
            return true;
        }
        if trimmed
            .chars()
            .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
        {
            return true;
        }
        false
    }
}

impl NodeAdapter for SessionContextAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let first_prompt_id = node_config
            .get("first_prompt_id")
            .and_then(|v| v.as_str())
            .unwrap_or("session_start");
        let continue_prompt_id = node_config
            .get("continue_prompt_id")
            .and_then(|v| v.as_str())
            .unwrap_or("session_continue");
        let unclear_prompt_id = node_config
            .get("unclear_prompt_id")
            .and_then(|v| v.as_str())
            .unwrap_or("session_unclear");

        // Sprawdz czy to pierwsza wiadomosc (z wyniku conversation_history)
        let is_first_message = ctx
            .node_results
            .values()
            .find_map(|v| v.get("is_first_message").and_then(|f| f.as_bool()))
            .unwrap_or(true);

        let is_noise = Self::is_likely_noise(&ctx.input);

        // Wybierz prompt
        let (session_type, prompt_id) = if is_noise && !is_first_message {
            ("unclear", unclear_prompt_id)
        } else if is_first_message {
            ("first", first_prompt_id)
        } else {
            ("continue", continue_prompt_id)
        };

        // Pobierz tresc promptu z rejestru
        let suffix = self
            .service_manager
            .prompt_registry
            .get_content(prompt_id)
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Dopisz suffix do system message w ctx.messages
        if !suffix.is_empty() && !ctx.messages.is_empty() {
            if let Some(first_msg) = ctx.messages.first_mut() {
                if first_msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                    if let Some(content) = first_msg.get("content").and_then(|c| c.as_str()) {
                        let new_content = format!("{}{}", content, suffix);
                        *first_msg = serde_json::json!({
                            "role": "system",
                            "content": new_content,
                        });
                    }
                }
            }
        }

        info!(
            session_type = session_type,
            prompt_id = prompt_id,
            is_first = is_first_message,
            "SessionContext: ustawiono kontekst sesji"
        );

        Ok(serde_json::json!({
            "session_type": session_type,
            "prompt_id": prompt_id,
            "is_first_message": is_first_message,
        }))
    }

    fn node_type(&self) -> &'static str {
        "session_context"
    }
}
