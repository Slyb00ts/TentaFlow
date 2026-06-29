// =============================================================================
// Plik: flow_engine/node_adapters/platform_switch.rs
// Opis: PlatformSwitchNodeAdapter — jawny switch platformy. Jedno UNIWERSALNE
//       wejście (Any: Image/Text/Audio/Video/Json/Other/...) i 5 wyjść:
//       android / ios / macos / windows / linux. Aktywuje DOKŁADNIE jeden port =
//       bieżąca platforma węzła (target_os, na którym flow faktycznie biegnie).
//       Payload przechodzi BEZ ZMIAN — to czysty router gałęzi per-urządzenie,
//       widoczny na diagramie. Wzór routingu jak document_router.rs
//       (override active_output_ports + passthrough payloadu).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashSet;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "platform_switch";

/// Klucz meta, pod którym switch zapisuje wybraną platformę — czytany przez
/// `active_output_ports` (jedno źródło prawdy, brak ponownej detekcji).
const ROUTE_META_KEY: &str = "platform_route";

pub struct PlatformSwitchNodeAdapter;

impl PlatformSwitchNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Nazwa portu = bieżąca platforma (target_os węzła wykonującego flow).
    /// `cfg!` jest stałą kompilacji — na każdej platformie zostaje dokładnie
    /// jedna gałąź. Nieznany OS → "linux" (najszerszy serwerowy fallback).
    fn current_platform() -> &'static str {
        if cfg!(target_os = "android") {
            "android"
        } else if cfg!(target_os = "ios") {
            "ios"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            "linux"
        }
    }
}

impl Default for PlatformSwitchNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for PlatformSwitchNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        // Uniwersalne wejście — dowolny payload przechodzi (Image/Text/Audio/...).
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        // Po jednym porcie na platformę; każdy `Any` (payload przechodzi verbatim).
        vec![
            PortSpec::new("android", FlowDataType::Any),
            PortSpec::new("ios", FlowDataType::Any),
            PortSpec::new("macos", FlowDataType::Any),
            PortSpec::new("windows", FlowDataType::Any),
            PortSpec::new("linux", FlowDataType::Any),
        ]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("platform_switch: brak krawędzi wejściowej"))?;

        let port = Self::current_platform();

        // Passthrough — switch NIE zmienia payloadu, tylko wybiera port. Wybór
        // trafia w meta, skąd `active_output_ports` go odczyta (lustro
        // document_router::ROUTE_META_KEY).
        let mut out = (*input.envelope).clone();
        out.meta.insert(
            ROUTE_META_KEY.to_string(),
            serde_json::json!({ "port": port }),
        );
        Ok(out)
    }

    /// Bramkowanie: aktywuje DOKŁADNIE jeden port — bieżącą platformę zapisaną
    /// w `meta.platform_route.port`. Gałęzie pozostałych platform są nieaktywne.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let port = result
            .meta
            .get(ROUTE_META_KEY)
            .and_then(|v| v.get("port"))
            .and_then(|v| v.as_str())
            .unwrap_or("linux")
            .to_string();
        Some(HashSet::from([port]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node() -> FlowNode {
        FlowNode {
            id: "platform-1".into(),
            node_type: NODE_TYPE.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    fn text_input() -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("dowolny payload".into());
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn activates_exactly_one_platform_port() {
        let ctx = stub_ctx();
        let out = PlatformSwitchNodeAdapter::new()
            .execute(&node(), &[text_input()], &ctx)
            .await
            .unwrap();
        let active = PlatformSwitchNodeAdapter::new()
            .active_output_ports(&node(), &out)
            .unwrap();
        assert_eq!(active.len(), 1, "switch aktywuje dokładnie jeden port");
        let port = active.into_iter().next().unwrap();
        assert!(
            ["android", "ios", "macos", "windows", "linux"].contains(&port.as_str()),
            "port musi być jedną z platform, dostał {port}"
        );
        assert_eq!(port, PlatformSwitchNodeAdapter::current_platform());
    }

    #[tokio::test]
    async fn passthrough_preserves_payload() {
        let ctx = stub_ctx();
        let input = text_input();
        let original = (*input.envelope).payload.clone();
        let out = PlatformSwitchNodeAdapter::new()
            .execute(&node(), &[input], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload, original, "switch nie modyfikuje payloadu");
    }

    #[test]
    fn advertises_five_platform_ports() {
        let names: Vec<String> = PlatformSwitchNodeAdapter::new()
            .output_ports()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, vec!["android", "ios", "macos", "windows", "linux"]);
    }
}
