// ===== File: flow_engine/node_adapters/bus_publish.rs —
// BusPublishNodeAdapter (node_type "bus_publish", category "output"). A
// sink-ish side-effect node — publishes the inbound envelope's payload as one
// TentaBus record and passes the envelope through unchanged (same shape as
// `persist_turn`/`store`: `in` (Any) -> `full` (Any)), so a flow can keep
// processing after the publish (e.g. `bus_publish -> output`).
//
// Config (PLAN §6.3): `instance_id` (plain string, required — the target
// TentaBus instance, plan-app-platform §3.3; read exactly like `topic`, no
// fallback), `topic` (plain string, required), `key` (CEL, optional
// — partition key), `headers` (object of `{header_name: CEL}`, optional —
// evaluated per-key, NOT the generic executor-level `io_mapping.rs`, which
// only reads the reserved `input_mapping`/`output_mapping` keys),
// `content_type` (optional — folded into a `content-type` header rather than
// a dedicated `PublishRecord` field, since the wire record has none),
// `create_if_missing` (bool, default false — on `TopicNotFound`, calls
// `create_topic` with default options and retries once).
//
// Payload: `expr::flow_value_to_json` on the inbound payload gives a uniform
// JSON projection for EVERY `FlowValue` variant (Text -> string, Json ->
// passthrough, a blob variant -> a small `{kind, mime, size_bytes}`
// descriptor — never bytes, matching PLAN §2.4's "topic carries a manifest,
// not the blob"). A `String` result publishes as raw UTF-8 (no JSON quoting,
// so a Text payload lands as plain text); anything else publishes as its
// JSON-serialized bytes. =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;

use crate::bus::instance::BusInstanceId;
use crate::bus::{self, BusCallContext, PublishBatch, PublishRecord};
use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::expr::{evaluate, flow_value_to_json, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::org::DEFAULT_ORG_ID;

pub const NODE_TYPE: &str = "bus_publish";

pub struct BusPublishNodeAdapter;

impl BusPublishNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BusPublishNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the `BusCallContext` this node publishes under, from the flow run's
/// own provenance — never a fabricated identity (§2.5 discipline: the
/// executing flow's actor/origin/correlation carry straight through, the same
/// way `bus::mod`'s own doc says `BusCallContext` mirrors `ExecutionContext`).
fn call_context(ctx: &ExecutionContext, svc: &bus::BusService) -> BusCallContext {
    BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id: ctx
            .org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string()),
        actor: ctx.actor_id.clone().or_else(|| ctx.actor_user_id.clone()),
        correlation_id: ctx.correlation_id.clone(),
        origin: ctx.origin.as_str().to_string(),
    }
}

/// Evaluates each `headers` config entry (`{name: CEL}`) against the inbound
/// envelope's scope. A `String` result becomes its raw UTF-8 bytes; any other
/// JSON value is serialized. `content_type`, when set, adds/overrides the
/// `content-type` header — evaluated headers win only if a flow author
/// explicitly names `content-type` in `headers` too, since that is a more
/// specific per-call statement than the node's blanket default.
fn build_headers(
    node: &FlowNode,
    scope: &ExprScope,
) -> Result<Vec<(String, Bytes)>> {
    let mut out = Vec::new();
    if let Some(ct) = node
        .config
        .get("content_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.push(("content-type".to_string(), Bytes::from(ct.to_string())));
    }
    if let Some(headers) = node.config.get("headers").and_then(|v| v.as_object()) {
        for (name, expr) in headers {
            let Some(expr) = expr.as_str() else {
                return Err(anyhow!(
                    "bus_publish: headers['{name}'] must be a CEL expression string"
                ));
            };
            let value = evaluate(expr, scope, None)
                .map_err(|e| anyhow!("bus_publish: header '{name}': {e}"))?;
            let bytes = match value {
                serde_json::Value::String(s) => Bytes::from(s),
                other => Bytes::from(serde_json::to_vec(&other)?),
            };
            out.retain(|(k, _)| k != name);
            out.push((name.clone(), bytes));
        }
    }
    Ok(out)
}

/// Evaluates `key` (CEL, optional) the same way `headers` values are: a
/// `String` result is its raw UTF-8 bytes, anything else is JSON-serialized.
fn build_key(node: &FlowNode, scope: &ExprScope) -> Result<Option<Bytes>> {
    let Some(expr) = node
        .config
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let value = evaluate(expr, scope, None).map_err(|e| anyhow!("bus_publish: key: {e}"))?;
    Ok(Some(match value {
        serde_json::Value::String(s) => Bytes::from(s),
        other => Bytes::from(serde_json::to_vec(&other)?),
    }))
}

/// Reads and validates the required `instance_id` config key, exactly like
/// `topic` (plan-app-platform §3.3): trimmed, non-empty, `BusInstanceId::
/// parse`. No fallback — an instance has no sensible default (see
/// `ConsumeConfig::instance_id`'s doc for the same rationale).
fn parse_instance_id(node: &FlowNode) -> Result<BusInstanceId> {
    let raw = node
        .config
        .get("instance_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("bus_publish requires a non-empty 'instance_id'"))?;
    Ok(BusInstanceId::parse(raw)?)
}

fn build_payload(envelope: &FlowEnvelope) -> Result<Bytes> {
    if matches!(envelope.payload, crate::flow_engine::envelope::FlowValue::Empty) {
        return Err(anyhow!("bus_publish: envelope has no payload to publish"));
    }
    Ok(match flow_value_to_json(&envelope.payload) {
        serde_json::Value::String(s) => Bytes::from(s),
        other => Bytes::from(serde_json::to_vec(&other)?),
    })
}

#[async_trait]
impl NodeAdapter for BusPublishNodeAdapter {
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
            .ok_or_else(|| anyhow!("bus_publish node requires exactly 1 input edge"))?;
        let instance_id = parse_instance_id(node)?;
        let topic = node
            .config
            .get("topic")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("bus_publish requires a non-empty 'topic'"))?
            .to_string();
        let create_if_missing = node
            .config
            .get("create_if_missing")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let extras: [(&str, serde_json::Value); 0] = [];
        let scope = ExprScope {
            vars: &input.envelope.variables,
            payload: &input.envelope.payload,
            artifacts: &input.envelope.artifacts,
            meta: &input.envelope.meta,
            extras: &extras,
        };
        let key = build_key(node, &scope)?;
        let headers = build_headers(node, &scope)?;
        let payload = build_payload(&input.envelope)?;

        let svc = bus::instance(&instance_id).ok_or_else(|| {
            anyhow!(
                "bus_publish: bus instance '{instance_id}' is not running (disabled, \
                 uninstalled, or not started yet)"
            )
        })?;
        let bctx = call_context(ctx, &svc);
        let record = PublishRecord {
            key,
            headers,
            payload,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            schema_id: 0,
        };
        let batch = PublishBatch {
            partition: None,
            producer: None,
            records: vec![record],
        };
        // `BusService::publish` (and `create_topic`) block internally
        // (`Partition::append_batch` uses `blocking_recv` on its own
        // channel) — `bus::mod`'s own doc requires every async caller to go
        // through `spawn_blocking`. `execute()` runs on the Tokio runtime via
        // `execute_blocking`'s node scheduler, so calling either directly
        // here panics ("cannot block the current thread from within a
        // runtime") the moment this node actually runs — caught by the P11
        // gate test (`tests/bus_flow_chain_p11_gate.rs`), never exercised
        // live before it.
        let topic_for_task = topic.clone();
        let result = tokio::task::spawn_blocking(move || {
            let topic = topic_for_task;
            match svc.publish(&bctx, &topic, batch.clone()) {
                Ok(r) => Ok(r),
                Err(bus::BusServiceError::TopicNotFound { .. }) if create_if_missing => {
                    svc.create_topic(&bctx, &topic, crate::bus::topics::TopicOptions::default())?;
                    svc.publish(&bctx, &topic, batch)
                }
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(|e| anyhow!("bus_publish: publish task panicked: {e}"))?
        .map_err(|e| anyhow!("bus_publish: {e}"))?;

        let mut out = (*input.envelope).clone();
        out.meta
            .insert("bus_publish_topic".into(), serde_json::json!(topic));
        // PLAN-F3 §4.5: how many of this call's records (always exactly 1
        // here, a single-record batch) were diverted to `__dlq.<topic>` for
        // failing schema validation under `validation = dlq`, rather than
        // published. Always present (unlike `bus_publish_partition`/
        // `_offset` below, which only exist when something actually landed
        // in a partition) so a downstream flow node can branch on a
        // record that was quarantined instead of appended.
        out.meta.insert(
            "bus_publish_schema_rejected".into(),
            serde_json::json!(result.schema_rejected),
        );
        if let Some(ack) = result.single_partition() {
            out.meta.insert(
                "bus_publish_partition".into(),
                serde_json::json!(ack.partition),
            );
            out.meta
                .insert("bus_publish_offset".into(), serde_json::json!(ack.base_offset));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowValue;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "bp-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    /// plan-app-platform §3.3: `instance_id` is REQUIRED, read exactly like
    /// `topic`, with no fallback.
    #[test]
    fn config_requires_instance_id() {
        let err = parse_instance_id(&node(serde_json::json!({"topic": "t"}))).unwrap_err();
        assert!(err.to_string().contains("instance_id"));

        let err = parse_instance_id(&node(serde_json::json!({"instance_id": ""}))).unwrap_err();
        assert!(err.to_string().contains("instance_id"));

        let err =
            parse_instance_id(&node(serde_json::json!({"instance_id": "bogus"}))).unwrap_err();
        assert!(err.to_string().contains("bus instance id"));

        let id = parse_instance_id(&node(
            serde_json::json!({"instance_id": "tentabus-aaaaaaaa"}),
        ))
        .unwrap();
        assert_eq!(id.as_str(), "tentabus-aaaaaaaa");
    }

    #[test]
    fn build_payload_rejects_empty() {
        let env = FlowEnvelope::empty();
        assert!(build_payload(&env).is_err());
    }

    #[test]
    fn build_payload_text_is_raw_utf8_not_json_quoted() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        assert_eq!(build_payload(&env).unwrap(), Bytes::from("hello"));
    }

    #[test]
    fn build_payload_json_is_serialized() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(serde_json::json!({"a": 1})));
        assert_eq!(
            build_payload(&env).unwrap(),
            Bytes::from(serde_json::to_vec(&serde_json::json!({"a": 1})).unwrap())
        );
    }

    #[test]
    fn build_key_none_when_absent() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(serde_json::json!({"id": "x"})));
        let extras: [(&str, serde_json::Value); 0] = [];
        let scope = ExprScope {
            vars: &env.variables,
            payload: &env.payload,
            artifacts: &env.artifacts,
            meta: &env.meta,
            extras: &extras,
        };
        assert_eq!(build_key(&node(serde_json::json!({})), &scope).unwrap(), None);
    }

    #[test]
    fn build_key_evaluates_cel_over_payload() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(serde_json::json!({"id": "abc"})));
        let extras: [(&str, serde_json::Value); 0] = [];
        let scope = ExprScope {
            vars: &env.variables,
            payload: &env.payload,
            artifacts: &env.artifacts,
            meta: &env.meta,
            extras: &extras,
        };
        let n = node(serde_json::json!({"key": "payload.id"}));
        assert_eq!(
            build_key(&n, &scope).unwrap(),
            Some(Bytes::from("abc"))
        );
    }

    #[test]
    fn build_headers_content_type_and_cel_header() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(serde_json::json!({"org": "cmc"})));
        let extras: [(&str, serde_json::Value); 0] = [];
        let scope = ExprScope {
            vars: &env.variables,
            payload: &env.payload,
            artifacts: &env.artifacts,
            meta: &env.meta,
            extras: &extras,
        };
        let n = node(serde_json::json!({
            "content_type": "application/json",
            "headers": {"x-org": "payload.org"},
        }));
        let headers = build_headers(&n, &scope).unwrap();
        assert!(headers.contains(&("content-type".to_string(), Bytes::from("application/json"))));
        assert!(headers.contains(&("x-org".to_string(), Bytes::from("cmc"))));
    }
}
