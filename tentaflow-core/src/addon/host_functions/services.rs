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

use serde::{Deserialize, Serialize};

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;

/// Required permission for both host functions. Risk class C — read-only.
const PERM_SERVICE_READ: &str = "service.read";

// ---------------------------------------------------------------------------
// service_list_v1
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct ServiceListInput {
    /// Optional category filter (`llm`, `embedding`, `stt`, `tts`, `vision`,
    /// ...). Match is case-sensitive against `ServiceInfo.category`.
    #[serde(default)]
    kind: Option<String>,
    /// Optional status filter (`starting`, `running`, `degraded`, `failed`,
    /// `stopped`). Match is case-sensitive.
    #[serde(default)]
    status: Option<String>,
    /// Restrict the list to one mesh node (typically the local node id).
    #[serde(default)]
    node_id: Option<String>,
}

#[derive(Debug, Serialize, Default)]
struct ServiceListOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    services: Vec<ServiceInfoOut>,
}

#[derive(Debug, Serialize)]
struct ServiceInfoOut {
    /// Stable composite id `<node>:<service>` so addons can address the same
    /// service across mesh reconnects without juggling two fields.
    service_id: String,
    /// Numeric service id local to the owning node (matches the DB rowid
    /// emitted by `services_repo`). Kept alongside `service_id` because the
    /// router APIs key on the numeric value.
    service_local_id: i64,
    display_name: String,
    /// `category` from the registry (`llm`, `embedding`, …).
    kind: String,
    status: String,
    node_id: String,
    /// Public endpoint URL where addons can dispatch (may be `None` for
    /// embedded native services that go via the in-process backend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    /// Union of model `capabilities` declared by every model the service
    /// exposes. Empty when the service publishes no models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<String>,
}

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

    // Empty input (input_len == 0) is allowed — returns the unfiltered list.
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit_service(caller.data(), "service.list", "error", Some("input_read_failed"));
            return e.as_i32();
        }
    };

    let input: ServiceListInput = if raw.trim().is_empty() {
        ServiceListInput::default()
    } else {
        match toml::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                audit_service(caller.data(), "service.list", "error", Some("invalid_toml"));
                return AbiError::Operation.as_i32();
            }
        }
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

    let router = match caller.data().router.as_ref() {
        Some(r) => r.clone(),
        None => {
            // Without a router the addon is running in a stripped test/boot
            // environment (no mesh registry wired). Return an empty list
            // rather than fabricating data.
            audit_service(caller.data(), "service.list", "ok", Some("router_unavailable"));
            let empty = ServiceListOutput::default();
            return write_toml_capped(&memory, &mut caller, &empty, out_ptr, out_cap, out_len_ptr);
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
            let mut caps: Vec<String> =
                s.models.iter().flat_map(|m| m.capabilities.clone()).collect();
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
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
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

#[derive(Debug, Deserialize)]
struct NodeResourcesInput {
    node_id: String,
}

#[derive(Debug, Serialize)]
struct NodeResourcesOut {
    node_id: String,
    cpu_cores: u32,
    /// Aggregate CPU usage across all cores, last refresh. 0..=100.
    cpu_load_pct: f64,
    ram_total_mb: u64,
    ram_used_mb: u64,
    /// First GPU only when the host exposes one. `gpu_count` carries the
    /// total so a multi-GPU host is not silently misreported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gpu: Option<GpuOut>,
    gpu_count: u32,
}

#[derive(Debug, Serialize)]
struct GpuOut {
    name: String,
    vram_total_mb: u64,
    vram_used_mb: u64,
    utilization_pct: f64,
}

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
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit_service(
                caller.data(),
                "service.node_resources_get",
                "error",
                Some("input_read_failed"),
            );
            return e.as_i32();
        }
    };
    let input: NodeResourcesInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit_service(
                caller.data(),
                "service.node_resources_get",
                "error",
                Some("invalid_toml"),
            );
            return AbiError::Operation.as_i32();
        }
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
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// ---------------------------------------------------------------------------
// Helpers (private to this module)
// ---------------------------------------------------------------------------

fn read_input_toml(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    // Service list / node resources payloads are tiny — cap at the Secret
    // bucket (64 KiB) which is plenty for any conceivable filter combo.
    if enforce_payload_size(input_len as usize, PayloadKind::Secret).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    if input_len == 0 {
        return Ok(String::new());
    }
    let bytes = read_guest_bytes(memory, caller, input_ptr, input_len)
        .ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| AbiError::Operation)
}

fn write_toml_capped<T: Serialize>(
    memory: &super::super::runtime::WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let serialized = match toml::to_string(value) {
        Ok(s) => s,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    // Plan target: typical clusters carry under ~100 services. A 32 KiB cap
    // keeps the wire format honest; UI consumers paginate in JS if they
    // ever exceed it.
    if serialized.len() > 32 * 1024 {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(
        memory,
        caller,
        serialized.as_bytes(),
        out_ptr,
        out_cap,
        out_len_ptr,
    )
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
                let mut caps: Vec<String> =
                    s.models.iter().flat_map(|m| m.capabilities.clone()).collect();
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
    fn service_list_input_accepts_empty_toml() {
        let v: ServiceListInput = toml::from_str("").expect("empty toml parses");
        assert!(v.kind.is_none() && v.status.is_none() && v.node_id.is_none());
    }

    #[test]
    fn service_list_input_parses_filters() {
        let v: ServiceListInput =
            toml::from_str("kind = \"llm\"\nstatus = \"running\"\nnode_id = \"n1\"").unwrap();
        assert_eq!(v.kind.as_deref(), Some("llm"));
        assert_eq!(v.status.as_deref(), Some("running"));
        assert_eq!(v.node_id.as_deref(), Some("n1"));
    }

    #[test]
    fn service_list_output_serialises_minimal_shape() {
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
        let s = toml::to_string(&out).unwrap();
        assert!(s.contains("service_id = \"n1:7\""));
        assert!(s.contains("kind = \"vision\""));
        assert!(s.contains("capabilities = [\"detect\"]"));
    }

    #[test]
    fn node_resources_input_requires_node_id() {
        let parsed: Result<NodeResourcesInput, _> = toml::from_str("");
        assert!(parsed.is_err(), "node_id is mandatory");
    }
}
