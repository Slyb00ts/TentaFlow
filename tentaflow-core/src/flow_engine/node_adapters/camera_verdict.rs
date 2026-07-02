// =============================================================================
// Plik: flow_engine/node_adapters/camera_verdict.rs
// Opis: camera_verdict node — deterministyczna decyzja ADR/UN z detekcji klatki.
//       Czyta meta["detections"] (po wzbogaceniu przez vision_ocr/classify),
//       ocenia czy klatka wymaga alarmu (uszkodzona/nieczytelna nalepka, brak
//       czytelnej tablicy), i zapisuje meta["verdict"] = {decision, reason}.
//       Przepuszcza payload bez zmian, zeby alert node dostal te sama klatke.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::detection_bus::Detection;

const NODE_TYPE: &str = "camera_verdict";

/// Default placard `stan` tags that escalate to an alarm (damaged / unreadable /
/// missing hazard label). Overridable via `node.config["alarm_states"]`.
const DEFAULT_ALARM_STATES: &[&str] = &["uszkodzona", "nieczytelna", "brak", "brudna"];

/// Classes carrying a hazard placard/sign whose `stan` is alarm-relevant.
fn is_placard(klasa: &str) -> bool {
    klasa.starts_with("nalepka") || klasa == "znak_srodowiskowy"
}

pub struct CameraVerdictNodeAdapter;

impl CameraVerdictNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeAdapter for CameraVerdictNodeAdapter {
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
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let envelope = inputs
            .first()
            .map(|i| i.envelope.clone())
            .unwrap_or_else(|| ctx.initial_envelope.clone());
        let mut out = (*envelope).clone();

        let detections: Vec<Detection> = match out.meta.get("detections") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| anyhow!("camera_verdict: meta[detections] not a Detection list: {e}"))?,
            None => Vec::new(),
        };

        let alarm_states: Vec<String> = node
            .config
            .get("alarm_states")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_else(|| DEFAULT_ALARM_STATES.iter().map(|s| s.to_string()).collect());
        let require_plate_text = node
            .config
            .get("require_plate_text")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut reasons: Vec<String> = Vec::new();
        for det in &detections {
            if is_placard(&det.klasa) {
                for s in &det.stan {
                    if alarm_states.iter().any(|a| a == s) {
                        reasons.push(format!("{}: {}", det.klasa, s));
                    }
                }
            }
            if require_plate_text && det.klasa == "tablica_rejestracyjna" && det.tekst.is_none() {
                reasons.push("nieczytelna tablica rejestracyjna".to_string());
            }
        }

        let decision = if reasons.is_empty() { "ok" } else { "alarm" };
        out.meta.insert(
            "verdict".into(),
            serde_json::json!({
                "decision": decision,
                "reason": reasons.join("; "),
            }),
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

    fn det(klasa: &str, stan: Vec<&str>, tekst: Option<&str>) -> Detection {
        Detection {
            klasa: klasa.into(),
            bbox: [0.0, 0.0, 0.5, 0.5],
            score: 0.9,
            stan: stan.into_iter().map(|s| s.to_string()).collect(),
            tekst: tekst.map(|s| s.to_string()),
            track_id: 0,
            vx: 0.,
            vy: 0.,
        }
    }

    fn node() -> FlowNode {
        FlowNode {
            id: "v".into(),
            node_type: NODE_TYPE.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    async fn verdict_for(dets: Vec<Detection>) -> serde_json::Value {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::with_payload(FlowValue::Empty);
        env.meta
            .insert("detections".into(), serde_json::to_value(&dets).unwrap());
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "out".into(),
            envelope: Arc::new(env),
        };
        let out = CameraVerdictNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        out.meta.get("verdict").unwrap().clone()
    }

    #[tokio::test]
    async fn ok_when_readable_plate_and_clean_placards() {
        let v = verdict_for(vec![
            det("tablica_rejestracyjna", vec![], Some("WX 12345")),
            det("nalepka_adr", vec!["pełna"], None),
        ])
        .await;
        assert_eq!(v["decision"], "ok");
    }

    #[tokio::test]
    async fn alarm_on_damaged_placard() {
        let v = verdict_for(vec![det("nalepka_adr", vec!["uszkodzona"], None)]).await;
        assert_eq!(v["decision"], "alarm");
        assert!(v["reason"].as_str().unwrap().contains("uszkodzona"));
    }

    #[tokio::test]
    async fn alarm_on_unreadable_plate() {
        let v = verdict_for(vec![det("tablica_rejestracyjna", vec![], None)]).await;
        assert_eq!(v["decision"], "alarm");
        assert!(v["reason"]
            .as_str()
            .unwrap()
            .contains("nieczytelna tablica"));
    }
}
