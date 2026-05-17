// ============ File: addon/host_functions/vector.rs — Vector storage host functions (F1c P3) ============
//
// Three host functions exposing the embedded usearch backend to addons:
//
//   * `vector_upsert_v1(namespace, ref_id, vector_b64)` — insert/replace
//   * `vector_search_v1(namespace, query_b64, k, gate_claim_id?)` — k-NN
//   * `vector_delete_v1(namespace, ref_id)` — remove a single key
//
// Every call:
//   1. checks `vector.read` / `vector.write` permission,
//   2. validates the namespace name + payload sizes,
//   3. resolves dim/metric from the addon manifest (`[[vector_namespace]]`),
//   4. enforces per-addon quotas (namespace count + total vectors),
//   5. evaluates the gate placeholder when the namespace declares one,
//   6. delegates to `services::vector::NamespaceManager`,
//   7. emits a risk-classed audit row on every exit path.
//
// Wire format: vector payloads cross the ABI as base64-encoded
// little-endian f32 bytes. This keeps the existing string-pointer ABI
// (no new ptr/len pair for binary buffers) without bloating the encoded
// size beyond ~33 % over raw bytes.

#![allow(clippy::too_many_arguments)]

use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::addon::manifest::VectorNamespaceSpec;
use crate::audit::RiskClass;
use crate::services::vector::{
    namespace::validate_namespace_name, Metric, NamespaceManager, VectorError,
};

// =============================================================================
// Permission constants
// =============================================================================

const PERM_VECTOR_READ: &str = "vector.read";
const PERM_VECTOR_WRITE: &str = "vector.write";

// =============================================================================
// Input / output payloads (TOML on the wire — same convention as camera_*_v1)
// =============================================================================

#[derive(Debug, Deserialize)]
struct UpsertInput {
    namespace: String,
    ref_id: u64,
    /// Base64-encoded little-endian f32 vector bytes.
    vector_b64: String,
}

#[derive(Debug, Deserialize)]
struct SearchInput {
    namespace: String,
    query_b64: String,
    k: u32,
    /// Required only when the namespace declares a `gate` in the manifest.
    /// F1c P3 keeps this a placeholder — P4 (policy/claims engine) will
    /// resolve the claim id against `policy_claims` + `legal_grants`.
    #[serde(default)]
    gate_claim_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteInput {
    namespace: String,
    ref_id: u64,
}

#[derive(Debug, Serialize)]
struct UpsertOutput {
    namespace: String,
    ref_id: u64,
    count: u64,
}

#[derive(Debug, Serialize)]
struct SearchHitOut {
    ref_id: u64,
    score: f32,
}

#[derive(Debug, Serialize)]
struct SearchOutput {
    namespace: String,
    hits: Vec<SearchHitOut>,
}

#[derive(Debug, Serialize)]
struct DeleteOutput {
    namespace: String,
    ref_id: u64,
    removed: bool,
    count: u64,
}

// =============================================================================
// Shared helpers
// =============================================================================

/// Maximum vectors per search (`k`). 1000 is well above plausible product
/// queries (UI top-10/100) and well below anything that would stall the
/// HNSW search graph.
pub const MAX_SEARCH_K: u32 = 1000;

fn audit(
    state: &AddonState,
    action: &str,
    namespace: Option<&str>,
    risk: RiskClass,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("vector_namespace"),
        namespace,
        risk,
        None,
        None,
        result,
        reason,
    );
}

/// Reads a TOML payload from guest memory while enforcing the payload size
/// limit BEFORE materializing a `String` on the host heap. Vector payloads
/// fall under `PayloadKind::VectorItem` (1 MiB) which is wide enough for a
/// 4096-dim f32 vector plus the base64 overhead and TOML framing.
fn read_toml(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if enforce_payload_size(input_len as usize, PayloadKind::VectorItem).is_err() {
        return Err(AbiError::PayloadTooLarge);
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
    if enforce_payload_size(serialized.len(), PayloadKind::VectorItem).is_err() {
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

/// Decode a `base64(little-endian f32)` payload into a `Vec<f32>`. Rejects
/// payloads whose byte length is not a multiple of 4 (corrupted) or whose
/// element count exceeds 4096 (matches the namespace dim ceiling).
pub fn decode_vector(b64: &str) -> Result<Vec<f32>, &'static str> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|_| "vector_b64_invalid")?;
    if raw.is_empty() {
        return Err("vector_empty");
    }
    if raw.len() % 4 != 0 {
        return Err("vector_byte_length_not_multiple_of_4");
    }
    let count = raw.len() / 4;
    if count > 4096 {
        return Err("vector_too_many_elements");
    }
    let mut out = Vec::with_capacity(count);
    for chunk in raw.chunks_exact(4) {
        let arr: [u8; 4] = chunk.try_into().expect("chunks_exact(4) yields 4 bytes");
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

/// Locates the `[[vector_namespace]]` block in the addon manifest by name.
/// Addons MUST declare every namespace they read/write in their manifest —
/// this binds the namespace to a fixed dim/metric/gate at install time and
/// stops an addon from creating arbitrary ad-hoc namespaces at runtime.
fn lookup_namespace_spec<'a>(
    state: &'a AddonState,
    namespace: &str,
) -> Option<&'a VectorNamespaceSpec> {
    state
        .manifest
        .vector_namespaces
        .iter()
        .find(|v| v.name == namespace)
}

fn spec_metric(spec: &VectorNamespaceSpec) -> Result<Metric, &'static str> {
    Metric::parse(&spec.distance).ok_or("invalid_metric_in_manifest")
}

/// Gate evaluation placeholder. P3 enforces only the structural rule: if the
/// namespace declares a gate, the caller MUST supply a non-empty
/// `gate_claim_id` in the search request. The actual claim validation against
/// `policy_claims` + `policy_claim_signatures` lands in P4. Returning
/// `GateNotSatisfied` now means callers wire claim plumbing today rather than
/// after a silent contract change at P4.
pub fn check_gate(
    spec: &VectorNamespaceSpec,
    claim_id: Option<&str>,
) -> Result<(), AbiError> {
    let Some(_gate_id) = spec.gate.as_deref() else {
        return Ok(());
    };
    match claim_id {
        Some(c) if !c.is_empty() => Ok(()),
        _ => Err(AbiError::GateNotSatisfied),
    }
}

/// Translates a `VectorError` into the (AbiError, audit_reason) pair we
/// surface to the caller. Quota / dim mismatch / metric mismatch get
/// dedicated AbiError codes so addons can react programmatically.
pub fn map_vector_error(e: VectorError) -> (AbiError, &'static str) {
    match e {
        VectorError::NamespaceNotFound { .. } => (AbiError::NotFound, "namespace_not_found"),
        VectorError::NamespaceExists { .. } => (AbiError::Conflict, "namespace_exists"),
        VectorError::DimMismatch { .. } => (AbiError::Operation, "dim_mismatch"),
        VectorError::InvalidDim(_) => (AbiError::Operation, "invalid_dim"),
        VectorError::MetricMismatch { .. } => (AbiError::Operation, "metric_mismatch"),
        VectorError::InvalidNamespaceName(_) => (AbiError::Operation, "invalid_namespace_name"),
        VectorError::InvalidRefId => (AbiError::Operation, "invalid_ref_id"),
        VectorError::EmptyVector => (AbiError::Operation, "empty_vector"),
        VectorError::NamespaceQuotaExceeded { .. } => {
            (AbiError::QuotaExceeded, "namespace_quota_exceeded")
        }
        VectorError::VectorQuotaExceeded { .. } => {
            (AbiError::QuotaExceeded, "vector_quota_exceeded")
        }
        VectorError::Io { .. } => (AbiError::Operation, "vector_io_error"),
        VectorError::Backend(_) => (AbiError::Operation, "vector_backend_error"),
        VectorError::Db(_) => (AbiError::Operation, "vector_db_error"),
    }
}

fn manager(state: &AddonState) -> &'static std::sync::Arc<NamespaceManager> {
    crate::services::vector_namespace_manager(&state.db)
}

// =============================================================================
// Host function: vector_upsert_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input TOML: `namespace`, `ref_id`, `vector_b64` (base64 of LE f32 bytes).
/// Output TOML: `namespace`, `ref_id`, `count` (post-upsert vector count).
/// Requires `vector.write` permission. Risk class B — embeddings of regulated
/// data classes (faces / persons) flow through here.
pub fn vector_upsert_v1(
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

    let toml_str = match read_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "vector.upsert",
                None,
                RiskClass::B,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let input: UpsertInput = match toml::from_str(&toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "vector.upsert",
                None,
                RiskClass::B,
                "denied",
                Some("toml_parse_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_VECTOR_WRITE, None) {
        audit(
            caller.data(),
            "vector.upsert",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    if let Err(_e) = validate_namespace_name(&input.namespace) {
        audit(
            caller.data(),
            "vector.upsert",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("invalid_namespace_name"),
        );
        return AbiError::Operation.as_i32();
    }

    let spec = match lookup_namespace_spec(caller.data(), &input.namespace) {
        Some(s) => s.clone(),
        None => {
            audit(
                caller.data(),
                "vector.upsert",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some("namespace_not_declared_in_manifest"),
            );
            return AbiError::NotFound.as_i32();
        }
    };

    let metric = match spec_metric(&spec) {
        Ok(m) => m,
        Err(reason) => {
            audit(
                caller.data(),
                "vector.upsert",
                Some(&input.namespace),
                RiskClass::B,
                "error",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let vector = match decode_vector(&input.vector_b64) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                caller.data(),
                "vector.upsert",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let mgr = manager(caller.data()).clone();

    if let Err(e) = mgr.check_vector_quota(&addon_id) {
        let (abi, reason) = map_vector_error(e);
        audit(
            caller.data(),
            "vector.upsert",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some(reason),
        );
        return abi.as_i32();
    }

    let backend = match mgr.get_or_create(&addon_id, &input.namespace, spec.dimensions, metric) {
        Ok(b) => b,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.upsert",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    if let Err(e) = backend.upsert(input.ref_id, &vector) {
        let (abi, reason) = map_vector_error(e);
        audit(
            caller.data(),
            "vector.upsert",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some(reason),
        );
        return abi.as_i32();
    }

    if let Err(e) = backend.save() {
        warn!(
            "vector.upsert: save failed for {}/{}: {:?}",
            addon_id, input.namespace, e
        );
        let (abi, reason) = map_vector_error(e);
        audit(
            caller.data(),
            "vector.upsert",
            Some(&input.namespace),
            RiskClass::B,
            "error",
            Some(reason),
        );
        return abi.as_i32();
    }

    let count = backend.count();
    let _ = mgr.update_count(&addon_id, &input.namespace, count);

    audit(
        caller.data(),
        "vector.upsert",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = UpsertOutput {
        namespace: input.namespace,
        ref_id: input.ref_id,
        count,
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: vector_search_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input TOML: `namespace`, `query_b64`, `k`, optional `gate_claim_id`.
/// Output TOML: `namespace`, `hits = [{ref_id, score}, ...]` (top-k, closest
/// first). Requires `vector.read` permission. Risk class B.
pub fn vector_search_v1(
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

    let toml_str = match read_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "vector.search",
                None,
                RiskClass::B,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let input: SearchInput = match toml::from_str(&toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "vector.search",
                None,
                RiskClass::B,
                "denied",
                Some("toml_parse_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_VECTOR_READ, None) {
        audit(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    if input.k == 0 || input.k > MAX_SEARCH_K {
        audit(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("invalid_k"),
        );
        return AbiError::Operation.as_i32();
    }

    if validate_namespace_name(&input.namespace).is_err() {
        audit(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("invalid_namespace_name"),
        );
        return AbiError::Operation.as_i32();
    }

    let spec = match lookup_namespace_spec(caller.data(), &input.namespace) {
        Some(s) => s.clone(),
        None => {
            audit(
                caller.data(),
                "vector.search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some("namespace_not_declared_in_manifest"),
            );
            return AbiError::NotFound.as_i32();
        }
    };

    if let Err(e) = check_gate(&spec, input.gate_claim_id.as_deref()) {
        audit(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("gate_not_satisfied"),
        );
        return e.as_i32();
    }

    let query = match decode_vector(&input.query_b64) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                caller.data(),
                "vector.search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let mgr = manager(caller.data()).clone();

    // F1c P3 search semantics: if the addon has never written to this
    // namespace we treat the read as "namespace exists in manifest but no
    // data" — return zero hits rather than NotFound. NotFound is reserved
    // for namespaces that are not declared in the manifest at all (caught
    // above).
    let metric = match spec_metric(&spec) {
        Ok(m) => m,
        Err(reason) => {
            audit(
                caller.data(),
                "vector.search",
                Some(&input.namespace),
                RiskClass::B,
                "error",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let backend = match mgr.get_or_create(&addon_id, &input.namespace, spec.dimensions, metric) {
        Ok(b) => b,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    let hits = match backend.search(&query, input.k as usize) {
        Ok(h) => h,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    audit(
        caller.data(),
        "vector.search",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = SearchOutput {
        namespace: input.namespace,
        hits: hits
            .into_iter()
            .map(|h| SearchHitOut {
                ref_id: h.ref_id,
                score: h.score,
            })
            .collect(),
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: vector_delete_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input TOML: `namespace`, `ref_id`. Output TOML: `namespace`, `ref_id`,
/// `removed` (true if the key existed), `count`. Requires `vector.write`.
pub fn vector_delete_v1(
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

    let toml_str = match read_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "vector.delete",
                None,
                RiskClass::B,
                "denied",
                Some("payload_invalid"),
            );
            return e.as_i32();
        }
    };

    let input: DeleteInput = match toml::from_str(&toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(
                caller.data(),
                "vector.delete",
                None,
                RiskClass::B,
                "denied",
                Some("toml_parse_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_VECTOR_WRITE, None) {
        audit(
            caller.data(),
            "vector.delete",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    if validate_namespace_name(&input.namespace).is_err() {
        audit(
            caller.data(),
            "vector.delete",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("invalid_namespace_name"),
        );
        return AbiError::Operation.as_i32();
    }

    if lookup_namespace_spec(caller.data(), &input.namespace).is_none() {
        audit(
            caller.data(),
            "vector.delete",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("namespace_not_declared_in_manifest"),
        );
        return AbiError::NotFound.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let mgr = manager(caller.data()).clone();

    let backend = match mgr.get(&addon_id, &input.namespace) {
        Ok(b) => b,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.delete",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    let removed = match backend.delete(input.ref_id) {
        Ok(b) => b,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.delete",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    if removed {
        if let Err(e) = backend.save() {
            warn!(
                "vector.delete: save failed for {}/{}: {:?}",
                addon_id, input.namespace, e
            );
        }
        let count = backend.count();
        let _ = mgr.update_count(&addon_id, &input.namespace, count);
    }

    audit(
        caller.data(),
        "vector.delete",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = DeleteOutput {
        namespace: input.namespace,
        ref_id: input.ref_id,
        removed,
        count: backend.count(),
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Public test surface — invoked from `tests/vector_host_functions.rs`
// =============================================================================

/// Re-exports the decode/gate helpers so integration tests can exercise the
/// validation path without spinning up a wasmtime store. Marked
/// `#[doc(hidden)]` — not part of the addon-facing API.
#[doc(hidden)]
pub mod test_api {
    pub use super::{check_gate, decode_vector, map_vector_error, MAX_SEARCH_K};
}
