// ===== File: flow_engine/node_adapters/interval.rs — IntervalNodeAdapter
// (node_type "interval", category transform, 1-in/1-out). A time gate: it sleeps
// for the configured number of seconds, then passes the envelope through
// unchanged. The sleep is interruptible — it races the run's cancel token and the
// effective deadline (the human-wait-extended deadline, §3.13), so a cancelled or
// expired run returns immediately instead of parking. Built to pace a polling
// watch region ("subagent_status → interval → loop_back") so a flow checks child
// progress periodically without a busy loop. (Harness §3.4.) =====

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "interval";
const DEFAULT_SECONDS: f64 = 10.0;
/// Upper bound on a single gate so a misconfigured flow cannot park a run for
/// hours; longer pacing is expressed by looping the region, not one long sleep.
const MAX_SECONDS: f64 = 3600.0;

pub struct IntervalNodeAdapter;

impl IntervalNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Sleep duration in seconds. Accepts an integer or fractional `seconds`
    /// value; non-positive / missing falls back to the default, and the value is
    /// clamped to `MAX_SECONDS`.
    fn duration(node: &FlowNode) -> Duration {
        let secs = node
            .config
            .get("seconds")
            .and_then(|v| v.as_f64())
            .filter(|n| *n > 0.0)
            .unwrap_or(DEFAULT_SECONDS)
            .min(MAX_SECONDS);
        Duration::from_secs_f64(secs)
    }
}

impl Default for IntervalNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for IntervalNodeAdapter {
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
            .ok_or_else(|| anyhow!("interval: missing input edge"))?;
        let envelope = &input.envelope;

        let mut wait = Self::duration(node);
        // Never sleep past the effective deadline — a region polling on this gate
        // must wake to let the executor's deadline check fire.
        if let Some(deadline) = ctx.effective_deadline() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            wait = wait.min(remaining);
        }

        // Race the sleep against cancellation; a cancelled run returns at once.
        tokio::select! {
            _ = ctx.cancel_token.cancelled() => {}
            _ = tokio::time::sleep(wait) => {}
        }

        Ok((**envelope).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowValue;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Instant;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "iv1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "prev".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn sleeps_then_passes_through() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("carry".into());
        let started = Instant::now();
        let out = IntervalNodeAdapter::new()
            .execute(&node(json!({"seconds": 0.05})), &[input(env)], &stub_ctx())
            .await
            .expect("execute");
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "did not sleep"
        );
        assert_eq!(out.payload.as_text(), Some("carry"));
    }

    #[tokio::test]
    async fn cancel_returns_early() {
        let env = FlowEnvelope::empty();
        let ctx = stub_ctx();
        // Cancel before the long sleep would elapse → returns immediately.
        ctx.cancel_token.cancel();
        let started = Instant::now();
        IntervalNodeAdapter::new()
            .execute(&node(json!({"seconds": 30})), &[input(env)], &ctx)
            .await
            .expect("execute");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancel did not short-circuit the sleep"
        );
    }
}
