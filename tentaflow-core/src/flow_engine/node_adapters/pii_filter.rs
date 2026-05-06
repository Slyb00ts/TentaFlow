// =============================================================================
// Plik: flow_engine/node_adapters/pii_filter.rs
// Opis: PiiFilterNodeAdapter — pobiera aktywne reguły PII z ctx.pii_rules,
//       aplikuje sekwencyjnie regex replace na envelope.payload (jeśli Text).
//       Plan v4.2 D3 — DbPool wycięty z adaptera, regex compile + cache w
//       impl `PiiRulesStore`.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use regex::RegexBuilder;
use tracing::{debug, warn};

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const REGEX_SIZE_LIMIT: usize = 1_000_000;

pub struct PiiFilterNodeAdapter;

impl PiiFilterNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PiiFilterNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

const INPUT_PORTS: &[&str] = &["in"];
const OUTPUT_PORTS: &[&str] = &["full"];

#[async_trait]
impl NodeAdapter for PiiFilterNodeAdapter {
    fn node_type(&self) -> &str {
        "pii_filter"
    }

    fn supported_input_ports(&self) -> &[&'static str] {
        INPUT_PORTS
    }

    fn supported_output_ports(&self) -> &[&'static str] {
        OUTPUT_PORTS
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("pii_filter node requires exactly 1 input edge"))?;

        // Bierzemy text z payload — jeśli payload nie jest Text, pii_filter
        // jest no-op (PII reguły są tekstowe, nie ma sensu próbować na audio
        // czy embeddings). Defensywnie passujemy envelope dalej.
        let mut out = (*input.envelope).clone();
        let mut text = match out.payload {
            FlowValue::Text(ref t) => t.clone(),
            _ => return Ok(out),
        };

        let rules = ctx.pii_rules.active_rules().await?;
        let mut applied = 0u32;
        for rule in &rules {
            match RegexBuilder::new(&rule.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
            {
                Ok(re) => {
                    let replaced = re.replace_all(&text, rule.replacement.as_str());
                    if let std::borrow::Cow::Owned(new_text) = replaced {
                        text = new_text;
                        applied += 1;
                        debug!(
                            rule_name = %rule.name,
                            category = %rule.category,
                            "pii_filter: zastosowano regule"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        rule_id = rule.id,
                        rule_name = %rule.name,
                        pattern = %rule.pattern,
                        error = %e,
                        "pii_filter: niepoprawny regex"
                    );
                }
            }
        }

        // Aktualizujemy też ostatnią User message w context.messages, żeby
        // kolejne LLM nody widziały już przefiltrowany input.
        if let Some(last_user) = out
            .context
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(m.role, crate::flow_engine::envelope::ChatRole::User))
        {
            last_user.content = text.clone();
        }

        out.payload = FlowValue::Text(text);
        out.meta.insert(
            "pii_rules_applied".into(),
            serde_json::json!(applied),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::pii_rules::{PiiRule, PiiRulesStore};
    use crate::flow_engine::envelope::{ChatMessage, ChatRole};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FakePiiRules(Vec<PiiRule>);
    #[async_trait]
    impl PiiRulesStore for FakePiiRules {
        async fn active_rules(&self) -> AnyResult<Vec<PiiRule>> {
            Ok(self.0.clone())
        }
    }

    fn pii_node() -> FlowNode {
        FlowNode {
            id: "pii-1".into(),
            node_type: "pii_filter".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
        }
    }

    fn make_input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "src".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn pii_filter_replaces_email_pattern() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "email".into(),
            category: "contact".into(),
            pattern: r"[a-z]+@[a-z]+\.com".into(),
            replacement: "[EMAIL]".into(),
        }]));

        let env = FlowEnvelope::with_payload(FlowValue::Text(
            "kontakt: foo@bar.com".into(),
        ));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("kontakt: [EMAIL]"));
        assert_eq!(
            out.meta.get("pii_rules_applied").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn pii_filter_updates_last_user_message_in_context() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "email".into(),
            category: "contact".into(),
            pattern: r"[a-z]+@[a-z]+\.com".into(),
            replacement: "[EMAIL]".into(),
        }]));

        let mut env = FlowEnvelope::with_payload(FlowValue::Text("foo@bar.com".into()));
        env.context.messages.push(ChatMessage::system("be helpful"));
        env.context.messages.push(ChatMessage::user("foo@bar.com"));

        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();

        let last_user = out
            .context
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::User))
            .unwrap();
        assert_eq!(last_user.content, "[EMAIL]");
    }

    #[tokio::test]
    async fn pii_filter_no_op_on_non_text_payload() {
        let env = FlowEnvelope::with_payload(FlowValue::Embedding(vec![0.1, 0.2]));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &stub_ctx())
            .await
            .unwrap();
        // Stub zwraca Vec::new() reguł, więc i tak by było no-op; ale tu
        // testujemy że non-Text payload jest passthrough nawet bez patrzenia
        // w reguły — meta nie dostaje pii_rules_applied.
        assert!(matches!(out.payload, FlowValue::Embedding(_)));
        assert!(out.meta.get("pii_rules_applied").is_none());
    }

    #[tokio::test]
    async fn pii_filter_invalid_regex_skipped_with_warning() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "bad".into(),
            category: "x".into(),
            pattern: "[unclosed".into(), // niepoprawny regex
            replacement: "x".into(),
        }]));
        let env = FlowEnvelope::with_payload(FlowValue::Text("payload".into()));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();
        // Original text intact, applied count = 0 (skipped invalid regex).
        assert_eq!(out.payload.as_text(), Some("payload"));
        assert_eq!(
            out.meta.get("pii_rules_applied").and_then(|v| v.as_u64()),
            Some(0)
        );
    }
}
