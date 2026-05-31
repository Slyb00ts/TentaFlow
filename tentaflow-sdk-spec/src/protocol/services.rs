// =============================================================================
// File: protocol/services.rs — service-catalog host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of
// `service_list_v1` and `node_resources_get_v1`. Shared verbatim by the core
// host (decode input / encode output) and the addon SDK (encode input / decode
// output) so the wire format cannot drift. Maps use integer keys via
// `#[cbor(map)]` + `#[n(N)]`. Filter fields and the optional GPU block are
// `Option` on the wire so a minimal payload can omit them.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// service_list_v1
// -----------------------------------------------------------------------------

/// Input for `service_list_v1`. Every filter is optional; an all-`None`
/// payload (or an empty input buffer) returns the unfiltered list.
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cbor(map)]
pub struct ServiceListInput {
    #[n(0)]
    pub kind: Option<String>,
    #[n(1)]
    pub status: Option<String>,
    #[n(2)]
    pub node_id: Option<String>,
}

/// One service row returned by `service_list_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ServiceInfoOut {
    #[n(0)]
    pub service_id: String,
    #[n(1)]
    pub service_local_id: i64,
    #[n(2)]
    pub display_name: String,
    #[n(3)]
    pub kind: String,
    #[n(4)]
    pub status: String,
    #[n(5)]
    pub node_id: String,
    #[n(6)]
    pub endpoint: Option<String>,
    #[n(7)]
    pub capabilities: Vec<String>,
}

/// Output of `service_list_v1`.
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cbor(map)]
pub struct ServiceListOutput {
    #[n(0)]
    pub services: Vec<ServiceInfoOut>,
}

// -----------------------------------------------------------------------------
// node_resources_get_v1
// -----------------------------------------------------------------------------

/// Input for `node_resources_get_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct NodeResourcesInput {
    #[n(0)]
    pub node_id: String,
}

/// First-GPU snapshot in `node_resources_get_v1`. `gpu_count` on the parent
/// carries the total so a multi-GPU host is not silently misreported.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GpuOut {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub vram_total_mb: u64,
    #[n(2)]
    pub vram_used_mb: u64,
    #[n(3)]
    pub utilization_pct: f64,
}

/// Output of `node_resources_get_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct NodeResourcesOut {
    #[n(0)]
    pub node_id: String,
    #[n(1)]
    pub cpu_cores: u32,
    #[n(2)]
    pub cpu_load_pct: f64,
    #[n(3)]
    pub ram_total_mb: u64,
    #[n(4)]
    pub ram_used_mb: u64,
    #[n(5)]
    pub gpu: Option<GpuOut>,
    #[n(6)]
    pub gpu_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_list_input_empty_and_filtered() {
        roundtrip(&ServiceListInput::default());
        roundtrip(&ServiceListInput {
            kind: Some("llm".into()),
            status: Some("running".into()),
            node_id: Some("n1".into()),
        });
    }

    #[test]
    fn roundtrip_list_output() {
        roundtrip(&ServiceListOutput {
            services: vec![ServiceInfoOut {
                service_id: "n1:7".into(),
                service_local_id: 7,
                display_name: "yolo".into(),
                kind: "vision".into(),
                status: "running".into(),
                node_id: "n1".into(),
                endpoint: Some("http://127.0.0.1:8000".into()),
                capabilities: vec!["detect".into()],
            }],
        });
    }

    #[test]
    fn roundtrip_node_resources_with_and_without_gpu() {
        roundtrip(&NodeResourcesOut {
            node_id: "n1".into(),
            cpu_cores: 16,
            cpu_load_pct: 42.5,
            ram_total_mb: 64_000,
            ram_used_mb: 12_000,
            gpu: Some(GpuOut {
                name: "RTX 4090".into(),
                vram_total_mb: 24_000,
                vram_used_mb: 3_000,
                utilization_pct: 71.0,
            }),
            gpu_count: 1,
        });
        roundtrip(&NodeResourcesOut {
            node_id: "n1".into(),
            cpu_cores: 8,
            cpu_load_pct: 0.0,
            ram_total_mb: 16_000,
            ram_used_mb: 2_000,
            gpu: None,
            gpu_count: 0,
        });
    }
}
