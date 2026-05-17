// =============================================================================
// File: tests/vector_gate_enforcement.rs
// Purpose: F1c P4 integration — verifies that a gated vector namespace
//          (manifest [[vector_namespace]] with `gate=...`) is correctly
//          enforced against the policy engine via `enforce_gate_with_policy`.
//          P3 only validated structural presence; P4 validates the claim
//          end-to-end (validity window, signers, scope).
// =============================================================================

#![cfg(feature = "vector")]

use std::sync::Arc;

use tempfile::TempDir;

use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::host_functions::gate as host_gate;
use tentaflow_core::addon::host_functions::vector::test_api::enforce_gate_with_policy;
use tentaflow_core::addon::manifest::{ClaimRequirement, GateSpec, VectorNamespaceSpec};
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::services::policy::{self, NewClaim, NewSignature};

fn open_pool() -> (TempDir, tentaflow_core::db::DbPool) {
    let d = TempDir::new().unwrap();
    let p = d.path().join("vge.db");
    let pool = tentaflow_core::db::init(&p).unwrap();
    (d, pool)
}

fn gate(id: &str) -> GateSpec {
    GateSpec {
        id: id.to_string(),
        display_name: id.to_string(),
        required_claims: vec![
            ClaimRequirement {
                claim_type: "approval".into(),
                subject: Some("dpo".into()),
                scope: None,
                status: None,
                value: None,
                oneof: Vec::new(),
                valid: None,
                has_expiry: None,
            },
            ClaimRequirement {
                claim_type: "dpia".into(),
                subject: None,
                scope: None,
                status: None,
                value: None,
                oneof: Vec::new(),
                valid: None,
                has_expiry: None,
            },
        ],
    }
}

fn vector_spec(name: &str, gate_id: Option<&str>) -> VectorNamespaceSpec {
    VectorNamespaceSpec {
        name: name.to_string(),
        dimensions: 128,
        distance: "cosine".to_string(),
        data_class: "B".to_string(),
        gate: gate_id.map(String::from),
    }
}

/// Builds a barebones AddonState — only the fields touched by gate
/// enforcement (`db`, `addon_id`, `manifest.gates`) carry meaningful values.
fn make_state(
    pool: tentaflow_core::db::DbPool,
    addon_id: &str,
    gates: Vec<GateSpec>,
) -> AddonState {
    use parking_lot::Mutex;

    let mut manifest = AddonManifest::default();
    manifest.gates = gates;
    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: "inst-test".to_string(),
        user_id: None,
        db: pool.clone(),
        permissions: Vec::new(),
        event_bus: Arc::new(tentaflow_core::addon::event_bus::EventBus::new()),
        permission_checker: Arc::new(
            tentaflow_core::addon::permissions::PermissionChecker::new(pool),
        ),
        fuel_consumed: 0,
        is_system_call: true,
        rate_limiter: None,
        net_manager: Arc::new(Mutex::new(
            tentaflow_core::addon::host_functions::network::NetworkConnectionManager::new(),
        )),
        settings_cipher: Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(manifest),
        memory_limit: 64 * 1024 * 1024,
        router: None,
        oauth_refresh_guard: Arc::new(
            tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
        ),
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

fn issue(
    pool: &tentaflow_core::db::DbPool,
    claim_id: &str,
    claim_type: &str,
    scope_addon: Option<&str>,
    scope_namespace: Option<&str>,
    signers: &[(&str, &str)],
) {
    policy::insert_claim(
        pool,
        &NewClaim {
            claim_id: claim_id.into(),
            claim_type: claim_type.into(),
            label: "T".into(),
            subject: None,
            scope: None,
            document_uri: None,
            scope_addon_id: scope_addon.map(String::from),
            scope_namespace: scope_namespace.map(String::from),
            valid_from: "2026-01-01T00:00:00Z".into(),
            valid_until: "2030-01-01T00:00:00Z".into(),
            issued_by_user: "admin".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        },
    )
    .unwrap();
    for (role, user) in signers {
        policy::insert_signature(
            pool,
            &NewSignature {
                claim_id: claim_id.into(),
                signer_role: (*role).into(),
                signer_user: (*user).into(),
                signed_at: "2026-01-02T00:00:00Z".into(),
                signature_b64: None,
            },
        )
        .unwrap();
    }
}

#[test]
fn ungated_namespace_passes_without_claim() {
    let (_d, pool) = open_pool();
    let state = make_state(pool, "addon-x", Vec::new());
    let spec = vector_spec("attributes", None);
    assert!(enforce_gate_with_policy(&state, &spec, None).is_ok());
}

#[test]
fn gated_namespace_without_claim_is_denied() {
    let (_d, pool) = open_pool();
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    let spec = vector_spec("faces", Some("d4-historical"));
    let (abi, reason) = enforce_gate_with_policy(&state, &spec, None).unwrap_err();
    assert_eq!(abi, AbiError::GateNotSatisfied);
    assert_eq!(reason, "gate_claim_id_missing");
}

#[test]
fn gated_namespace_with_unknown_gate_id_returns_not_found() {
    let (_d, pool) = open_pool();
    // Manifest has no gate with id "d4-historical" -> install would have
    // rejected, but the runtime guards anyway.
    let state = make_state(pool, "addon-x", Vec::new());
    let spec = vector_spec("faces", Some("d4-historical"));
    let (abi, reason) = enforce_gate_with_policy(&state, &spec, Some("c1")).unwrap_err();
    assert_eq!(abi, AbiError::NotFound);
    assert_eq!(reason, "gate_not_declared_in_manifest");
}

#[test]
fn gated_namespace_with_valid_claim_ok() {
    let (_d, pool) = open_pool();
    issue(&pool, "c1", "dpia", None, None, &[("dpo", "alice")]);
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    let spec = vector_spec("faces", Some("d4-historical"));
    enforce_gate_with_policy(&state, &spec, Some("c1")).unwrap();
}

#[test]
fn gated_namespace_with_revoked_claim_denied() {
    let (_d, pool) = open_pool();
    issue(&pool, "c1", "dpia", None, None, &[("dpo", "alice")]);
    policy::revoke_claim(&pool, "c1", "audit fail", "2026-02-01T00:00:00Z").unwrap();
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    let spec = vector_spec("faces", Some("d4-historical"));
    let (abi, reason) = enforce_gate_with_policy(&state, &spec, Some("c1")).unwrap_err();
    assert_eq!(abi, AbiError::GateNotSatisfied);
    assert_eq!(reason, "claim_revoked");
}

#[test]
fn gated_namespace_with_namespace_scoped_claim_matches() {
    let (_d, pool) = open_pool();
    issue(&pool, "c1", "dpia", None, Some("faces"), &[("dpo", "alice")]);
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    let spec = vector_spec("faces", Some("d4-historical"));
    enforce_gate_with_policy(&state, &spec, Some("c1")).unwrap();
}

#[test]
fn gated_namespace_with_addon_scoped_claim_rejects_wrong_addon() {
    let (_d, pool) = open_pool();
    issue(&pool, "c1", "dpia", Some("addon-y"), None, &[("dpo", "alice")]);
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    let spec = vector_spec("faces", Some("d4-historical"));
    let (abi, reason) = enforce_gate_with_policy(&state, &spec, Some("c1")).unwrap_err();
    assert_eq!(abi, AbiError::GateNotSatisfied);
    assert_eq!(reason, "claim_scope_mismatch");
}

#[test]
fn helper_lookup_gate_finds_declared() {
    let (_d, pool) = open_pool();
    let state = make_state(pool, "addon-x", vec![gate("d4-historical")]);
    assert!(host_gate::lookup_gate(&state, "d4-historical").is_some());
    assert!(host_gate::lookup_gate(&state, "missing").is_none());
}
