// =============================================================================
// File: tests/policy_host_function.rs
// Purpose: F1c P4 — exercise the policy/claims engine surface used by the
//          `gate_check_v1` host function (gate lookup, ctx builder, claim
//          verification, error→reason mapping). Wasmtime ABI wiring is
//          covered by the unit tests in `gate.rs` + the integration via
//          `vector_gate_enforcement` (real WASM path).
// =============================================================================

use tempfile::TempDir;

use tentaflow_core::addon::host_functions::gate::{
    build_context, policy_error_to_reason, primary_claim_type_for_gate,
    required_signer_roles_for_gate,
};
use tentaflow_core::addon::manifest::{ClaimRequirement, GateSpec};
use tentaflow_core::services::policy::{
    self, ClaimContext, NewClaim, NewSignature, PolicyError,
};

fn open_pool() -> (TempDir, tentaflow_core::db::DbPool) {
    let d = TempDir::new().unwrap();
    let p = d.path().join("policy.db");
    let pool = tentaflow_core::db::init(&p).unwrap();
    (d, pool)
}

fn gate(claims: Vec<ClaimRequirement>) -> GateSpec {
    GateSpec {
        id: "d4-historical".to_string(),
        display_name: "D4 historical search".to_string(),
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

fn issue_claim(
    pool: &tentaflow_core::db::DbPool,
    claim_id: &str,
    claim_type: &str,
    signers: &[(&str, &str)],
) {
    policy::insert_claim(
        pool,
        &NewClaim {
            claim_id: claim_id.to_string(),
            claim_type: claim_type.to_string(),
            label: "Test".to_string(),
            subject: None,
            scope: None,
            document_uri: None,
            scope_addon_id: None,
            scope_namespace: None,
            valid_from: "2026-01-01T00:00:00Z".to_string(),
            valid_until: "2030-01-01T00:00:00Z".to_string(),
            issued_by_user: "admin".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        },
    )
    .unwrap();
    for (role, user) in signers {
        policy::insert_signature(
            pool,
            &NewSignature {
                claim_id: claim_id.to_string(),
                signer_role: role.to_string(),
                signer_user: user.to_string(),
                signed_at: "2026-01-02T00:00:00Z".to_string(),
                signature_b64: None,
            },
        )
        .unwrap();
    }
}

#[test]
fn gate_check_v1_engine_path_ok_with_required_signers() {
    let (_d, pool) = open_pool();
    issue_claim(&pool, "c1", "dpia", &[("dpo", "alice"), ("supervisor", "bob")]);
    let g = gate(vec![
        req("approval", Some("dpo")),
        req("approval", Some("supervisor")),
        req("dpia", None),
    ]);
    let ctx = build_context("addon-x", &g, Some("faces"));
    let v = policy::verify_claim(&pool, "c1", &ctx).unwrap();
    assert_eq!(v.claim_id, "c1");
    assert_eq!(v.signers.len(), 2);
}

#[test]
fn gate_check_v1_engine_path_denied_when_revoked() {
    let (_d, pool) = open_pool();
    issue_claim(&pool, "c1", "dpia", &[("dpo", "alice")]);
    policy::revoke_claim(&pool, "c1", "audit fail", "2026-02-01T00:00:00Z").unwrap();
    let g = gate(vec![req("dpia", None)]);
    let ctx = build_context("addon-x", &g, None);
    let err = policy::verify_claim(&pool, "c1", &ctx).unwrap_err();
    let (code, msg) = policy_error_to_reason(&err);
    assert_eq!(code, "claim_revoked");
    assert!(msg.contains("audit fail"));
}

#[test]
fn gate_check_v1_engine_path_denied_when_unknown_claim() {
    let (_d, pool) = open_pool();
    let g = gate(vec![req("dpia", None)]);
    let ctx = build_context("addon-x", &g, None);
    let err = policy::verify_claim(&pool, "missing", &ctx).unwrap_err();
    let (code, _) = policy_error_to_reason(&err);
    assert_eq!(code, "claim_not_found");
}

#[test]
fn helper_primary_claim_type_prefers_artifact_type() {
    let g = gate(vec![req("approval", Some("dpo")), req("fria", None)]);
    assert_eq!(primary_claim_type_for_gate(&g), "fria");
}

#[test]
fn helper_required_signer_roles_aggregates_unique_sorted() {
    let g = gate(vec![
        req("approval", Some("supervisor")),
        req("approval", Some("dpo")),
        req("approval", Some("dpo")),
    ]);
    assert_eq!(
        required_signer_roles_for_gate(&g),
        vec!["dpo".to_string(), "supervisor".to_string()]
    );
}

#[test]
fn ctx_builder_uses_default_dpo_when_gate_has_no_approval_subject() {
    let g = gate(vec![req("dpia", None)]);
    let ctx: ClaimContext = build_context("addon-x", &g, None);
    assert_eq!(ctx.required_signer_roles, vec!["dpo".to_string()]);
    assert_eq!(ctx.claim_type_required, "dpia");
}

#[test]
fn policy_error_to_reason_covers_every_variant() {
    let cases = [
        PolicyError::ClaimNotFound("c".into()),
        PolicyError::ClaimRevoked {
            claim_id: "c".into(),
            reason: "r".into(),
        },
        PolicyError::ClaimNotInValidityPeriod {
            claim_id: "c".into(),
            now: "n".into(),
            valid_from: "f".into(),
            valid_until: "u".into(),
        },
        PolicyError::ClaimTypeMismatch {
            expected: "a".into(),
            actual: "b".into(),
        },
        PolicyError::ClaimScopeMismatch { detail: "d".into() },
        PolicyError::MissingRequiredSigner { role: "dpo".into() },
        PolicyError::DbError("x".into()),
    ];
    for e in cases {
        let (code, _) = policy_error_to_reason(&e);
        assert!(!code.is_empty(), "missing code for {e:?}");
    }
}
