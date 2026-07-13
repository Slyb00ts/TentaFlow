// ===== File: flow_engine/node_adapters/on_subagent_complete.rs —
// OnSubagentCompleteNodeAdapter (node_type "on_subagent_complete", category
// "trigger"). An EVENT entry: a flow with this entry runs REACTIVELY when a
// sub-agent run settles, instead of on an inbound request like `trigger`. It
// behaves exactly like a trigger at execute time (no input edges, emits a clone
// of `ctx.initial_envelope`); the reactor (`agents::subagent_reactor`) seeds the
// initial envelope with the finished child's result as the payload plus meta
// keys (`child_run_id`, `child_status`, `agent_id`). The node's `config` carries
// the subscription FILTER the reactor matches against — `agent_id` (only react
// to children of that agent) and/or `match_status` (only that terminal status).
// Validation (R5) treats this and `trigger` as the two mutually exclusive entry
// kinds: a flow has exactly one entry, of either kind. (Harness §3.6 phase 4b.)
// =====

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub const NODE_TYPE: &str = "on_subagent_complete";

pub struct OnSubagentCompleteNodeAdapter;

impl OnSubagentCompleteNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OnSubagentCompleteNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// One node's parsed subscription filter. At least one of `agent_id` /
/// `match_status` must be present so the flow does not fan out on EVERY child
/// completion across the process; the reactor rejects an unfiltered node at
/// subscription-build time. `match_status` defaults to `completed` when only
/// `agent_id` is given (a reactive flow almost always wants successful results,
/// not failures/cancellations) — but an explicit `match_status` overrides that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionFilter {
    pub agent_id: Option<String>,
    pub match_status: Option<String>,
}

impl CompletionFilter {
    /// Parses the filter from a node's `config`. Returns an error when neither
    /// `agent_id` nor `match_status` is set (an unfiltered subscription is a
    /// configuration mistake, not a wildcard). An empty string is treated as
    /// absent.
    pub fn from_config(config: &serde_json::Value) -> Result<Self> {
        let agent_id = config
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let match_status = config
            .get("match_status")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if agent_id.is_none() && match_status.is_none() {
            return Err(anyhow!(
                "on_subagent_complete requires at least one of 'agent_id' or 'match_status' \
                 (an unfiltered trigger would fire on every child completion)"
            ));
        }
        Ok(Self {
            agent_id,
            match_status,
        })
    }

    /// The status this filter reacts to. Explicit `match_status` wins; otherwise
    /// the default is `completed` (success-only reactive flows).
    pub fn effective_status(&self) -> &str {
        self.match_status.as_deref().unwrap_or("completed")
    }

    /// Whether a settled child (its agent + terminal status) matches this filter.
    pub fn matches(&self, child_agent_id: &str, child_status: &str) -> bool {
        if let Some(want) = &self.agent_id {
            if want != child_agent_id {
                return false;
            }
        }
        self.effective_status() == child_status
    }
}

#[async_trait]
impl NodeAdapter for OnSubagentCompleteNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        Vec::new()
    }

    // Same six typed output ports as `trigger`: the reactor seeds the result as a
    // single payload (Text/Json) and downstream branches consume their modality.
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("text", FlowDataType::Text),
            PortSpec::new("audio", FlowDataType::Audio),
            PortSpec::new("image", FlowDataType::Image),
            PortSpec::new("video", FlowDataType::Video),
            PortSpec::new("embedding", FlowDataType::Embedding),
            PortSpec::new("other", FlowDataType::Other),
        ]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        // An entry node — like `trigger`, it must not have inbound edges.
        if !inputs.is_empty() {
            return Err(anyhow!(
                "on_subagent_complete node must not have incoming edges (got {})",
                inputs.len()
            ));
        }
        Ok((*ctx.initial_envelope).clone())
    }

    /// Modality gating mirrors `trigger`: only the port matching the seeded
    /// payload's modality is active (the reactor seeds Text/Json, so the `text`
    /// branch is the live one). `None` = no modality (empty seed) → all ports.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        modality_port(&result.payload).map(|p| HashSet::from([p.to_string()]))
    }
}

/// Maps a `FlowValue` variant to the output port name. `Json` rides the `text`
/// channel (structured data consumed by text branches). `Empty` carries no
/// modality.
fn modality_port(value: &FlowValue) -> Option<&'static str> {
    match value {
        FlowValue::Empty => None,
        FlowValue::Text(_) | FlowValue::Json(_) => Some("text"),
        FlowValue::Audio { .. } => Some("audio"),
        FlowValue::Image { .. } => Some("image"),
        FlowValue::Video { .. } => Some("video"),
        FlowValue::Embedding(_) => Some("embedding"),
        FlowValue::Other { .. } => Some("other"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx_with_initial;
    use serde_json::json;
    use std::sync::Arc;

    fn node() -> FlowNode {
        FlowNode {
            id: "evt-1".into(),
            node_type: NODE_TYPE.into(),
            config: json!({"agent_id": "a1", "match_status": "completed"}),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn emits_clone_of_seeded_envelope() {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("child result".into()));
        env.meta.insert("child_run_id".into(), json!("run-9"));
        let ctx = stub_ctx_with_initial(env);
        let out = OnSubagentCompleteNodeAdapter::new()
            .execute(&node(), &[], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("child result"));
        assert_eq!(
            out.meta.get("child_run_id").and_then(|v| v.as_str()),
            Some("run-9")
        );
    }

    #[tokio::test]
    async fn rejects_incoming_edges() {
        let inputs = vec![NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(FlowEnvelope::empty()),
        }];
        let ctx = stub_ctx_with_initial(FlowEnvelope::empty());
        let err = OnSubagentCompleteNodeAdapter::new()
            .execute(&node(), &inputs, &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not have incoming edges"));
    }

    #[test]
    fn filter_requires_agent_or_status() {
        assert!(CompletionFilter::from_config(&json!({})).is_err());
        assert!(CompletionFilter::from_config(&json!({"agent_id": ""})).is_err());
        assert!(CompletionFilter::from_config(&json!({"agent_id": "a1"})).is_ok());
        assert!(CompletionFilter::from_config(&json!({"match_status": "failed"})).is_ok());
    }

    #[test]
    fn filter_defaults_status_to_completed() {
        let f = CompletionFilter::from_config(&json!({"agent_id": "a1"})).unwrap();
        assert_eq!(f.effective_status(), "completed");
        assert!(f.matches("a1", "completed"));
        assert!(!f.matches("a1", "failed"));
        assert!(!f.matches("a2", "completed"));
    }

    #[test]
    fn filter_matches_explicit_status_and_any_agent() {
        // match_status only: any agent with that status matches.
        let f = CompletionFilter::from_config(&json!({"match_status": "failed"})).unwrap();
        assert!(f.matches("a1", "failed"));
        assert!(f.matches("zzz", "failed"));
        assert!(!f.matches("a1", "completed"));
    }

    #[test]
    fn text_payload_activates_only_text_port() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        let ports = OnSubagentCompleteNodeAdapter::new()
            .active_output_ports(&node(), &env)
            .unwrap();
        assert_eq!(ports, HashSet::from(["text".to_string()]));
    }

    #[test]
    fn empty_payload_does_not_gate() {
        let env = FlowEnvelope::empty();
        assert_eq!(
            OnSubagentCompleteNodeAdapter::new().active_output_ports(&node(), &env),
            None
        );
    }
}
