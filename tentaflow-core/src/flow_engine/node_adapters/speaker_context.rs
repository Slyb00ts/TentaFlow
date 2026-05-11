// =============================================================================
// Plik: flow_engine/node_adapters/speaker_context.rs
// Opis: SpeakerContextNodeAdapter — personalizuje rozmowę na podstawie
//       informacji o mowcy. Speaker ID / confidence / name pochodzą z
//       envelope.meta (wstrzykiwane przez upstream STT/diarization). Per
//       confidence + first-message, dopisuje prompt z PromptStore do
//       envelope.context.system_prompts. Brak prompt_id w node config dla
//       danej gałęzi → passthrough (plan: flow_json definiuje zachowanie).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashMap;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "speaker_context";
const DEFAULT_HIGH: f32 = 0.85;
const DEFAULT_MEDIUM: f32 = 0.60;

pub struct SpeakerContextNodeAdapter;

impl SpeakerContextNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_threshold(node: &FlowNode, key: &str, default: f32) -> f32 {
        node.config
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|f| f as f32)
            .unwrap_or(default)
    }

    fn pick_prompt_id<'a>(node: &'a FlowNode, key: &str) -> Option<&'a str> {
        node.config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Wartość z meta jako f32. Brak / nie-liczba → 0.0.
    fn meta_f32(envelope: &FlowEnvelope, key: &str) -> f32 {
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|f| f as f32)
            .unwrap_or(0.0)
    }

    fn meta_string(envelope: &FlowEnvelope, key: &str) -> Option<String> {
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn payload_text(envelope: &FlowEnvelope) -> Option<&str> {
        match &envelope.payload {
            FlowValue::Text(t) => Some(t.as_str()),
            _ => None,
        }
    }

    fn is_introduction(text: &str) -> bool {
        let lower = text.trim().to_lowercase();
        const PREFIXES: &[&str] = &[
            "jestem ",
            "mam na imię ",
            "mam na imie ",
            "nazywam się ",
            "nazywam sie ",
            "moje imię to ",
            "moje imie to ",
        ];
        PREFIXES.iter().any(|p| lower.starts_with(p))
    }

    fn extract_introduction_name(text: &str) -> Option<String> {
        let lower = text.trim().to_lowercase();
        const PREFIXES: &[&str] = &[
            "jestem ",
            "mam na imię ",
            "mam na imie ",
            "nazywam się ",
            "nazywam sie ",
            "moje imię to ",
            "moje imie to ",
        ];
        let prefix = PREFIXES.iter().find(|p| lower.starts_with(*p))?;
        let after = &text.trim()[prefix.len()..];
        let name = after
            .split_whitespace()
            .next()?
            .trim_end_matches(|c: char| c.is_ascii_punctuation())
            .to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
}

impl Default for SpeakerContextNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for SpeakerContextNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("speaker_context adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let high = Self::pick_threshold(node, "high_threshold", DEFAULT_HIGH);
        let medium = Self::pick_threshold(node, "medium_threshold", DEFAULT_MEDIUM);

        let confidence = Self::meta_f32(envelope, "speaker_confidence");
        let person_id = Self::meta_string(envelope, "person_id");
        let speaker_name = Self::meta_string(envelope, "speaker_name");
        let is_first = envelope.context.messages.is_empty();

        // Wybór gałęzi zgodny z legacy `speaker_context` po mapowaniu na
        // narrow trait — bez ServiceManager, bez node_results.
        let (prompt_id_key, vars): (Option<&str>, Vec<(&str, String)>) =
            if confidence >= medium && person_id.is_some() {
                let name = speaker_name.unwrap_or_else(|| "Nieznany".to_string());
                if confidence >= high {
                    let key = if is_first {
                        "personalization_first_prompt"
                    } else {
                        "personalization_continue_prompt"
                    };
                    (
                        Self::pick_prompt_id(node, key),
                        vec![("name", name)],
                    )
                } else {
                    (
                        Self::pick_prompt_id(node, "medium_confidence_known_prompt"),
                        vec![("name", name)],
                    )
                }
            } else if let Some(text) = Self::payload_text(envelope) {
                if Self::is_introduction(text) {
                    if let Some(name) = Self::extract_introduction_name(text) {
                        (
                            Self::pick_prompt_id(node, "new_speaker_prompt"),
                            vec![("name", name)],
                        )
                    } else {
                        (None, Vec::new())
                    }
                } else if confidence >= medium {
                    (
                        Self::pick_prompt_id(node, "medium_confidence_unknown_prompt"),
                        Vec::new(),
                    )
                } else if !is_first {
                    (
                        Self::pick_prompt_id(node, "new_voice_prompt"),
                        Vec::new(),
                    )
                } else {
                    (
                        Self::pick_prompt_id(node, "unknown_user_prompt"),
                        Vec::new(),
                    )
                }
            } else {
                (None, Vec::new())
            };

        let mut out: FlowEnvelope = (**envelope).clone();
        if let Some(pid) = prompt_id_key {
            if let Some(template) = ctx.prompts.get_prompt(pid, None).await? {
                if !template.is_empty() {
                    let map: HashMap<&str, String> = vars.into_iter().collect();
                    let resolved = render_template(&template, &map);
                    out.context.system_prompts.push(resolved);
                }
            }
        }
        Ok(out)
    }
}

/// Prosty placeholder render: `{key}` → wartość z mapy. Brak zaawansowanej
/// składni — zgodne z legacy `resolve_prompt`.
fn render_template(tpl: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::PromptStore;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "sp1".into(),
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

    struct FakePrompts(HashMap<String, String>);
    #[async_trait]
    impl PromptStore for FakePrompts {
        async fn get_prompt(&self, key: &str, _: Option<&str>) -> Result<Option<String>> {
            Ok(self.0.get(key).cloned())
        }
    }

    #[tokio::test]
    async fn high_confidence_first_message_renders_name() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hi".into());
        env.meta.insert("speaker_confidence".into(), json!(0.9));
        env.meta.insert("person_id".into(), json!("p1"));
        env.meta.insert("speaker_name".into(), json!("Anna"));
        let mut ctx = stub_ctx();
        ctx.prompts = Arc::new(FakePrompts(HashMap::from([(
            "first".to_string(),
            "Hi {name}!".to_string(),
        )])));
        let out = SpeakerContextNodeAdapter::new()
            .execute(
                &node(json!({"personalization_first_prompt": "first"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts, vec!["Hi Anna!".to_string()]);
    }

    #[tokio::test]
    async fn introduction_extracts_name_for_new_speaker_prompt() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Jestem Piotr".into());
        let mut ctx = stub_ctx();
        ctx.prompts = Arc::new(FakePrompts(HashMap::from([(
            "intro".to_string(),
            "Welcome {name}".to_string(),
        )])));
        let out = SpeakerContextNodeAdapter::new()
            .execute(
                &node(json!({"new_speaker_prompt": "intro"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.context.system_prompts, vec!["Welcome Piotr".to_string()]);
    }

    #[tokio::test]
    async fn no_prompt_id_in_config_is_passthrough() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hi".into());
        let ctx = stub_ctx();
        let out = SpeakerContextNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert!(out.context.system_prompts.is_empty());
    }
}
