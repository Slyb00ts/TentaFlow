// =============================================================================
// Plik: flow_engine/node_adapters/addon.rs
// Opis: AddonNodeAdapter — generyczny adapter dla custom flow blocks z addonow
//       WASM. Resolver w AdapterRegistry (etap B) buduje instancje tego adaptera
//       per node_type "addon.{addon_id}.{block_type}" z AddonFlowRegistry.
//       Decyzje: fresh instance per call (#7), per-call fuel/memory/timeout
//       (#6), JSON envelope na granicy ABI (#1).
// =============================================================================

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tracing::warn;

use crate::addon::flow_blocks::AddonFlowBlock;
use crate::addon::AddonManager;
use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

/// Domyslny budzet paliwa per invocation bloku — 50M instrukcji (5x wiecej
/// niz DEFAULT_FUEL_LIMIT dla tool calls, bo blok flow moze robic wiecej
/// pracy: parsing envelope, transform, serializacja).
const DEFAULT_BLOCK_FUEL: u64 = 50_000_000;

/// Domyslny timeout per invocation bloku — 30 sekund. Operator moze nadpisac
/// w `node.config["timeout_ms"]`. Brak deadline = brak watchdog'a.
const DEFAULT_BLOCK_TIMEOUT_MS: u64 = 30_000;

/// Adapter dla pojedynczego bloku zarejestrowanego przez addon. Jedna
/// instancja per (addon_id, block_type) — owned przez resolver closure
/// w AdapterRegistry.
pub struct AddonNodeAdapter {
    /// Pelny node_type — "addon.{addon_id}.{block_type}". To samo co
    /// `AddonFlowBlock.block_type`.
    node_type: String,
    addon_id: String,
    block_type: String,
    /// Porty input deklarowane przez addon w blocks.json — port name + typ
    /// FlowDataType (mapowanie z `BlockPort.port_type` przez `parse_data_type`).
    input_ports: Vec<PortSpec>,
    output_ports: Vec<PortSpec>,
    /// Manager addona — wykonuje invoke_block. Trzymany jako Arc bo
    /// resolver buduje adapter dynamicznie i musi miec wlasna referencje
    /// (resolver moze byc wywolany po install/uninstall cycle, manager
    /// musi przezywac adapter).
    manager: Arc<AddonManager>,
}

impl AddonNodeAdapter {
    /// Buduje adapter z deklaracji `AddonFlowBlock` + referencja do managera.
    /// Resolver w `dispatcher::build_registry` woła to per node_type.
    pub fn from_block(block: &AddonFlowBlock, manager: Arc<AddonManager>) -> Self {
        let input_ports = block
            .inputs
            .iter()
            .map(|p| PortSpec::new(p.name.clone(), parse_data_type(&p.port_type)))
            .collect();
        let output_ports = block
            .outputs
            .iter()
            .map(|p| PortSpec::new(p.name.clone(), parse_data_type(&p.port_type)))
            .collect();
        Self {
            node_type: block.block_type.clone(),
            addon_id: block.addon_id.clone(),
            block_type: block.original_type.clone(),
            input_ports,
            output_ports,
            manager,
        }
    }
}

#[async_trait]
impl NodeAdapter for AddonNodeAdapter {
    fn node_type(&self) -> &str {
        &self.node_type
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        self.input_ports.clone()
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        self.output_ports.clone()
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        // Single-input rule (R4) — adapter pobiera envelope z inputs[0]
        // jesli jest, albo seed z ctx.initial_envelope gdy block jest
        // bezposrednio za triggerem (rare, ale dozwolone bo trigger nie
        // ma input).
        let input_envelope: FlowEnvelope = if let Some(input) = inputs.first() {
            (*input.envelope).clone()
        } else {
            (*ctx.initial_envelope).clone()
        };

        let envelope_json = serde_json::to_vec(&input_envelope)
            .map_err(|e| anyhow!("AddonNodeAdapter '{}': serialize envelope: {e}", self.node_type))?;

        // Per-call fuel budget — operator moze nadpisac w node config,
        // inaczej DEFAULT_BLOCK_FUEL.
        let fuel = node
            .config
            .get("fuel_budget")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_BLOCK_FUEL);

        // Per-call deadline — node.config["timeout_ms"] > ctx.deadline >
        // DEFAULT_BLOCK_TIMEOUT_MS. ctx.deadline (z executora) wygrywa nad
        // node config gdy jest blizej w czasie — block nie moze przedluzyc
        // flow timeoutu.
        let configured_timeout_ms = node
            .config
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_BLOCK_TIMEOUT_MS);
        let from_config = Instant::now() + Duration::from_millis(configured_timeout_ms);
        let deadline = match ctx.deadline {
            Some(flow_deadline) if flow_deadline < from_config => Some(flow_deadline),
            Some(_) => Some(from_config),
            None => Some(from_config),
        };

        // Cancel check before going into WASM — jesli klient juz disconnected,
        // nie marnujemy paliwa na invocation.
        if ctx.cancel_token.is_cancelled() {
            return Err(anyhow!(
                "AddonNodeAdapter '{}': cancelled before invocation",
                self.node_type
            ));
        }

        // invoke_block jest synchroniczny (wasmtime call) — wpychamy go na
        // blocking thread zeby nie blokowac tokio executora.
        let manager = self.manager.clone();
        let addon_id = self.addon_id.clone();
        let block_type = self.block_type.clone();
        let user_id = ctx.user_id;
        let node_type = self.node_type.clone();

        let response_bytes = tokio::task::spawn_blocking(move || {
            manager.invoke_block(
                &addon_id,
                &block_type,
                &envelope_json,
                user_id,
                fuel,
                deadline,
            )
        })
        .await
        .map_err(|e| anyhow!("AddonNodeAdapter '{}': spawn_blocking join: {e}", node_type))?
        .map_err(|e| anyhow!("AddonNodeAdapter '{}': invoke_block: {e}", self.node_type))?;

        // Odpowiedz musi byc FlowEnvelope-shaped JSON. Addon ktory nie zwraca
        // poprawnego envelope (np. zwroci `{"error": "..."}`) dostaje czytelny
        // blad z bracketem na addon_id, zeby debug w GUI byl prosty.
        let envelope: FlowEnvelope = serde_json::from_slice(&response_bytes).map_err(|e| {
            let preview = String::from_utf8_lossy(&response_bytes);
            let preview = if preview.len() > 200 {
                format!("{}...", &preview[..200])
            } else {
                preview.into_owned()
            };
            warn!(
                "AddonNodeAdapter '{}': response nie jest FlowEnvelope JSON: {} (preview: {})",
                self.node_type, e, preview
            );
            anyhow!(
                "AddonNodeAdapter '{}': response nie jest FlowEnvelope JSON: {e}",
                self.node_type
            )
        })?;

        Ok(envelope)
    }
}

/// Mapuje stringowy `BlockPort.port_type` (np. "text", "audio") na
/// `FlowDataType`. Format zgodny z `FlowDataType::as_wire_str`.
fn parse_data_type(s: &str) -> FlowDataType {
    match s {
        "text" => FlowDataType::Text,
        "audio" => FlowDataType::Audio,
        "image" => FlowDataType::Image,
        "video" => FlowDataType::Video,
        "embedding" => FlowDataType::Embedding,
        "json" => FlowDataType::Json,
        "other" => FlowDataType::Other,
        _ => FlowDataType::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_data_type_maps_wire_strings() {
        assert_eq!(parse_data_type("text"), FlowDataType::Text);
        assert_eq!(parse_data_type("audio"), FlowDataType::Audio);
        assert_eq!(parse_data_type("image"), FlowDataType::Image);
        assert_eq!(parse_data_type("video"), FlowDataType::Video);
        assert_eq!(parse_data_type("embedding"), FlowDataType::Embedding);
        assert_eq!(parse_data_type("json"), FlowDataType::Json);
        assert_eq!(parse_data_type("other"), FlowDataType::Other);
        assert_eq!(parse_data_type("nieznany"), FlowDataType::Any);
        assert_eq!(parse_data_type(""), FlowDataType::Any);
    }
}
