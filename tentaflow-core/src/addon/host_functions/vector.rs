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
use tentaflow_sdk_spec::{
    FieldSpec, FieldType, Fusion, VectorDeleteInput, VectorDeleteOutput, VectorHybridSearchInput,
    VectorSearchHit, VectorSearchInput, VectorSearchOutput, VectorUpsertInput, VectorUpsertOutput,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
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
    audit_with_claim(state, action, namespace, risk, result, reason, None);
}

/// Variant of `audit` that links the row to a `policy_claims.claim_id` via
/// the audit chain `related_claim_id` column. Used by gate-denial paths so
/// the compliance audit shows *which* claim was rejected, not just that a
/// gate denied (data minimization rule for F1c P4).
fn audit_with_claim(
    state: &AddonState,
    action: &str,
    namespace: Option<&str>,
    risk: RiskClass,
    result: &str,
    reason: Option<&str>,
    related_claim_id: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("vector_namespace"),
        namespace,
        risk,
        related_claim_id,
        None,
        result,
        reason,
    );
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

/// Translate the manifest's declared metadata fields into the universal
/// `FieldSpec` schema the backend understands. An unknown `type` string is a
/// manifest error surfaced to the caller (so a typo fails loudly at runtime
/// rather than silently dropping the column).
fn spec_fields(spec: &VectorNamespaceSpec) -> Result<Vec<FieldSpec>, &'static str> {
    spec.fields
        .iter()
        .map(|f| {
            let field_type = match f.field_type.as_str() {
                "str" => FieldType::Str,
                "int" => FieldType::Int,
                "float" => FieldType::Float,
                "bool" => FieldType::Bool,
                _ => return Err("invalid_field_type_in_manifest"),
            };
            Ok(FieldSpec {
                name: f.name.clone(),
                field_type,
                indexed: f.indexed,
            })
        })
        .collect()
}

/// Structural gate check — kept as the first defence so callers always
/// supply `gate_claim_id` for a gated namespace before we even touch the
/// policy DB. Full claim validation (DPIA / FRIA / signers / validity
/// window) happens in `enforce_gate_with_policy` below, which delegates to
/// `services::policy::verify_claim` against the addon manifest `[[gate]]`
/// entry referenced by `spec.gate`.
pub fn check_gate(spec: &VectorNamespaceSpec, claim_id: Option<&str>) -> Result<(), AbiError> {
    let Some(_gate_id) = spec.gate.as_deref() else {
        return Ok(());
    };
    match claim_id {
        Some(c) if !c.is_empty() => Ok(()),
        _ => Err(AbiError::GateNotSatisfied),
    }
}

/// Outcome of `enforce_gate_with_policy` on the deny path. Carries the
/// short audit reason code plus the `claim_id` that was attempted (when
/// the addon supplied one) so the caller can link the audit row to the
/// rejected claim via `related_claim_id`. `claim_id` is `None` only when
/// the gated namespace was called without any claim id at all.
#[derive(Debug)]
pub struct GateDenial {
    pub abi: AbiError,
    pub reason: &'static str,
    pub attempted_claim_id: Option<String>,
}

/// Full policy enforcement for a gated namespace. Returns Ok with the
/// `claim_id` that satisfied the gate (or `None` when the namespace has
/// no `gate` field). On rejection returns `GateDenial` with the audit
/// reason and the attempted claim id (if any) so the caller can emit a
/// `gate_denied` audit row tied to the claim via `related_claim_id`.
pub fn enforce_gate_with_policy(
    state: &AddonState,
    spec: &VectorNamespaceSpec,
    claim_id: Option<&str>,
) -> Result<Option<String>, GateDenial> {
    let Some(gate_id) = spec.gate.as_deref() else {
        return Ok(None);
    };
    let claim_id = match claim_id {
        Some(c) if !c.is_empty() => c,
        _ => {
            return Err(GateDenial {
                abi: AbiError::GateNotSatisfied,
                reason: "gate_claim_id_missing",
                attempted_claim_id: None,
            });
        }
    };
    let gate = match super::gate::lookup_gate(state, gate_id) {
        Some(g) => g.clone(),
        None => {
            return Err(GateDenial {
                abi: AbiError::NotFound,
                reason: "gate_not_declared_in_manifest",
                attempted_claim_id: Some(claim_id.to_string()),
            });
        }
    };
    let ctx = super::gate::build_context(
        &state.addon_id,
        state.org_id.as_deref(),
        &gate,
        Some(&spec.name),
    );
    match crate::services::policy::verify_claim(&state.db, claim_id, &ctx) {
        Ok(_) => Ok(Some(claim_id.to_string())),
        Err(e) => {
            let (reason, _) = super::gate::policy_error_to_reason(&e);
            Err(GateDenial {
                abi: AbiError::GateNotSatisfied,
                reason,
                attempted_claim_id: Some(claim_id.to_string()),
            })
        }
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
        VectorError::InvalidFilter(_) => (AbiError::Operation, "invalid_filter"),
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
/// Input CBOR: `namespace`, `ref_id`, `vector_b64` (base64 of LE f32 bytes).
/// Output CBOR: `namespace`, `ref_id`, `count` (post-upsert vector count).
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

    if !check_permission(caller.data(), PERM_VECTOR_WRITE, None) {
        audit(
            caller.data(),
            "vector.upsert",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: VectorUpsertInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::VectorItem,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "vector.upsert",
                None,
                RiskClass::B,
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

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

    let field_specs = match spec_fields(&spec) {
        Ok(f) => f,
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
    let field_values = input.fields.unwrap_or_default();

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
    let org_id_for_query = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let mgr = manager(caller.data()).clone();

    // Wymiar przestrzeni bierzemy z RZECZYWISTEGO wektora, nie z manifestu.
    // Model embeddingu jest konfigurowalny (np. RAG: jina=1024d / nemotron=2048d),
    // wiec `spec.dimensions` z manifestu nie moze pinowac przestrzeni — robi to ta
    // sama sciezka co flow-store (tworzy namespace na dlugosci wektora). Manifest
    // nadal DEKLARUJE namespace (gate dostepu wyzej), ale jego `dimensions` jest
    // pogladowe. get_or_create i tak kapuje wymiar do 1..=4096, a istniejacy
    // namespace o innym wymiarze zwraca DimMismatch (zmiana modelu => nowa kolekcja).
    let dim = u32::try_from(vector.len()).unwrap_or(u32::MAX);
    // upsert_with_quota holds an IMMEDIATE SQLite transaction across the
    // quota check + backend insert + count UPDATE, so two concurrent
    // upserts cannot both pass the cap. The backend persists internally,
    // so a successful return implies a durable write.
    let count = match mgr.upsert_with_quota(
        &org_id_for_query,
        &addon_id,
        &input.namespace,
        input.ref_id,
        &vector,
        dim,
        metric,
        &field_specs,
        &field_values,
        spec.sparse,
        input.sparse.as_ref(),
    ) {
        Ok(c) => c,
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

    audit(
        caller.data(),
        "vector.upsert",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = VectorUpsertOutput {
        namespace: input.namespace,
        ref_id: input.ref_id,
        count,
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::VectorItem,
    )
}

// =============================================================================
// Host function: vector_search_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input CBOR: `namespace`, `query_b64`, `k`, optional `gate_claim_id`.
/// Output CBOR: `namespace`, `hits = [{ref_id, score}, ...]` (top-k, closest
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

    if !check_permission(caller.data(), PERM_VECTOR_READ, None) {
        audit(
            caller.data(),
            "vector.search",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: VectorSearchInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::VectorItem,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "vector.search",
                None,
                RiskClass::B,
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

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

    if let Err(denial) =
        enforce_gate_with_policy(caller.data(), &spec, input.gate_claim_id.as_deref())
    {
        audit_with_claim(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "gate_denied",
            Some(denial.reason),
            denial.attempted_claim_id.as_deref(),
        );
        return denial.abi.as_i32();
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
    let org_id_for_query = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let mgr = manager(caller.data()).clone();

    // Read path: validate spec metric matches the on-disk geometry but do
    // NOT create the namespace. Searching a namespace the addon never wrote
    // to returns an empty hit list (the manifest declares it, but no data
    // landed yet) rather than spawning a DB row + on-disk file from a
    // vector.read-permission call.
    let _ = match spec_metric(&spec) {
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
    let backend = match mgr.get(&org_id_for_query, &addon_id, &input.namespace) {
        Ok(b) => Some(b),
        Err(VectorError::NamespaceNotFound { .. }) => None,
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

    let Some(backend) = backend else {
        audit(
            caller.data(),
            "vector.search",
            Some(&input.namespace),
            RiskClass::B,
            "ok",
            Some("namespace_empty"),
        );
        let out = VectorSearchOutput {
            namespace: input.namespace,
            hits: Vec::new(),
        };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::VectorItem,
        );
    };

    let output_fields = input.output_fields.unwrap_or_default();
    let hits = match backend.search(
        &query,
        input.k as usize,
        input.filter.as_ref(),
        &output_fields,
    ) {
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

    let out = VectorSearchOutput {
        namespace: input.namespace,
        hits: hits
            .into_iter()
            .map(|h| VectorSearchHit {
                ref_id: h.ref_id,
                score: h.score,
                fields: if h.fields.is_empty() {
                    None
                } else {
                    Some(h.fields)
                },
            })
            .collect(),
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::VectorItem,
    )
}

// =============================================================================
// Host function: vector_hybrid_search_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Hybrid dense + sparse k-NN. Input CBOR: `namespace`, `dense_b64`, `sparse`,
/// `k`, optional `gate_claim_id`, `filter`, `output_fields`, `fusion`. Output is
/// the same `VectorSearchOutput` as `vector_search_v1`. Requires `vector.read`
/// and the namespace must declare `sparse = true`. Risk class B.
pub fn vector_hybrid_search_v1(
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

    if !check_permission(caller.data(), PERM_VECTOR_READ, None) {
        audit(
            caller.data(),
            "vector.hybrid_search",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: VectorHybridSearchInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::VectorItem,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "vector.hybrid_search",
                None,
                RiskClass::B,
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

    if input.k == 0 || input.k > MAX_SEARCH_K {
        audit(
            caller.data(),
            "vector.hybrid_search",
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
            "vector.hybrid_search",
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
                "vector.hybrid_search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some("namespace_not_declared_in_manifest"),
            );
            return AbiError::NotFound.as_i32();
        }
    };

    if !spec.sparse {
        audit(
            caller.data(),
            "vector.hybrid_search",
            Some(&input.namespace),
            RiskClass::B,
            "denied",
            Some("namespace_not_sparse"),
        );
        return AbiError::Operation.as_i32();
    }

    if let Err(denial) =
        enforce_gate_with_policy(caller.data(), &spec, input.gate_claim_id.as_deref())
    {
        audit_with_claim(
            caller.data(),
            "vector.hybrid_search",
            Some(&input.namespace),
            RiskClass::B,
            "gate_denied",
            Some(denial.reason),
            denial.attempted_claim_id.as_deref(),
        );
        return denial.abi.as_i32();
    }

    let dense = match decode_vector(&input.dense_b64) {
        Ok(v) => v,
        Err(reason) => {
            audit(
                caller.data(),
                "vector.hybrid_search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id_for_query = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let mgr = manager(caller.data()).clone();

    let backend = match mgr.get(&org_id_for_query, &addon_id, &input.namespace) {
        Ok(b) => Some(b),
        Err(VectorError::NamespaceNotFound { .. }) => None,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.hybrid_search",
                Some(&input.namespace),
                RiskClass::B,
                "denied",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    let Some(backend) = backend else {
        audit(
            caller.data(),
            "vector.hybrid_search",
            Some(&input.namespace),
            RiskClass::B,
            "ok",
            Some("namespace_empty"),
        );
        let out = VectorSearchOutput {
            namespace: input.namespace,
            hits: Vec::new(),
        };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::VectorItem,
        );
    };

    let output_fields = input.output_fields.unwrap_or_default();
    let fusion = input.fusion.unwrap_or(Fusion::Rrf(60));
    let hits = match backend.hybrid_search(
        &dense,
        &input.sparse,
        input.k as usize,
        input.filter.as_ref(),
        &output_fields,
        fusion,
    ) {
        Ok(h) => h,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.hybrid_search",
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
        "vector.hybrid_search",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = VectorSearchOutput {
        namespace: input.namespace,
        hits: hits
            .into_iter()
            .map(|h| VectorSearchHit {
                ref_id: h.ref_id,
                score: h.score,
                fields: if h.fields.is_empty() {
                    None
                } else {
                    Some(h.fields)
                },
            })
            .collect(),
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::VectorItem,
    )
}

// =============================================================================
// Host function: vector_delete_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Input CBOR: `namespace`, `ref_id`. Output CBOR: `namespace`, `ref_id`,
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

    if !check_permission(caller.data(), PERM_VECTOR_WRITE, None) {
        audit(
            caller.data(),
            "vector.delete",
            None,
            RiskClass::B,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let input: VectorDeleteInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::VectorItem,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "vector.delete",
                None,
                RiskClass::B,
                "denied",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };

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
    let org_id_for_query = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let mgr = manager(caller.data()).clone();

    // Delete is idempotent at the namespace level: a delete on a namespace
    // that was never written to is reported as removed=false, count=0
    // rather than NotFound — matches REST DELETE semantics and lets addons
    // call this without first checking existence.
    let backend = match mgr.get(&org_id_for_query, &addon_id, &input.namespace) {
        Ok(b) => Some(b),
        Err(VectorError::NamespaceNotFound { .. }) => None,
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

    let Some(backend) = backend else {
        audit(
            caller.data(),
            "vector.delete",
            Some(&input.namespace),
            RiskClass::B,
            "ok",
            Some("namespace_empty"),
        );
        let out = VectorDeleteOutput {
            namespace: input.namespace,
            ref_id: input.ref_id,
            removed: false,
            count: 0,
        };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::VectorItem,
        );
    };

    // backend.delete() persists internally before returning Ok — a success
    // implies durability. Failure here propagates upstream rather than
    // returning success with a non-durable in-memory delete.
    let removed = match backend.delete(input.ref_id) {
        Ok(b) => b,
        Err(e) => {
            let (abi, reason) = map_vector_error(e);
            audit(
                caller.data(),
                "vector.delete",
                Some(&input.namespace),
                RiskClass::B,
                "error",
                Some(reason),
            );
            return abi.as_i32();
        }
    };

    let count = backend.count();
    if removed {
        let _ = mgr.update_count(&org_id_for_query, &addon_id, &input.namespace, count);
    }

    audit(
        caller.data(),
        "vector.delete",
        Some(&input.namespace),
        RiskClass::B,
        "ok",
        None,
    );

    let out = VectorDeleteOutput {
        namespace: input.namespace,
        ref_id: input.ref_id,
        removed,
        count,
    };
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::VectorItem,
    )
}

// =============================================================================
// Public test surface — invoked from `tests/vector_host_functions.rs`
// =============================================================================

/// Re-exports the decode/gate helpers so integration tests can exercise the
/// validation path without spinning up a wasmtime store. Marked
/// `#[doc(hidden)]` — not part of the addon-facing API.
#[doc(hidden)]
pub mod test_api {
    pub use super::{
        check_gate, decode_vector, enforce_gate_with_policy, map_vector_error, GateDenial,
        MAX_SEARCH_K,
    };
}
