// =============================================================================
// Plik: flow_engine/node_adapters/camera_alert.rs
// Opis: camera_alert node — gdy poprzedni verdict to "alarm", publikuje
//       strukturalny event "camera.alarm" na globalnym EventBusie. Zasubskrybowane
//       addony (TentaVision) dostają go przez on_event i zapisują alarm w swojej
//       bazie. Node jest core (stabilny node_type, seedowalny), a własność danych
//       alarmów zostaje po stronie addonu. Payload przepuszczany bez zmian;
//       meta["alert"] zapisuje czy event poszedł (obserwowalne w testach/overlay).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::addon::event_bus::{publish_global_event, Event};
use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "camera_alert";
/// Event type emitted on an alarm verdict. Addons subscribe to this to ingest
/// alarms (TentaVision `on_event` → `db::insert_alarm`).
const ALARM_EVENT: &str = "camera.alarm";

pub struct CameraAlertNodeAdapter;

impl CameraAlertNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeAdapter for CameraAlertNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("out", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let envelope = inputs
            .first()
            .map(|i| i.envelope.clone())
            .unwrap_or_else(|| ctx.initial_envelope.clone());
        let mut out = (*envelope).clone();

        let verdict = out.meta.get("verdict");
        let is_alarm = verdict
            .and_then(|v| v.get("decision"))
            .and_then(|d| d.as_str())
            == Some("alarm");

        if !is_alarm {
            out.meta
                .insert("alert".into(), serde_json::json!({ "emitted": false }));
            return Ok(out);
        }

        let camera_id = out
            .meta
            .get("camera_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let reason = verdict
            .and_then(|v| v.get("reason"))
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        if camera_id.is_empty() {
            return Err(anyhow!(
                "camera_alert: alarm verdict but meta[camera_id] missing"
            ));
        }

        let payload = serde_json::json!({
            "camera_id": camera_id,
            "reason": reason,
            "severity": "high",
            "detections": out.meta.get("detections").cloned().unwrap_or(serde_json::Value::Null),
        });
        publish_global_event(Event {
            event_type: ALARM_EVENT.to_string(),
            source_addon: None,
            source_user: None,
            payload,
            timestamp: chrono::Utc::now(),
        });

        out.meta.insert(
            "alert".into(),
            serde_json::json!({ "emitted": true, "reason": reason }),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowValue;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node() -> FlowNode {
        FlowNode {
            id: "a".into(),
            node_type: NODE_TYPE.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    async fn alert_meta(verdict: serde_json::Value, camera_id: &str) -> serde_json::Value {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::with_payload(FlowValue::Empty);
        env.meta.insert("verdict".into(), verdict);
        env.meta.insert(
            "camera_id".into(),
            serde_json::Value::String(camera_id.into()),
        );
        let input = NodeInput {
            from_node_id: "v".into(),
            from_port: "out".into(),
            envelope: Arc::new(env),
        };
        let out = CameraAlertNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        out.meta.get("alert").unwrap().clone()
    }

    #[tokio::test]
    async fn emits_on_alarm_verdict() {
        let m = alert_meta(
            serde_json::json!({ "decision": "alarm", "reason": "nalepka_adr: uszkodzona" }),
            "cam_1",
        )
        .await;
        assert_eq!(m["emitted"], true);
    }

    #[tokio::test]
    async fn silent_on_ok_verdict() {
        let m = alert_meta(
            serde_json::json!({ "decision": "ok", "reason": "" }),
            "cam_1",
        )
        .await;
        assert_eq!(m["emitted"], false);
    }
}
