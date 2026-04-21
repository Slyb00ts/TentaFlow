// =============================================================================
// Plik: flow_engine/adapters/speaker_context.rs
// Opis: Adapter rozpoznawania mowcy - identyfikacja glosu, personalizacja,
//       obsluga nieznanego uzytkownika. Wstrzykuje kontekst osoby do messages.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info};

use crate::config::RouterConfig;
use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;
use crate::routing::service_manager::ServiceManager;

pub struct SpeakerContextAdapter {
    service_manager: Arc<ServiceManager>,
    #[allow(dead_code)]
    config: Arc<RouterConfig>,
}

impl SpeakerContextAdapter {
    pub fn new(service_manager: Arc<ServiceManager>, config: Arc<RouterConfig>) -> Self {
        Self {
            service_manager,
            config,
        }
    }

    /// Pobierz tresc promptu z rejestru i podstaw zmienne
    fn resolve_prompt(&self, prompt_id: &str, vars: &[(&str, &str)]) -> Option<String> {
        let content = self
            .service_manager
            .prompt_registry
            .get_content(prompt_id)?;
        let mut result = content.to_string();
        for (key, value) in vars {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        Some(result)
    }

    /// Dopisz suffix do system message w ctx.messages
    fn append_to_system_message(messages: &mut Vec<Value>, suffix: &str) {
        if messages.is_empty() || suffix.is_empty() {
            return;
        }
        if let Some(first_msg) = messages.first_mut() {
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

    /// Heurystyka: czy wiadomosc to przedstawienie sie
    fn is_introduction(text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.starts_with("jestem ")
            || lower.starts_with("mam na imię ")
            || lower.starts_with("mam na imie ")
            || lower.starts_with("nazywam się ")
            || lower.starts_with("nazywam sie ")
            || lower.starts_with("moje imię to ")
            || lower.starts_with("moje imie to ")
    }

    /// Heurystyka: czy wiadomosc jest szumem
    fn is_noise(text: &str) -> bool {
        let trimmed = text.trim();
        trimmed.len() < 3
            || trimmed
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_whitespace())
    }
}

impl NodeAdapter for SpeakerContextAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let high_threshold = node_config
            .get("high_threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.85) as f32;
        let medium_threshold = node_config
            .get("medium_threshold")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.60) as f32;

        let person_id = ctx.person_id.clone();
        let confidence = ctx.speaker_confidence;
        let speaker_name = ctx.speaker_name.clone();

        let is_first_message = ctx
            .node_results
            .values()
            .find_map(|v| v.get("is_first_message").and_then(|f| f.as_bool()))
            .unwrap_or(true);

        let confidence_level = if confidence >= high_threshold {
            "high"
        } else if confidence >= medium_threshold {
            "medium"
        } else if confidence > 0.0 {
            "low"
        } else {
            "none"
        };

        let mut recognized = false;
        let mut person_name = String::new();

        if confidence >= medium_threshold && person_id.is_some() {
            recognized = true;
            let name = speaker_name
                .clone()
                .unwrap_or_else(|| "Nieznany".to_string());
            person_name = name.clone();

            if confidence >= high_threshold {
                let prompt_id = if is_first_message {
                    node_config
                        .get("personalization_first_prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("personalization_first_template")
                } else {
                    node_config
                        .get("personalization_continue_prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("personalization_continue_template")
                };

                if let Some(suffix) = self.resolve_prompt(prompt_id, &[("name", &name)]) {
                    Self::append_to_system_message(&mut ctx.messages, &suffix);
                }
            } else {
                let prompt_id = node_config
                    .get("medium_confidence_known_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("medium_confidence_known_template");

                if let Some(suffix) = self.resolve_prompt(prompt_id, &[("name", &name)]) {
                    Self::append_to_system_message(&mut ctx.messages, &suffix);
                }
            }
        } else {
            if Self::is_noise(&ctx.input) {
                debug!("SpeakerContext: szum, pomijam");
            } else if Self::is_introduction(&ctx.input) {
                let prompt_id = node_config
                    .get("new_speaker_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("new_speaker_introduced_template");

                let extracted_name = ctx
                    .input
                    .trim()
                    .trim_start_matches("jestem ")
                    .trim_start_matches("Jestem ")
                    .trim_start_matches("mam na imię ")
                    .trim_start_matches("Mam na imię ")
                    .trim_start_matches("mam na imie ")
                    .trim_start_matches("nazywam się ")
                    .trim_start_matches("Nazywam się ")
                    .trim_start_matches("nazywam sie ")
                    .trim_start_matches("moje imię to ")
                    .trim_start_matches("moje imie to ")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(|c: char| c.is_ascii_punctuation());

                if !extracted_name.is_empty() {
                    person_name = extracted_name.to_string();
                    if let Some(suffix) =
                        self.resolve_prompt(prompt_id, &[("name", extracted_name)])
                    {
                        Self::append_to_system_message(&mut ctx.messages, &suffix);
                    }
                }
            } else if confidence >= medium_threshold {
                let prompt_id = node_config
                    .get("medium_confidence_unknown_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("medium_confidence_unknown");

                if let Some(suffix) = self.resolve_prompt(prompt_id, &[]) {
                    Self::append_to_system_message(&mut ctx.messages, &suffix);
                }
            } else if !is_first_message {
                let prompt_id = node_config
                    .get("new_voice_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("new_voice_during_conversation");

                if let Some(suffix) = self.resolve_prompt(prompt_id, &[]) {
                    Self::append_to_system_message(&mut ctx.messages, &suffix);
                }
            } else {
                let prompt_id = node_config
                    .get("unknown_user_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown_user_strong");

                if let Some(suffix) = self.resolve_prompt(prompt_id, &[]) {
                    Self::append_to_system_message(&mut ctx.messages, &suffix);
                }
            }
        }

        info!(
            recognized = recognized,
            person_name = %person_name,
            confidence_level = confidence_level,
            confidence = confidence,
            "SpeakerContext: przetworzono kontekst mowcy"
        );

        Ok(serde_json::json!({
            "recognized": recognized,
            "person_name": person_name,
            "confidence_level": confidence_level,
            "confidence": confidence,
        }))
    }

    fn node_type(&self) -> &'static str {
        "speaker_context"
    }
}
