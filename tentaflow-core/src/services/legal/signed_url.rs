// =============================================================================
// File: services/legal/signed_url.rs — F2 P8.c HMAC-signed download URLs
// =============================================================================
//
// Mints + verifies the four-component download tokens that gate
// `GET /legal/<doc_id>?token=&exp=&org=&nonce=`. The actual HMAC primitive is
// the generic `SignedUrlIssuer` from `services::signed_urls`; this module
// layers the legal-specific binding (doc_id + org_id + nonce) on top so the
// issuer's `ref_id` field stays a single composite blob.
//
// Composite ref format: `<doc_id>|<org_id>|<nonce>`. The pipe is deliberate
// (not a URL-safe character) so a malformed `doc_id` cannot smuggle org / nonce
// bits past the splitter. The issuer signs the full composite — the HMAC
// already covers all four fields together with the scope literal and expiry.
//
// `nonce` is a fresh UUIDv4 minted per issuance. It is not a one-shot guard
// (the issuer is multi-use within TTL) — it simply ensures two identical
// (doc_id, org_id, expiry) tuples produce different signatures, so URLs are
// unguessable by replay even if the operator double-clicks "Generate".

use std::sync::Arc;

use uuid::Uuid;

use crate::services::signed_urls::{SignedUrl, SignedUrlError, SignedUrlIssuer};

/// Default download TTL — 1 h. Matches `UrlScope::LegalUrl::max_ttl_secs()`,
/// callers can override down to 60 s (the per-scope minimum).
pub const DEFAULT_LEGAL_URL_TTL_SECS: u64 = 3600;

/// Field separator inside the composite `ref_id` consumed by the generic
/// issuer. `|` is intentional — not a valid character in UUIDs / nonces, so
/// any malformed input fails the `Composite::parse` split.
const REF_SEP: char = '|';

/// Output of a successful mint: the components needed to build the URL on the
/// caller side plus the raw token bytes. `signed_url` is the ready-to-render
/// relative path the dashboard hands back to the operator.
#[derive(Debug, Clone)]
pub struct LegalSignedUrl {
    pub doc_id: String,
    pub org_id: String,
    pub nonce: String,
    pub expiry_unix_ms: u64,
    pub token_b64: String,
    pub signed_url: String,
}

/// Compose the four-component ref the underlying issuer signs.
fn compose_ref(doc_id: &str, org_id: &str, nonce: &str) -> String {
    let mut out = String::with_capacity(doc_id.len() + org_id.len() + nonce.len() + 2);
    out.push_str(doc_id);
    out.push(REF_SEP);
    out.push_str(org_id);
    out.push(REF_SEP);
    out.push_str(nonce);
    out
}

/// Mint a fresh signed URL for `doc_id` scoped to `org_id`. Each call produces
/// a unique URL (new nonce + signature) — duplicate clicks do not surface the
/// same string twice.
pub fn mint_legal_url(
    issuer: &Arc<SignedUrlIssuer>,
    doc_id: &str,
    org_id: &str,
    ttl_secs: u64,
) -> Result<LegalSignedUrl, SignedUrlError> {
    let nonce = Uuid::new_v4().to_string();
    let composite = compose_ref(doc_id, org_id, &nonce);
    let SignedUrl {
        ref_id: _,
        expiry_unix_ms,
        token_b64,
    } = issuer.issue(composite, ttl_secs)?;
    let signed_url = format!(
        "/legal/{}?token={}&exp={}&org={}&nonce={}",
        url_encode(doc_id),
        url_encode(&token_b64),
        expiry_unix_ms,
        url_encode(org_id),
        url_encode(&nonce),
    );
    Ok(LegalSignedUrl {
        doc_id: doc_id.to_string(),
        org_id: org_id.to_string(),
        nonce,
        expiry_unix_ms,
        token_b64,
        signed_url,
    })
}

/// Verify a presented `(doc_id, org_id, nonce, expiry, token)` quintuple.
/// Wraps the generic issuer's constant-time HMAC compare — the issuer drives
/// scope-mismatch, expiry, and per-key candidate folding.
pub fn verify_legal_token(
    issuer: &Arc<SignedUrlIssuer>,
    doc_id: &str,
    org_id: &str,
    nonce: &str,
    expiry_unix_ms: u64,
    token_b64: &str,
) -> Result<(), SignedUrlError> {
    let composite = compose_ref(doc_id, org_id, nonce);
    issuer.verify(&composite, expiry_unix_ms, token_b64)
}

/// Minimal RFC 3986 unreserved-only escaping — matches the encoding the
/// `SignedUrl::query_string()` helper uses for `ref` / `token`. Kept local so
/// this module is free of any dependency on `urlencoding` (the runtime crate
/// already vendors a different escape behaviour for the query parser).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::signed_urls::UrlScope;

    fn issuer() -> Arc<SignedUrlIssuer> {
        Arc::new(SignedUrlIssuer::new_for_tests(
            UrlScope::LegalUrl,
            [0x77u8; 32],
        ))
    }

    const DOC: &str = "11111111-1111-4111-8111-111111111111";
    const ORG: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn mint_and_verify_round_trip() {
        let iss = issuer();
        let url = mint_legal_url(&iss, DOC, ORG, 600).expect("mint");
        verify_legal_token(
            &iss,
            DOC,
            ORG,
            &url.nonce,
            url.expiry_unix_ms,
            &url.token_b64,
        )
        .expect("verify");
        assert!(url.signed_url.starts_with("/legal/"));
        assert!(url.signed_url.contains("token="));
        assert!(url.signed_url.contains("exp="));
        assert!(url.signed_url.contains("org="));
        assert!(url.signed_url.contains("nonce="));
    }

    #[test]
    fn cross_org_token_rejected() {
        let iss = issuer();
        let url = mint_legal_url(&iss, DOC, ORG, 600).expect("mint");
        // Token minted for ORG must not verify when presented with another org.
        let other_org = "33333333-3333-4333-8333-333333333333";
        let err = verify_legal_token(
            &iss,
            DOC,
            other_org,
            &url.nonce,
            url.expiry_unix_ms,
            &url.token_b64,
        )
        .unwrap_err();
        assert_eq!(err, SignedUrlError::InvalidSignature);
    }

    #[test]
    fn tampered_doc_id_rejected() {
        let iss = issuer();
        let url = mint_legal_url(&iss, DOC, ORG, 600).expect("mint");
        let other_doc = "44444444-4444-4444-8444-444444444444";
        let err = verify_legal_token(
            &iss,
            other_doc,
            ORG,
            &url.nonce,
            url.expiry_unix_ms,
            &url.token_b64,
        )
        .unwrap_err();
        assert_eq!(err, SignedUrlError::InvalidSignature);
    }

    #[test]
    fn each_mint_uses_fresh_nonce() {
        let iss = issuer();
        let a = mint_legal_url(&iss, DOC, ORG, 600).expect("mint a");
        let b = mint_legal_url(&iss, DOC, ORG, 600).expect("mint b");
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.token_b64, b.token_b64);
    }
}
