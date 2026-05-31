// =============================================================================
// File: addon/host_functions/services.rs
// F2 P2.a — addon-facing read-only views over the runtime service catalog
// and per-node hardware resources. Two host functions:
//   * service_list_v1   — filters the mesh-wide service registry (local
//                          snapshot + every reachable peer) for the M16 v2
//                          dropdown / addon orchestration code.
//   * node_resources_get_v1 — CPU/RAM/GPU snapshot for the named node. Only
//                              the local node is materialised today; remote
//                              peers will fold in once `NodeInfo` heartbeats
//                              carry live CPU/RAM (current heartbeat schema
//                              is unstable, so cross-node returns NotFound).
//
// Both require `service.read` permission, risk class C. Services are
// organisation-global by design (admin-registered, see F2 plan §P1.c.4) —
// the org_id is still threaded into audit so an org admin can see who
// inspected the catalog from their tenancy.
// =============================================================================

use minicbor::Encode;
use tentaflow_sdk_spec::{
    GpuOut, NodeResourcesInput, NodeResourcesOut, ServiceInfoOut, ServiceListInput,
    ServiceListOutput,
};

use super::abi_helpers::{write_output_with_retry_semantics, PayloadKind};
use super::cbor_io::read_input_cbor;
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;

/// Required permission for both host functions. Risk class C — read-only.
const PERM_SERVICE_READ: &str = "service.read";

// ---------------------------------------------------------------------------
// service_list_v1
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn service_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    if !check_permission(caller.data(), PERM_SERVICE_READ, None) {
        audit_service(
            caller.data(),
            "service.list",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    // Empty input (input_len == 0) is allowed — returns the unfiltered list.
    let input: ServiceListInput = if input_len == 0 {
        ServiceListInput::default()
    } else {
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::Secret) {
            Ok(v) => v,
            Err(e) => {
                audit_service(
                    caller.data(),
                    "service.list",
                    "error",
                    Some(if e == AbiError::PayloadTooLarge {
                        "payload_too_large"
                    } else {
                        "invalid_payload"
                    }),
                );
                return e.as_i32();
            }
        }
    };

    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            // Without a router the addon is running in a stripped test/boot
            // environment (no mesh registry wired). Return an empty list
            // rather than fabricating data.
            audit_service(
                caller.data(),
                "service.list",
                "ok",
                Some("router_unavailable"),
            );
            let empty = ServiceListOutput::default();
            return write_services_output(
                &memory, &mut caller, &empty, out_ptr, out_cap, out_len_ptr,
            );
        }
    };

    let visible = {
        let guard = router.service_manager().mesh_services_registry.read();
        match guard.as_ref() {
            Some(reg) => reg.visible_services(),
            None => Vec::new(),
        }
    };

    let filtered: Vec<ServiceInfoOut> = visible
        .into_iter()
        .filter(|s| match input.kind.as_deref() {
            Some(k) => s.category == k,
            None => true,
        })
        .filter(|s| match input.status.as_deref() {
            Some(st) => s.status == st,
            None => true,
        })
        .filter(|s| match input.node_id.as_deref() {
            Some(nid) => s.node_id == nid,
            None => true,
        })
        .map(|s| {
            let mut caps: Vec<String> = s
                .models
                .iter()
                .flat_map(|m| m.capabilities.clone())
                .collect();
            caps.sort();
            caps.dedup();
            ServiceInfoOut {
                service_id: format!("{}:{}", s.node_id, s.id),
                service_local_id: s.id,
                display_name: s.display_name.clone(),
                kind: s.category.clone(),
                status: s.status.clone(),
                node_id: s.node_id.clone(),
                endpoint: s.endpoint_url.as_deref().map(sanitize_endpoint_url),
                capabilities: caps,
            }
        })
        .collect();

    let out = ServiceListOutput { services: filtered };
    audit_service(caller.data(), "service.list", "ok", None);
    write_services_output(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

/// Strips userinfo (`user:pass@`) and query-string from a service endpoint
/// URL before exposing it to addons via `service_list_v1`. If an admin
/// inadvertently stored credentials inline (e.g. `http://user:pass@host:8000`)
/// or token-bearing query params, this prevents any addon holding
/// `service.read` from harvesting them. Falls back to the original on parse
/// failure since the URL was already in storage and stripping is best-effort.
fn sanitize_endpoint_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.into()
        }
        Err(_) => raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// node_resources_get_v1
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn node_resources_get_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_SERVICE_READ, None) {
        audit_service(
            caller.data(),
            "service.node_resources_get",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: NodeResourcesInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::Secret) {
            Ok(v) => v,
            Err(e) => {
                audit_service(
                    caller.data(),
                    "service.node_resources_get",
                    "error",
                    Some(if e == AbiError::PayloadTooLarge {
                        "payload_too_large"
                    } else {
                        "invalid_payload"
                    }),
                );
                return e.as_i32();
            }
        };

    // Resolve which node the caller asked about. We materialise resources
    // only for the local node — remote peers do not yet broadcast a stable
    // hardware snapshot we can return. Resolve the local id via the router,
    // falling back to the heartbeat collector if the registry has not been
    // wired (boot, tests).
    let local_node_id: Option<String> = caller.data().router.as_ref().and_then(|r| {
        let guard = r.service_manager().mesh_services_registry.read();
        guard.as_ref().map(|reg| reg.local().node_id.clone())
    });
    let local_node_id = local_node_id.unwrap_or_default();

    if input.node_id != local_node_id {
        audit_service(
            caller.data(),
            "service.node_resources_get",
            "denied",
            Some("unknown_node"),
        );
        return AbiError::NotFound.as_i32();
    }

    let node_info = crate::mesh::node_info_collector::collect_node_info(&local_node_id);
    let metrics = crate::mesh::node_info_collector::collect_fast_metrics();

    let gpu = node_info.gpu_info.first().map(|g| {
        // VRAM totals come from the static `NodeInfo` payload; live used/util
        // come from the fast metrics refresh. Align them by index when
        // possible — fallback to the static row when fast metrics is empty.
        let live = metrics.gpus.first();
        GpuOut {
            name: g.name.clone(),
            vram_total_mb: g.vram_total_mb,
            vram_used_mb: live.map(|x| x.vram_used_mb).unwrap_or(g.vram_used_mb),
            utilization_pct: live
                .map(|x| x.usage_percent as f64)
                .unwrap_or(g.usage_percent as f64),
        }
    });

    let out = NodeResourcesOut {
        node_id: local_node_id.clone(),
        cpu_cores: node_info.cpu_count,
        cpu_load_pct: metrics.cpu_usage_percent as f64,
        ram_total_mb: node_info.ram_total_mb,
        ram_used_mb: metrics.ram_used_mb,
        gpu,
        gpu_count: node_info.gpu_info.len() as u32,
    };

    audit_service(caller.data(), "service.node_resources_get", "ok", None);
    write_services_output(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// ---------------------------------------------------------------------------
// Helpers (private to this module)
// ---------------------------------------------------------------------------

/// Encodes `value` to CBOR and writes it through the retry helper. The service
/// catalog / node-resources responses are small and bounded by design, so the
/// cap stays at the module-local 32 KiB ceiling rather than a `PayloadKind`
/// bucket: typical clusters carry under ~100 services and UI consumers paginate
/// in JS if they ever approach it.
fn write_services_output<T: Encode<()>>(
    memory: &super::super::runtime::WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let mut serialized = Vec::new();
    if minicbor::encode(value, &mut serialized).is_err() {
        return AbiError::Operation.as_i32();
    }
    if serialized.len() > 32 * 1024 {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(memory, caller, &serialized, out_ptr, out_cap, out_len_ptr)
}

fn audit_service(state: &AddonState, action: &str, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("service"),
        None,
        RiskClass::C,
        None,
        None,
        result,
        reason,
    );
}

// =============================================================================
// Public test surface — invoked from `tests/service_list_host_fn.rs`
// =============================================================================

/// Re-exports the internal logic under a stable name so integration tests
/// can drive permission gating + filtering without standing up a WASM Store.
/// Marked `#[doc(hidden)]` — not part of the addon-facing API.
#[doc(hidden)]
pub mod test_api {
    use crate::services::mesh_registry::MeshServicesRegistry;
    use tentaflow_protocol::ServiceInfo as RegistryServiceInfo;

    #[doc(hidden)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServiceListItem {
        pub service_id: String,
        pub service_local_id: i64,
        pub display_name: String,
        pub kind: String,
        pub status: String,
        pub node_id: String,
        pub endpoint: Option<String>,
        pub capabilities: Vec<String>,
    }

    /// Pure-functional core of `service_list_v1`: filter `visible` against
    /// the three optional predicates and project to the wire shape.
    #[doc(hidden)]
    pub fn filter_services(
        visible: Vec<RegistryServiceInfo>,
        kind: Option<&str>,
        status: Option<&str>,
        node_id: Option<&str>,
    ) -> Vec<ServiceListItem> {
        visible
            .into_iter()
            .filter(|s| kind.map_or(true, |k| s.category == k))
            .filter(|s| status.map_or(true, |st| s.status == st))
            .filter(|s| node_id.map_or(true, |n| s.node_id == n))
            .map(|s| {
                let mut caps: Vec<String> = s
                    .models
                    .iter()
                    .flat_map(|m| m.capabilities.clone())
                    .collect();
                caps.sort();
                caps.dedup();
                ServiceListItem {
                    service_id: format!("{}:{}", s.node_id, s.id),
                    service_local_id: s.id,
                    display_name: s.display_name.clone(),
                    kind: s.category.clone(),
                    status: s.status.clone(),
                    node_id: s.node_id.clone(),
                    endpoint: s.endpoint_url.as_deref().map(super::sanitize_endpoint_url),
                    capabilities: caps,
                }
            })
            .collect()
    }

    /// Convenience: snapshot the registry then filter. Mirrors the
    /// non-permission path of `service_list_v1`.
    #[doc(hidden)]
    pub fn list_from_registry(
        registry: &MeshServicesRegistry,
        kind: Option<&str>,
        status: Option<&str>,
        node_id: Option<&str>,
    ) -> Vec<ServiceListItem> {
        filter_services(registry.visible_services(), kind, status, node_id)
    }

    /// Materialise local node resources. Mirrors the same call path the
    /// host function takes (`collect_node_info` + `collect_fast_metrics`)
    /// minus the WASM ABI shell.
    #[doc(hidden)]
    pub fn local_node_resources(node_id: &str) -> super::NodeResourcesOutPublic {
        let info = crate::mesh::node_info_collector::collect_node_info(node_id);
        let metrics = crate::mesh::node_info_collector::collect_fast_metrics();
        let gpu = info.gpu_info.first().map(|g| {
            let live = metrics.gpus.first();
            super::GpuOutPublic {
                name: g.name.clone(),
                vram_total_mb: g.vram_total_mb,
                vram_used_mb: live.map(|x| x.vram_used_mb).unwrap_or(g.vram_used_mb),
                utilization_pct: live
                    .map(|x| x.usage_percent as f64)
                    .unwrap_or(g.usage_percent as f64),
            }
        });
        super::NodeResourcesOutPublic {
            node_id: node_id.to_string(),
            cpu_cores: info.cpu_count,
            cpu_load_pct: metrics.cpu_usage_percent as f64,
            ram_total_mb: info.ram_total_mb,
            ram_used_mb: metrics.ram_used_mb,
            gpu,
            gpu_count: info.gpu_info.len() as u32,
        }
    }
}

/// Public-but-doc-hidden mirrors of the private wire structs so the
/// integration test can inspect fields without round-tripping TOML.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct NodeResourcesOutPublic {
    pub node_id: String,
    pub cpu_cores: u32,
    pub cpu_load_pct: f64,
    pub ram_total_mb: u64,
    pub ram_used_mb: u64,
    pub gpu: Option<GpuOutPublic>,
    pub gpu_count: u32,
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GpuOutPublic {
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: u64,
    pub utilization_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_list_input_roundtrips_filters() {
        let input = ServiceListInput {
            kind: Some("llm".into()),
            status: Some("running".into()),
            node_id: Some("n1".into()),
        };
        let mut buf = Vec::new();
        minicbor::encode(&input, &mut buf).unwrap();
        let decoded: ServiceListInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.kind.as_deref(), Some("llm"));
        assert_eq!(decoded.status.as_deref(), Some("running"));
        assert_eq!(decoded.node_id.as_deref(), Some("n1"));
    }

    #[test]
    fn service_list_output_roundtrips_minimal_shape() {
        let out = ServiceListOutput {
            services: vec![ServiceInfoOut {
                service_id: "n1:7".to_string(),
                service_local_id: 7,
                display_name: "yolo".into(),
                kind: "vision".into(),
                status: "running".into(),
                node_id: "n1".into(),
                endpoint: Some("http://127.0.0.1:8000".into()),
                capabilities: vec!["detect".into()],
            }],
        };
        let mut buf = Vec::new();
        minicbor::encode(&out, &mut buf).unwrap();
        let decoded: ServiceListOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.services.len(), 1);
        assert_eq!(decoded.services[0].service_id, "n1:7");
        assert_eq!(decoded.services[0].kind, "vision");
        assert_eq!(decoded.services[0].capabilities, vec!["detect".to_string()]);
    }

    #[test]
    fn node_resources_input_roundtrips_node_id() {
        let input = NodeResourcesInput {
            node_id: "n1".into(),
        };
        let mut buf = Vec::new();
        minicbor::encode(&input, &mut buf).unwrap();
        let decoded: NodeResourcesInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.node_id, "n1");
    }
}
