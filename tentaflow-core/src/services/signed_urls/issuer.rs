// =============================================================================
// File: services/signed_urls/issuer.rs — generic HMAC-SHA256 URL issuer
// =============================================================================
//
// Multi-use signed URLs scoped by `UrlScope`. Tokens carry the scope literal as
// part of the HMAC payload (`<scope>:<ref>:<expiry_ms>`) so a token minted for
// raw frames cannot be replayed against the recording endpoint even if both
// issuers somehow shared a key — defense in depth on top of per-scope keys.
// Verification is non-consuming: callers may verify the same token repeatedly
// until expiry.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlScope {
    /// Raw frames served by Service-to-Core frame API.
    FrameUrl,
    /// Snapshots / segments out of the `recordings` table.
    Recording,
}

impl UrlScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FrameUrl => "frame",
            Self::Recording => "recording",
        }
    }

    pub fn min_ttl_secs(&self) -> u64 {
        match self {
            Self::FrameUrl => 60,
            Self::Recording => 60,
        }
    }

    pub fn max_ttl_secs(&self) -> u64 {
        match self {
            Self::FrameUrl => 600,
            Self::Recording => 3600,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SignedUrl {
    pub ref_id: String,
    pub expiry_unix_ms: u64,
    pub token_b64: String,
}

impl SignedUrl {
    /// `token=<b64>&exp=<ms>&ref=<url-encoded>` — the canonical query fragment
    /// appended by callers building a full URL.
    pub fn query_string(&self) -> String {
        format!(
            "token={}&exp={}&ref={}",
            url_encode(&self.token_b64),
            self.expiry_unix_ms,
            url_encode(&self.ref_id),
        )
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignedUrlError {
    #[error("invalid token format")]
    InvalidFormat,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("token expired")]
    Expired,
    #[error("ttl out of range: {0} not in [{1}, {2}] secs")]
    TtlOutOfRange(u64, u64, u64),
    #[error("ref empty or too long")]
    RefInvalid,
}

pub struct SignedUrlIssuer {
    scope: UrlScope,
    signing_key: Arc<[u8; 32]>,
}

impl SignedUrlIssuer {
    /// Generate a fresh signing key from `OsRng`. Process-local; restart
    /// invalidates every outstanding token (acceptable: max TTL is 1 h).
    pub fn new(scope: UrlScope) -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("OS RNG fill for SignedUrlIssuer signing key");
        Self {
            scope,
            signing_key: Arc::new(key),
        }
    }

    /// Test constructor — pinned key, no RNG.
    #[doc(hidden)]
    pub fn new_for_tests(scope: UrlScope, key: [u8; 32]) -> Self {
        Self {
            scope,
            signing_key: Arc::new(key),
        }
    }

    pub fn scope(&self) -> UrlScope {
        self.scope
    }

    pub fn issue(&self, ref_id: String, ttl_secs: u64) -> Result<SignedUrl, SignedUrlError> {
        let min = self.scope.min_ttl_secs();
        let max = self.scope.max_ttl_secs();
        if ttl_secs < min || ttl_secs > max {
            return Err(SignedUrlError::TtlOutOfRange(ttl_secs, min, max));
        }
        if ref_id.is_empty() || ref_id.len() > 256 {
            return Err(SignedUrlError::RefInvalid);
        }
        let expiry_unix_ms = now_unix_ms() + ttl_secs * 1000;
        let payload = format!("{}:{}:{}", self.scope.as_str(), ref_id, expiry_unix_ms);
        let sig = hmac_sign(&*self.signing_key, payload.as_bytes());
        let token_b64 = B64.encode(sig);
        Ok(SignedUrl {
            ref_id,
            expiry_unix_ms,
            token_b64,
        })
    }

    /// Multi-use verify. Does NOT mark the token consumed — callers may verify
    /// the same `(ref_id, expiry_unix_ms, token_b64)` triple as many times as
    /// they like until `expiry_unix_ms` passes.
    pub fn verify(
        &self,
        ref_id: &str,
        expiry_unix_ms: u64,
        token_b64: &str,
    ) -> Result<(), SignedUrlError> {
        if ref_id.is_empty() || ref_id.len() > 256 {
            return Err(SignedUrlError::RefInvalid);
        }
        let provided = B64
            .decode(token_b64.as_bytes())
            .map_err(|_| SignedUrlError::InvalidFormat)?;
        if now_unix_ms() > expiry_unix_ms {
            return Err(SignedUrlError::Expired);
        }
        let payload = format!("{}:{}:{}", self.scope.as_str(), ref_id, expiry_unix_ms);
        let expected = hmac_sign(&*self.signing_key, payload.as_bytes());
        if provided.len() != expected.len() || !bool::from(provided.ct_eq(&expected)) {
            return Err(SignedUrlError::InvalidSignature);
        }
        Ok(())
    }
}

fn hmac_sign(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let result = mac.finalize().into_bytes();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&result);
    arr
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Minimal RFC 3986 unreserved-only escaping for the ref_id field. We keep the
/// dependency surface tight by not pulling in `percent-encoding`; everything
/// outside `[A-Za-z0-9._~-]` is %XX-encoded.
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

    fn frame_issuer() -> SignedUrlIssuer {
        SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [11u8; 32])
    }

    fn rec_issuer() -> SignedUrlIssuer {
        SignedUrlIssuer::new_for_tests(UrlScope::Recording, [22u8; 32])
    }

    #[test]
    fn test_issue_and_verify_basic() {
        let i = frame_issuer();
        let u = i.issue("frame_abc".into(), 120).expect("issue ok");
        i.verify(&u.ref_id, u.expiry_unix_ms, &u.token_b64)
            .expect("verify ok");
    }

    #[test]
    fn test_verify_wrong_ref_fails() {
        let i = frame_issuer();
        let u = i.issue("frame_a".into(), 120).unwrap();
        let err = i.verify("frame_b", u.expiry_unix_ms, &u.token_b64).unwrap_err();
        assert_eq!(err, SignedUrlError::InvalidSignature);
    }

    #[test]
    fn test_verify_expired() {
        let i = frame_issuer();
        let u = i.issue("frame_a".into(), 60).unwrap();
        // Forge an expiry in the past — signature won't match, but the expiry
        // check must run BEFORE signature compare and surface `Expired`.
        let past = now_unix_ms().saturating_sub(10_000);
        // Recompute a valid signature for the past expiry so we exercise the
        // expiry branch specifically.
        let payload = format!("{}:{}:{}", UrlScope::FrameUrl.as_str(), "frame_a", past);
        let sig = hmac_sign(&[11u8; 32], payload.as_bytes());
        let tok = B64.encode(sig);
        let err = i.verify("frame_a", past, &tok).unwrap_err();
        assert_eq!(err, SignedUrlError::Expired);
    }

    #[test]
    fn test_verify_tampered_signature() {
        let i = frame_issuer();
        let u = i.issue("frame_a".into(), 120).unwrap();
        let mut t = u.token_b64.clone();
        let last = t.pop().unwrap();
        t.push(if last == 'A' { 'B' } else { 'A' });
        let err = i.verify(&u.ref_id, u.expiry_unix_ms, &t).unwrap_err();
        assert_eq!(err, SignedUrlError::InvalidSignature);
    }

    #[test]
    fn test_ttl_out_of_range_frame_url() {
        let i = frame_issuer();
        assert!(matches!(
            i.issue("f".into(), 10).unwrap_err(),
            SignedUrlError::TtlOutOfRange(10, 60, 600)
        ));
        assert!(matches!(
            i.issue("f".into(), 700).unwrap_err(),
            SignedUrlError::TtlOutOfRange(700, 60, 600)
        ));
    }

    #[test]
    fn test_ttl_out_of_range_recording() {
        let i = rec_issuer();
        assert!(matches!(
            i.issue("r".into(), 10).unwrap_err(),
            SignedUrlError::TtlOutOfRange(10, 60, 3600)
        ));
        assert!(matches!(
            i.issue("r".into(), 3700).unwrap_err(),
            SignedUrlError::TtlOutOfRange(3700, 60, 3600)
        ));
    }

    #[test]
    fn test_multi_use_verify() {
        let i = frame_issuer();
        let u = i.issue("frame_x".into(), 120).unwrap();
        for _ in 0..3 {
            i.verify(&u.ref_id, u.expiry_unix_ms, &u.token_b64)
                .expect("multi-use ok");
        }
    }

    #[test]
    fn test_scope_mismatch_fails() {
        // Mint a token using the FrameUrl issuer's KEY but with the "recording"
        // scope literal baked into the payload. Verifying through the FrameUrl
        // issuer must reject — the HMAC over `frame:...` won't match the bytes
        // signed over `recording:...`, so the verdict is InvalidSignature even
        // though the keys are identical.
        let frame = SignedUrlIssuer::new_for_tests(UrlScope::FrameUrl, [33u8; 32]);
        let exp = now_unix_ms() + 120_000;
        let forged_payload = format!("{}:{}:{}", UrlScope::Recording.as_str(), "x", exp);
        let sig = hmac_sign(&[33u8; 32], forged_payload.as_bytes());
        let tok = B64.encode(sig);
        let err = frame.verify("x", exp, &tok).unwrap_err();
        assert_eq!(err, SignedUrlError::InvalidSignature);
    }

    #[test]
    fn test_query_string_format() {
        let u = SignedUrl {
            ref_id: "clip_1 2".into(),
            expiry_unix_ms: 1234,
            token_b64: "AB==".into(),
        };
        let q = u.query_string();
        assert!(q.contains("token=AB%3D%3D"));
        assert!(q.contains("exp=1234"));
        assert!(q.contains("ref=clip_1%202"));
    }

    #[test]
    fn test_ref_invalid() {
        let i = frame_issuer();
        assert_eq!(i.issue("".into(), 120).unwrap_err(), SignedUrlError::RefInvalid);
        let big = "a".repeat(257);
        assert_eq!(i.issue(big, 120).unwrap_err(), SignedUrlError::RefInvalid);
    }
}
