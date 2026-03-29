// =============================================================================
// Plik: flow_engine/adapters/conversation_history.rs
// Opis: Adapter historii konwersacji - wstrzykuje poprzednie wiadomosci
//       do ctx.messages i zapisuje biezaca wiadomosc do cache.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;

pub struct ConversationHistoryAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl ConversationHistoryAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }
}

impl NodeAdapter for ConversationHistoryAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let max_messages = node_config
            .get("max_messages")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;

        let session_id = ctx.session_id.clone().unwrap_or_else(|| "default".to_string());
        let cache = &self.service_manager.conversation_cache;

        // Pobierz historie
        let history = cache.get_history(&session_id).await;
        let is_first_message = history.is_empty();
        let history_count = history.len();

        // Wstrzyknij historie do ctx.messages (po system message, przed ostatnia user)
        if !history.is_empty() && !ctx.messages.is_empty() {
            let limited: Vec<_> = if history.len() > max_messages {
                history[history.len() - max_messages..].to_vec()
            } else {
                history.clone()
            };

            // Znajdz pozycje wstawienia: po pierwszym system message
            let insert_pos: usize = if ctx.messages.first()
                .and_then(|m| m.get("role"))
                .and_then(|r| r.as_str()) == Some("system")
            {
                1
            } else {
                0
            };

            // Konwertuj historie na format messages
            let history_messages: Vec<Value> = limited.iter().map(|msg| {
                serde_json::json!({
                    "role": msg.role,
                    "content": msg.content,
                })
            }).collect();

            // Wstaw przed ostatnia wiadomoscia user
            let last_msg = ctx.messages.pop();
            let mut new_messages = Vec::new();
            for (i, msg) in ctx.messages.drain(..).enumerate() {
                new_messages.push(msg);
                if i == insert_pos.saturating_sub(1) && insert_pos > 0 {
                    new_messages.extend(history_messages.clone());
                }
            }
            if insert_pos == 0 {
                let mut tmp = history_messages;
                tmp.extend(new_messages);
                new_messages = tmp;
            }
            if let Some(last) = last_msg {
                new_messages.push(last);
            }
            ctx.messages = new_messages;
        }

        // Zapisz biezaca wiadomosc user do cache
        if !ctx.input.is_empty() {
            cache.add_message(&session_id, "user", &ctx.input).await;
        }

        info!(
            session_id = %session_id,
            is_first = is_first_message,
            history_count = history_count,
            "ConversationHistory: wstrzyknieto historie"
        );

        Ok(serde_json::json!({
            "is_first_message": is_first_message,
            "history_count": history_count,
            "session_id": session_id,
        }))
    }

    fn node_type(&self) -> &'static str {
        "conversation_history"
    }
}
