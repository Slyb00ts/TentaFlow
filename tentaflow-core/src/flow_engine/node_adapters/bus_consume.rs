// ===== File: flow_engine/node_adapters/bus_consume.rs —
// BusConsumeNodeAdapter (node_type "bus_consume", category "trigger"). An
// EVENT entry, third member of R5's mutually-exclusive entry set alongside
// `trigger` (request-driven) and `on_subagent_complete` (sub-agent event-
// driven): a flow with this entry runs REACTIVELY when `bus::reactor` fetches
// a batch on the subscribed (topic, group). Like `on_subagent_complete`, the
// adapter itself is a dumb passthrough — no incoming edges, emits a clone of
// `ctx.initial_envelope` — because the REAL seeding (JSON-decoding the
// fetched record(s), building `meta`/`artifacts["meta"]`) happens once in the
// reactor, not per flow. The node's `config` carries the subscription itself
// (instance_id/topic/group/batch_size/max_wait_ms/commit_mode/on_error/
// org_id), which `bus::reactor` parses via `ConsumeConfig::from_config` to
// build its per-flow consume loop. PLAN.md §6.3 (M3a); `instance_id` added
// by plan-app-platform §3.3/§3.5 (multi-instance TentaBus). =====

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::bus::groups::CommitMode;
use crate::bus::instance::BusInstanceId;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::org::DEFAULT_ORG_ID;

pub const NODE_TYPE: &str = "bus_consume";

/// `batch_size` is clamped to this range (PLAN §6.3: "1–1000, domyślnie 1").
pub const MIN_BATCH_SIZE: u32 = 1;
pub const MAX_BATCH_SIZE: u32 = 1000;
const DEFAULT_BATCH_SIZE: u32 = 1;

/// Long-poll bound `bus::reactor` passes to `ConsumerHandle::fetch` on every
/// cycle. Not in PLAN's config field list as a hard-coded default — kept
/// configurable per node since a low-latency flow and a bulk-batch flow want
/// very different poll cadences.
const DEFAULT_MAX_WAIT_MS: u32 = 1000;
const MAX_MAX_WAIT_MS: u32 = 60_000;

pub struct BusConsumeNodeAdapter;

impl BusConsumeNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BusConsumeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// What `bus::reactor` does when a fetched batch fails to dispatch (the flow
/// run errors, or — before dispatch even happens — the record's payload is
/// not valid JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Default: route through `BusService::note_delivery_failure` per record
    /// (`DlqReason::ConsumerError`/`SchemaViolation`) — that call already
    /// implements retry-until-`max_delivery_attempts`-then-DLQ with an
    /// offset auto-advance on escalation (mod.rs's `note_delivery_failure`,
    /// designed for exactly this call site in M1, never invoked in
    /// production until M3a).
    Dlq,
    /// Commit through the batch's end offset without a DLQ entry — the data
    /// is dropped, only a warning is logged. No delivery-attempt tracking.
    Skip,
    /// Do not commit; stop this flow's consume loop entirely. An operator
    /// must fix and republish the flow — `bus::reactor`'s subscription
    /// registry rebuilds on the next flow-set signature change (a save bumps
    /// `flows.version`), which restarts the loop.
    Halt,
}

impl OnError {
    pub fn as_str(self) -> &'static str {
        match self {
            OnError::Dlq => "dlq",
            OnError::Skip => "skip",
            OnError::Halt => "halt",
        }
    }

    fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "dlq" => Some(OnError::Dlq),
            "skip" => Some(OnError::Skip),
            "halt" => Some(OnError::Halt),
            _ => None,
        }
    }
}

impl Default for OnError {
    fn default() -> Self {
        OnError::Dlq
    }
}

/// One `bus_consume` node's parsed subscription. `bus::reactor` builds one
/// background consume loop per (flow_id, this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumeConfig {
    /// TentaBus instance this subscription reads from (plan-app-platform
    /// §3.3). REQUIRED — no fallback, unlike `org_id` below: an instance has
    /// no sensible default, and defaulting it would silently point a
    /// subscription at whichever instance happens to be the only one
    /// running, exactly the cross-instance data leak this field exists to
    /// close (see `bus::reactor::subscription_loop`).
    pub instance_id: BusInstanceId,
    pub org_id: String,
    pub topic: String,
    pub group: String,
    pub batch_size: u32,
    pub max_wait_ms: u32,
    pub commit_mode: CommitMode,
    pub on_error: OnError,
}

impl ConsumeConfig {
    /// Parses a `bus_consume` node's `config`. `topic` and `group` are
    /// required (a subscription addressing nothing, or committing offsets
    /// under no group, is a configuration mistake, not a wildcard) — every
    /// other field defaults.
    ///
    /// `org_id` is NOT in PLAN §6.3's literal field list — added because
    /// `DbFlow` carries no org scope (flows are a global resource, unlike
    /// topics, which ARE org-scoped) and a subscription cannot address a
    /// topic without one. Defaults to `DEFAULT_ORG_ID` so a single-tenant
    /// deployment (the CMC/ŚUM starting shape) never has to set it.
    pub fn from_config(config: &serde_json::Value) -> Result<Self> {
        let instance_id = config
            .get("instance_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("bus_consume requires a non-empty 'instance_id'"))?;
        let instance_id = BusInstanceId::parse(instance_id)?;
        let topic = config
            .get("topic")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("bus_consume requires a non-empty 'topic'"))?
            .to_string();
        let group = config
            .get("group")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("bus_consume requires a non-empty 'group'"))?
            .to_string();
        let org_id = config
            .get("org_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_ORG_ID)
            .to_string();
        let batch_size = config
            .get("batch_size")
            .and_then(|v| v.as_u64())
            .map(|n| (n as u32).clamp(MIN_BATCH_SIZE, MAX_BATCH_SIZE))
            .unwrap_or(DEFAULT_BATCH_SIZE);
        let max_wait_ms = config
            .get("max_wait_ms")
            .and_then(|v| v.as_u64())
            .map(|n| (n as u32).min(MAX_MAX_WAIT_MS))
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_WAIT_MS);
        let commit_mode = match config.get("commit_mode").and_then(|v| v.as_str()) {
            Some("explicit") => CommitMode::Explicit,
            Some("at_most_once") => CommitMode::AtMostOnce,
            Some("auto_after_success") | None => CommitMode::AutoAfterSuccess,
            Some(other) => {
                return Err(anyhow!("bus_consume: unknown commit_mode '{other}'"));
            }
        };
        let on_error = match config.get("on_error").and_then(|v| v.as_str()) {
            Some(s) => OnError::from_config_str(s)
                .ok_or_else(|| anyhow!("bus_consume: unknown on_error '{s}'"))?,
            None => OnError::default(),
        };
        Ok(Self {
            instance_id,
            org_id,
            topic,
            group,
            batch_size,
            max_wait_ms,
            commit_mode,
            on_error,
        })
    }
}

#[async_trait]
impl NodeAdapter for BusConsumeNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        Vec::new()
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("message", FlowDataType::Json),
            PortSpec::new("batch", FlowDataType::Json),
            PortSpec::new("meta", FlowDataType::Json),
        ]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        if !inputs.is_empty() {
            return Err(anyhow!(
                "bus_consume node must not have incoming edges (got {})",
                inputs.len()
            ));
        }
        Ok((*ctx.initial_envelope).clone())
    }

    /// "meta" is always live (delivery metadata is always available once
    /// this node runs at all); exactly one of "message" (`batch_size == 1`,
    /// a JSON object payload) / "batch" (a JSON array payload) is live,
    /// determined by the SEEDED payload's own shape rather than re-reading
    /// the node's static config — self-consistent even if the reactor and
    /// this adapter ever parsed a default differently.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let mut ports = HashSet::from(["meta".to_string()]);
        if let FlowValue::Json(v) = &result.payload {
            ports.insert(if v.is_array() { "batch" } else { "message" }.to_string());
        }
        Some(ports)
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
            id: "bc-1".into(),
            node_type: NODE_TYPE.into(),
            config: json!({"topic": "orders.created", "group": "billing"}),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn emits_clone_of_seeded_envelope() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(json!({"id": 1})));
        let ctx = stub_ctx_with_initial(env);
        let out = BusConsumeNodeAdapter::new()
            .execute(&node(), &[], &ctx)
            .await
            .unwrap();
        match &out.payload {
            FlowValue::Json(v) => assert_eq!(v, &json!({"id": 1})),
            other => panic!("expected Json payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_incoming_edges() {
        let inputs = vec![NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(FlowEnvelope::empty()),
        }];
        let ctx = stub_ctx_with_initial(FlowEnvelope::empty());
        let err = BusConsumeNodeAdapter::new()
            .execute(&node(), &inputs, &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not have incoming edges"));
    }

    const TEST_INSTANCE_ID: &str = "tentabus-aaaaaaaa";

    #[test]
    fn config_requires_topic_and_group() {
        assert!(ConsumeConfig::from_config(&json!({"instance_id": TEST_INSTANCE_ID})).is_err());
        assert!(ConsumeConfig::from_config(
            &json!({"instance_id": TEST_INSTANCE_ID, "topic": "t"})
        )
        .is_err());
        assert!(ConsumeConfig::from_config(
            &json!({"instance_id": TEST_INSTANCE_ID, "group": "g"})
        )
        .is_err());
        assert!(ConsumeConfig::from_config(
            &json!({"instance_id": TEST_INSTANCE_ID, "topic": "t", "group": "g"})
        )
        .is_ok());
    }

    /// plan-app-platform §3.3: `instance_id` is REQUIRED with no fallback,
    /// unlike `org_id` (which defaults to `DEFAULT_ORG_ID`). Missing,
    /// malformed and foreign-package ids must all be rejected.
    #[test]
    fn config_requires_instance_id() {
        let err = ConsumeConfig::from_config(&json!({"topic": "t", "group": "g"})).unwrap_err();
        assert!(err.to_string().contains("instance_id"));

        let err =
            ConsumeConfig::from_config(&json!({"instance_id": "", "topic": "t", "group": "g"}))
                .unwrap_err();
        assert!(err.to_string().contains("instance_id"));

        let err = ConsumeConfig::from_config(
            &json!({"instance_id": "not-a-valid-id", "topic": "t", "group": "g"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("bus instance id"));

        let ok = ConsumeConfig::from_config(
            &json!({"instance_id": TEST_INSTANCE_ID, "topic": "t", "group": "g"}),
        )
        .unwrap();
        assert_eq!(ok.instance_id.as_str(), TEST_INSTANCE_ID);
    }

    #[test]
    fn config_defaults() {
        let c = ConsumeConfig::from_config(
            &json!({"instance_id": TEST_INSTANCE_ID, "topic": "t", "group": "g"}),
        )
        .unwrap();
        assert_eq!(c.org_id, DEFAULT_ORG_ID);
        assert_eq!(c.batch_size, 1);
        assert_eq!(c.max_wait_ms, DEFAULT_MAX_WAIT_MS);
        assert_eq!(c.commit_mode, CommitMode::AutoAfterSuccess);
        assert_eq!(c.on_error, OnError::Dlq);
    }

    #[test]
    fn config_clamps_batch_size_and_overrides() {
        let c = ConsumeConfig::from_config(&json!({
            "instance_id": TEST_INSTANCE_ID, "topic": "t", "group": "g", "batch_size": 5000,
            "org_id": "org-2", "commit_mode": "explicit", "on_error": "halt",
        }))
        .unwrap();
        assert_eq!(c.batch_size, MAX_BATCH_SIZE);
        assert_eq!(c.org_id, "org-2");
        assert_eq!(c.commit_mode, CommitMode::Explicit);
        assert_eq!(c.on_error, OnError::Halt);
    }

    #[test]
    fn config_rejects_unknown_on_error() {
        let err = ConsumeConfig::from_config(&json!({
            "instance_id": TEST_INSTANCE_ID, "topic": "t", "group": "g", "on_error": "bogus"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("on_error"));
    }

    #[test]
    fn single_message_activates_message_and_meta_ports() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(json!({"id": 1})));
        let ports = BusConsumeNodeAdapter::new()
            .active_output_ports(&node(), &env)
            .unwrap();
        assert_eq!(
            ports,
            HashSet::from(["message".to_string(), "meta".to_string()])
        );
    }

    #[test]
    fn batch_array_activates_batch_and_meta_ports() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(json!([{"id": 1}, {"id": 2}])));
        let ports = BusConsumeNodeAdapter::new()
            .active_output_ports(&node(), &env)
            .unwrap();
        assert_eq!(
            ports,
            HashSet::from(["batch".to_string(), "meta".to_string()])
        );
    }
}
