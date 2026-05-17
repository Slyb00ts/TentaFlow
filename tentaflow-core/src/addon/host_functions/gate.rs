// ============ File: addon/host_functions/gate.rs — F1c P4 gate_check_v1 ============
//
// Addon-facing API for verifying a policy claim *before* the addon attempts
// the gated operation. Useful when the addon wants to short-circuit an
// expensive ML pipeline (face re-id, attribute search) without first
// kicking off a vector_search that would itself reject on the same claim.
//
// Wire format (TOML in/out):
//   Input:  gate_id = "<id from manifest [[gate]]>", claim_id = "<claim>"
//   Output: { valid = bool, claim_type = "...", valid_until = "...",
//             signers = [{role, user}, ...], reason = "..." (when invalid) }
//
// Requires `policy.read` permission.
// Risk class B — read-only inspection of a regulated policy artifact.

use serde::{Deserialize, Serialize};

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::addon::manifest::{ClaimRequirement, GateSpec};
use crate::audit::RiskClass;
use crate::services::policy::{verify_claim, ClaimContext, PolicyError, SignerEntry};

const PERM_POLICY_READ: &str = "policy.read";

#[derive(Debug, Deserialize)]
struct GateCheckInput {
    gate_id: String,
    claim_id: String,
    /// Optional resource scope hint — when the gated namespace narrowing is
    /// pinned to a specific resource (e.g. `faces` vector namespace).
    #[serde(default)]
    resource_scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct GateCheckOutput {
    valid: bool,
    claim_id: String,
    claim_type: String,
    valid_until: String,
    signers: Vec<SignerEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn audit(
    state: &AddonState,
    gate_id: Option<&str>,
    claim_id: Option<&str>,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        "policy.gate_check",
        Some("gate"),
        gate_id,
        RiskClass::B,
        claim_id,
        None,
        result,
        reason,
    );
}

/// Resolves required signer roles from a gate's `required_claims` block.
/// Convention used by F1c-P4: every claim requirement with `type = "approval"`
/// + `subject = "<role>"` is treated as a required signer role. When the gate
/// declares none, the engine defaults to `["dpo"]` (DPIA artifact always
/// requires a Data Protection Officer signature under the F1c policy model).
pub fn required_signer_roles_for_gate(gate: &GateSpec) -> Vec<String> {
    let mut roles: Vec<String> = gate
        .required_claims
        .iter()
        .filter_map(|c: &ClaimRequirement| {
            if c.claim_type == "approval" {
                c.subject.clone()
            } else {
                None
            }
        })
        .collect();
    if roles.is_empty() {
        roles.push("dpo".to_string());
    }
    roles.sort();
    roles.dedup();
    roles
}

/// Pulls the dominant claim type required by the gate. When multiple
/// `required_claims` are listed, the engine prefers the first non-"approval"
/// type (the artifact type — e.g. "dpia", "consent") so the engine can match
/// against `policy_claims.claim_type`. Falls back to the first declared type
/// or to "dpia" when the gate has none.
pub fn primary_claim_type_for_gate(gate: &GateSpec) -> String {
    if let Some(c) = gate
        .required_claims
        .iter()
        .find(|c| c.claim_type != "approval")
    {
        return c.claim_type.clone();
    }
    if let Some(c) = gate.required_claims.first() {
        return c.claim_type.clone();
    }
    "dpia".to_string()
}

/// Builds a `ClaimContext` from a manifest gate + caller identity. Pulled
/// out for reuse by `vector_search_v1` enforcement (P4 §B.5).
pub fn build_context(
    addon_id: &str,
    gate: &GateSpec,
    resource_scope: Option<&str>,
) -> ClaimContext {
    ClaimContext {
        addon_id: addon_id.to_string(),
        claim_type_required: primary_claim_type_for_gate(gate),
        resource_scope: resource_scope.map(String::from),
        required_signer_roles: required_signer_roles_for_gate(gate),
        now_utc: chrono::Utc::now().to_rfc3339(),
    }
}

/// Looks up a `[[gate]]` entry by id from the addon manifest.
pub fn lookup_gate<'a>(state: &'a AddonState, gate_id: &str) -> Option<&'a GateSpec> {
    state.manifest.gates.iter().find(|g| g.id == gate_id)
}

/// Maps a `PolicyError` to a short, audit-friendly reason code and an
/// addon-facing message. Reason code drives audit `details`; message goes
/// into the TOML output so the addon can surface it to the user.
pub fn policy_error_to_reason(err: &PolicyError) -> (&'static str, String) {
    match err {
        PolicyError::ClaimNotFound(_) => ("claim_not_found", err.to_string()),
        PolicyError::ClaimRevoked { .. } => ("claim_revoked", err.to_string()),
        PolicyError::ClaimNotInValidityPeriod { .. } => {
            ("claim_outside_validity", err.to_string())
        }
        PolicyError::ClaimTypeMismatch { .. } => ("claim_type_mismatch", err.to_string()),
        PolicyError::ClaimScopeMismatch { .. } => ("claim_scope_mismatch", err.to_string()),
        PolicyError::MissingRequiredSigner { .. } => {
            ("missing_required_signer", err.to_string())
        }
        PolicyError::DbError(_) => ("policy_db_error", err.to_string()),
    }
}

fn read_toml(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    // Gate payloads are tiny — reuse the small KV bucket to stay well
    // under the generic 64 KiB ceiling.
    if enforce_payload_size(input_len as usize, PayloadKind::Secret).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes = read_guest_bytes(memory, caller, input_ptr, input_len)
        .ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| AbiError::Operation)
}

fn write_toml<T: Serialize>(
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
    if enforce_payload_size(serialized.len(), PayloadKind::Secret).is_err() {
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

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Returns AbiError::Ok on success. The TOML body always carries `valid`
/// (true / false) — a false outcome is NOT mapped to AbiError::GateNotSatisfied
/// because the addon explicitly asked for an inspection. Hard ABI errors
/// (permission missing, gate id not in manifest, payload malformed) still
/// return the matching AbiError code so the SDK wrapper can distinguish
/// "call shape was wrong" from "claim was rejected on policy grounds".
pub fn gate_check_v1(
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
            audit(caller.data(), None, None, "denied", Some("payload_invalid"));
            return e.as_i32();
        }
    };

    let input: GateCheckInput = match toml::from_str(&toml_str) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), None, None, "denied", Some("toml_parse_error"));
            return AbiError::Operation.as_i32();
        }
    };

    if !check_permission(caller.data(), PERM_POLICY_READ, None) {
        audit(
            caller.data(),
            Some(&input.gate_id),
            Some(&input.claim_id),
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let gate_id = input.gate_id.clone();
    let claim_id = input.claim_id.clone();
    let resource_scope = input.resource_scope.clone();

    let gate = match lookup_gate(caller.data(), &gate_id) {
        Some(g) => g.clone(),
        None => {
            audit(
                caller.data(),
                Some(&gate_id),
                Some(&claim_id),
                "denied",
                Some("gate_not_declared_in_manifest"),
            );
            return AbiError::NotFound.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let pool = caller.data().db.clone();
    let ctx = build_context(&addon_id, &gate, resource_scope.as_deref());

    let result = verify_claim(&pool, &claim_id, &ctx);
    match result {
        Ok(verified) => {
            audit(
                caller.data(),
                Some(&gate_id),
                Some(&claim_id),
                "gate_ok",
                None,
            );
            let out = GateCheckOutput {
                valid: true,
                claim_id: verified.claim_id,
                claim_type: verified.claim_type,
                valid_until: verified.valid_until,
                signers: verified.signers,
                reason: None,
            };
            write_toml(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
        Err(e) => {
            let (reason_code, message) = policy_error_to_reason(&e);
            audit(
                caller.data(),
                Some(&gate_id),
                Some(&claim_id),
                "gate_denied",
                Some(reason_code),
            );
            let out = GateCheckOutput {
                valid: false,
                claim_id: claim_id.clone(),
                claim_type: String::new(),
                valid_until: String::new(),
                signers: Vec::new(),
                reason: Some(message),
            };
            write_toml(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::manifest::{ClaimRequirement, GateSpec};

    fn make_gate(claims: Vec<ClaimRequirement>) -> GateSpec {
        GateSpec {
            id: "d4-historical".to_string(),
            display_name: "D4 Historical Face Search".to_string(),
            required_claims: claims,
        }
    }

    fn req(claim_type: &str, subject: Option<&str>) -> ClaimRequirement {
        ClaimRequirement {
            claim_type: claim_type.to_string(),
            subject: subject.map(String::from),
            scope: None,
            status: None,
            value: None,
            oneof: Vec::new(),
            valid: None,
            has_expiry: None,
        }
    }

    #[test]
    fn primary_claim_type_picks_non_approval() {
        let g = make_gate(vec![
            req("approval", Some("dpo")),
            req("dpia", None),
        ]);
        assert_eq!(primary_claim_type_for_gate(&g), "dpia");
    }

    #[test]
    fn primary_claim_type_defaults_when_empty() {
        let g = make_gate(vec![]);
        assert_eq!(primary_claim_type_for_gate(&g), "dpia");
    }

    #[test]
    fn required_signer_roles_from_approvals() {
        let g = make_gate(vec![
            req("approval", Some("dpo")),
            req("approval", Some("supervisor")),
            req("dpia", None),
        ]);
        let roles = required_signer_roles_for_gate(&g);
        assert_eq!(roles, vec!["dpo".to_string(), "supervisor".to_string()]);
    }

    #[test]
    fn required_signer_roles_default_to_dpo() {
        let g = make_gate(vec![req("dpia", None)]);
        let roles = required_signer_roles_for_gate(&g);
        assert_eq!(roles, vec!["dpo".to_string()]);
    }
}
