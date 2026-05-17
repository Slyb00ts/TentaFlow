// ============ File: services/policy/engine.rs — Claim verification engine ============
//
// Verifies that a `claim_id` issued via `policy issue` satisfies the
// requirements declared on an addon manifest `[[gate]]` block before a
// gated host function (vector_search on a `requires_claim` namespace, the
// addon-callable `gate_check_v1`) hands the addon a result.
//
// The engine is read-only: it never mutates the DB. All side effects
// (issuance, revocation) belong to `repo`.

use crate::db::DbPool;

use super::error::{PolicyError, Result};
use super::repo;

/// Context passed by the gated caller. Built from the addon manifest
/// `[[gate]].required_claims` block matched at install time and the
/// per-call resource identity (vector namespace / alias id).
#[derive(Debug, Clone)]
pub struct ClaimContext {
    pub addon_id: String,
    /// Expected claim type — must match `policy_claims.claim_type`.
    /// Allowed values are mirrored from `manifest::CLAIM_TYPES`
    /// ("approval" | "grant" | "deployment_profile" | "consent" | "dpia" | "fria"
    /// | "legal_grant"). The engine does not enforce the enum — admins can
    /// issue future claim types as long as the manifest agrees.
    pub claim_type_required: String,
    /// Optional resource identity (namespace / alias id). When the claim
    /// has a non-NULL `scope_namespace`, this must equal it.
    pub resource_scope: Option<String>,
    /// Required signer roles. Engine asserts that every role here appears
    /// at least once in `policy_claim_signatures` for the claim.
    pub required_signer_roles: Vec<String>,
    /// "Now" timestamp as an RFC 3339 string (any offset accepted — `Z`,
    /// `+00:00`, `+02:00`...). Caller-supplied so unit tests can pin time
    /// deterministically; host fn callers use `chrono::Utc::now().to_rfc3339()`.
    /// The engine parses every timestamp into `DateTime<Utc>` before
    /// comparing, so mixing zone offsets across `now_utc`, `valid_from` and
    /// `valid_until` is safe — they are normalized to UTC instants.
    pub now_utc: String,
}

/// Parses an RFC 3339 timestamp into UTC. Accepts every offset form
/// (`...Z`, `...+00:00`, `...+02:00`, etc.) and converts to UTC so callers
/// can lex-free compare instants. Wrapped here so every validity-window
/// check in `verify_claim` returns the same `PolicyError::DbError` shape
/// when a stored claim has a malformed timestamp.
fn parse_rfc3339_utc(s: &str, field: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| PolicyError::DbError(format!("invalid RFC3339 timestamp in {field} ('{s}'): {e}")))
}

/// Signer entry returned in the verified payload (role + user identity).
/// The signature blob itself stays in the DB — addons get the attribution
/// chain (audit trail) but never the raw signature bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SignerEntry {
    pub role: String,
    pub user: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaimVerified {
    pub claim_id: String,
    pub claim_type: String,
    pub valid_until: String,
    pub signers: Vec<SignerEntry>,
}

/// Resolves a claim id against the policy store and returns `ClaimVerified`
/// if every check passes. A single failing check returns the matching
/// `PolicyError` variant — the host function callers map this to either an
/// `AbiError::GateNotSatisfied` (addon-visible) or a CLI "invalid: reason"
/// line. The engine never returns `Ok` for a revoked or expired claim.
pub fn verify_claim(
    pool: &DbPool,
    claim_id: &str,
    ctx: &ClaimContext,
) -> Result<ClaimVerified> {
    let row = repo::get_claim(pool, claim_id)?
        .ok_or_else(|| PolicyError::ClaimNotFound(claim_id.to_string()))?;

    if let Some(revoked_at) = &row.revoked_at {
        let reason = row
            .revoked_reason
            .clone()
            .unwrap_or_else(|| format!("revoked at {revoked_at} (no reason recorded)"));
        return Err(PolicyError::ClaimRevoked {
            claim_id: row.claim_id,
            reason,
        });
    }

    // Validity window. Parse every timestamp through `DateTime<Utc>` so we
    // compare instants, not strings — mixing `Z` and `+00:00` (or any other
    // offset) across `now_utc`, `valid_from` and `valid_until` would break
    // lexicographic ordering and could let an expired claim through.
    let now_dt = parse_rfc3339_utc(&ctx.now_utc, "now_utc")?;
    let valid_from_dt = parse_rfc3339_utc(&row.valid_from, "valid_from")?;
    let valid_until_dt = parse_rfc3339_utc(&row.valid_until, "valid_until")?;
    if now_dt < valid_from_dt || now_dt > valid_until_dt {
        return Err(PolicyError::ClaimNotInValidityPeriod {
            claim_id: row.claim_id,
            now: ctx.now_utc.clone(),
            valid_from: row.valid_from,
            valid_until: row.valid_until,
        });
    }

    if row.claim_type != ctx.claim_type_required {
        return Err(PolicyError::ClaimTypeMismatch {
            expected: ctx.claim_type_required.clone(),
            actual: row.claim_type,
        });
    }

    // Scope narrowing: addon scope binds if set; namespace scope binds if set.
    // NULL = global / unrestricted on that axis.
    if let Some(scope_addon) = &row.scope_addon_id {
        if scope_addon != &ctx.addon_id {
            return Err(PolicyError::ClaimScopeMismatch {
                detail: format!(
                    "claim restricted to addon '{scope_addon}', caller is '{}'",
                    ctx.addon_id
                ),
            });
        }
    }
    if let Some(scope_ns) = &row.scope_namespace {
        match &ctx.resource_scope {
            Some(rs) if rs == scope_ns => {}
            Some(rs) => {
                return Err(PolicyError::ClaimScopeMismatch {
                    detail: format!(
                        "claim restricted to namespace '{scope_ns}', caller asked for '{rs}'"
                    ),
                });
            }
            None => {
                return Err(PolicyError::ClaimScopeMismatch {
                    detail: format!(
                        "claim restricted to namespace '{scope_ns}', caller did not provide a resource scope"
                    ),
                });
            }
        }
    }

    // Signature check. Every required role must have at least one signer
    // present in policy_claim_signatures. The `signature_b64` blob is NOT
    // re-verified here — manual admin acknowledgment is the contract in
    // F1c-P4; a future hardening can re-run Ed25519 verify on the blob.
    let sigs = repo::list_signatures(pool, claim_id)?;
    for role in &ctx.required_signer_roles {
        let role_present = sigs.iter().any(|s| &s.signer_role == role);
        if !role_present {
            return Err(PolicyError::MissingRequiredSigner {
                role: role.clone(),
            });
        }
    }

    Ok(ClaimVerified {
        claim_id: row.claim_id,
        claim_type: row.claim_type,
        valid_until: row.valid_until,
        signers: sigs
            .into_iter()
            .map(|s| SignerEntry {
                role: s.signer_role,
                user: s.signer_user,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::policy::repo::{NewClaim, NewSignature};

    fn open_pool() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("policy_test.db");
        let pool = crate::db::init(&path).expect("init DB");
        (dir, pool)
    }

    fn base_claim(claim_id: &str) -> NewClaim {
        NewClaim {
            claim_id: claim_id.to_string(),
            claim_type: "dpia".to_string(),
            label: "Test DPIA".to_string(),
            subject: None,
            scope: None,
            document_uri: None,
            scope_addon_id: None,
            scope_namespace: None,
            valid_from: "2026-01-01T00:00:00Z".to_string(),
            valid_until: "2027-01-01T00:00:00Z".to_string(),
            issued_by_user: "admin".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn sig(claim_id: &str, role: &str, user: &str) -> NewSignature {
        NewSignature {
            claim_id: claim_id.to_string(),
            signer_role: role.to_string(),
            signer_user: user.to_string(),
            signed_at: "2026-01-02T00:00:00Z".to_string(),
            signature_b64: None,
        }
    }

    fn ctx(addon_id: &str, claim_type: &str, scope: Option<&str>) -> ClaimContext {
        ClaimContext {
            addon_id: addon_id.to_string(),
            claim_type_required: claim_type.to_string(),
            resource_scope: scope.map(String::from),
            required_signer_roles: vec!["dpo".to_string(), "supervisor".to_string()],
            now_utc: "2026-06-15T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_verify_valid_claim_ok() {
        let (_d, pool) = open_pool();
        repo::insert_claim(&pool, &base_claim("c1")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "alice")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "bob")).unwrap();
        let v = verify_claim(&pool, "c1", &ctx("addon-x", "dpia", None)).unwrap();
        assert_eq!(v.claim_id, "c1");
        assert_eq!(v.signers.len(), 2);
    }

    #[test]
    fn test_verify_unknown_claim_id() {
        let (_d, pool) = open_pool();
        let err = verify_claim(&pool, "ghost", &ctx("a", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimNotFound(_)));
    }

    #[test]
    fn test_verify_revoked_claim() {
        let (_d, pool) = open_pool();
        repo::insert_claim(&pool, &base_claim("c1")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "alice")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "bob")).unwrap();
        repo::revoke_claim(&pool, "c1", "audit fail", "2026-02-01T00:00:00Z").unwrap();
        let err = verify_claim(&pool, "c1", &ctx("a", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimRevoked { .. }));
    }

    #[test]
    fn test_verify_expired_claim() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.valid_until = "2026-03-01T00:00:00Z".to_string();
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimNotInValidityPeriod { .. }));
    }

    #[test]
    fn test_verify_future_claim_not_yet_valid() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.valid_from = "2027-01-01T00:00:00Z".to_string();
        c.valid_until = "2028-01-01T00:00:00Z".to_string();
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimNotInValidityPeriod { .. }));
    }

    #[test]
    fn test_verify_claim_type_mismatch() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.claim_type = "consent".to_string();
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimTypeMismatch { .. }));
    }

    #[test]
    fn test_verify_scope_mismatch_wrong_addon() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.scope_addon_id = Some("addon-y".to_string());
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(&pool, "c1", &ctx("addon-x", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimScopeMismatch { .. }));
    }

    #[test]
    fn test_verify_scope_mismatch_wrong_namespace() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.scope_namespace = Some("faces".to_string());
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(
            &pool,
            "c1",
            &ctx("addon-x", "dpia", Some("attributes")),
        )
        .unwrap_err();
        assert!(matches!(err, PolicyError::ClaimScopeMismatch { .. }));
    }

    #[test]
    fn test_verify_missing_required_signer() {
        let (_d, pool) = open_pool();
        repo::insert_claim(&pool, &base_claim("c1")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "alice")).unwrap();
        // missing supervisor
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::MissingRequiredSigner { .. }));
    }

    #[test]
    fn test_verify_global_scope_matches_any_addon() {
        let (_d, pool) = open_pool();
        // Both scope_addon_id and scope_namespace stay NULL -> global claim.
        repo::insert_claim(&pool, &base_claim("c1")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        verify_claim(&pool, "c1", &ctx("addon-1", "dpia", Some("ns1"))).unwrap();
        verify_claim(&pool, "c1", &ctx("addon-2", "dpia", Some("ns2"))).unwrap();
    }

    #[test]
    fn test_verify_accepts_z_suffix() {
        let (_d, pool) = open_pool();
        repo::insert_claim(&pool, &base_claim("c1")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let mut c = ctx("addon", "dpia", None);
        c.now_utc = "2026-06-15T12:00:00Z".to_string();
        verify_claim(&pool, "c1", &c).unwrap();
    }

    #[test]
    fn test_verify_accepts_plus_offset() {
        let (_d, pool) = open_pool();
        let mut nc = base_claim("c1");
        nc.valid_from = "2026-01-01T00:00:00+00:00".to_string();
        nc.valid_until = "2027-01-01T00:00:00+00:00".to_string();
        repo::insert_claim(&pool, &nc).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let mut c = ctx("addon", "dpia", None);
        c.now_utc = "2026-06-15T00:00:00Z".to_string();
        verify_claim(&pool, "c1", &c).unwrap();
    }

    #[test]
    fn test_verify_accepts_non_utc_offset() {
        // valid_until = 2027-01-01T02:00:00+02:00 == 2027-01-01T00:00:00Z.
        // now      = 2026-12-31T23:59:00Z is BEFORE valid_until in UTC even
        // though lex-string `2026-12-31...Z` > `2027-01-01T02:...+02:00`.
        let (_d, pool) = open_pool();
        let mut nc = base_claim("c1");
        nc.valid_from = "2026-01-01T02:00:00+02:00".to_string();
        nc.valid_until = "2027-01-01T02:00:00+02:00".to_string();
        repo::insert_claim(&pool, &nc).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let mut c = ctx("addon", "dpia", None);
        c.now_utc = "2026-12-31T23:59:00Z".to_string();
        verify_claim(&pool, "c1", &c).unwrap();
    }

    #[test]
    fn test_verify_rejects_malformed_timestamp() {
        let (_d, pool) = open_pool();
        let mut nc = base_claim("c1");
        nc.valid_until = "not-a-date".to_string();
        repo::insert_claim(&pool, &nc).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::DbError(_)), "got {err:?}");
    }

    #[test]
    fn test_verify_namespace_scoped_claim_requires_matching_namespace_arg() {
        let (_d, pool) = open_pool();
        let mut c = base_claim("c1");
        c.scope_namespace = Some("faces".to_string());
        repo::insert_claim(&pool, &c).unwrap();
        repo::insert_signature(&pool, &sig("c1", "dpo", "a")).unwrap();
        repo::insert_signature(&pool, &sig("c1", "supervisor", "b")).unwrap();
        // Caller did not provide resource_scope -> denial.
        let err = verify_claim(&pool, "c1", &ctx("addon", "dpia", None)).unwrap_err();
        assert!(matches!(err, PolicyError::ClaimScopeMismatch { .. }));
        // Matching scope -> OK.
        verify_claim(&pool, "c1", &ctx("addon", "dpia", Some("faces"))).unwrap();
    }
}
