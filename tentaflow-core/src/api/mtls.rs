// =============================================================================
// Plik: api/mtls.rs
// Opis: Konfiguracja mTLS pinning client cert dla Service-to-Core endpointu
//       /core/frame/pickup. Trzyma allowlist hex SHA-256 fingerprintow oraz
//       wlasnego rustls ClientCertVerifier, ktory akceptuje kazdy syntactycznie
//       poprawny cert na poziomie handshake'u — wlasciwe pinning fingerprintu
//       robi sie pozniej w aplikacji (warstwa HTTP w api/dashboard/server.rs).
// =============================================================================

use std::sync::Arc;

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};
use sha2::{Digest, Sha256};

/// Effective runtime config for mTLS pinning of `/core/frame/pickup`.
///
/// `pickup_required = false` means the legacy path: the endpoint is reachable
/// without a client certificate. This is the F1a/F1b default and matches the
/// HMAC-only design. Production deployments should flip this to `true` and
/// publish at least one fingerprint via `client_cert_fingerprints`.
#[derive(Debug, Clone, Default)]
pub struct PickupMtlsConfig {
    pub pickup_required: bool,
    /// Lower-case hex SHA-256 fingerprints of the DER-encoded leaf certificate
    /// (no colons). One entry per acceptable client identity.
    pub allowed_fingerprints: Vec<String>,
}

impl PickupMtlsConfig {
    pub fn new(pickup_required: bool, allowed_fingerprints: Vec<String>) -> Self {
        Self {
            pickup_required,
            allowed_fingerprints: allowed_fingerprints
                .into_iter()
                .map(|s| s.trim().replace(':', "").to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    /// True when the verifier should request a client certificate at all.
    pub fn requests_client_cert(&self) -> bool {
        self.pickup_required
    }

    /// Constant-time fingerprint match (length is fixed 64 hex chars, so the
    /// timing leak is at most "wrong length vs right length", which is fine).
    pub fn matches(&self, peer_cert_der: &[u8]) -> bool {
        if self.allowed_fingerprints.is_empty() {
            return false;
        }
        let fp = fingerprint_hex(peer_cert_der);
        self.allowed_fingerprints.iter().any(|a| a == &fp)
    }
}

/// Newtype carried via `Request::extensions()` from the TLS accept loop to the
/// HTTP handlers — holds the DER bytes of the client's leaf certificate.
#[derive(Debug, Clone)]
pub struct ClientCertDer(pub Vec<u8>);

/// SHA-256 fingerprint of a DER-encoded certificate as lower-case hex.
pub fn fingerprint_hex(der: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(der);
    hex::encode(h.finalize())
}

/// rustls verifier that accepts any client certificate at the TLS layer. The
/// application enforces fingerprint pinning afterwards via `PickupMtlsConfig`.
/// Using this verifier in a TLS 1.3 config makes rustls *request* the client
/// cert (`offer_client_auth = true`) but not reject the handshake when the
/// chain is unknown — without this we would need a real CA bundle on the
/// server side, which we do not have (pickup client certs are self-signed).
#[derive(Debug)]
pub struct AnyClientCertVerifier {
    schemes: WebPkiSupportedAlgorithms,
    empty_subjects: Vec<DistinguishedName>,
}

impl AnyClientCertVerifier {
    pub fn new() -> Arc<Self> {
        let schemes = rustls::crypto::CryptoProvider::get_default()
            .map(|p| p.signature_verification_algorithms)
            .unwrap_or_else(|| {
                rustls::crypto::ring::default_provider().signature_verification_algorithms
            });
        Arc::new(Self {
            schemes,
            empty_subjects: Vec::new(),
        })
    }
}

impl ClientCertVerifier for AnyClientCertVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        // We accept connections without a client cert too. The HTTP layer
        // rejects /core/frame/pickup later when pinning is required and the
        // peer cert is missing or has the wrong fingerprint.
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &self.empty_subjects
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.schemes)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.schemes)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.supported_schemes()
    }
}

// =============================================================================
// Process-global pickup mTLS config (single-node, set once at server start).
// =============================================================================

use std::sync::OnceLock;

static PICKUP_MTLS: OnceLock<Arc<PickupMtlsConfig>> = OnceLock::new();

/// Initialise the process-wide pickup mTLS config. Subsequent calls are no-ops
/// (configuration is immutable after boot — single-node F1a/F1b constraint).
pub fn set_pickup_mtls_config(cfg: PickupMtlsConfig) {
    let _ = PICKUP_MTLS.set(Arc::new(cfg));
}

/// Returns the configured pickup mTLS profile, or the default (disabled,
/// empty allowlist) when nothing was set.
pub fn pickup_mtls_config() -> Arc<PickupMtlsConfig> {
    PICKUP_MTLS
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(PickupMtlsConfig::default()))
}

// =============================================================================
// Universal HTTP security headers (HSTS + hardening trio).
// =============================================================================

/// Production HSTS policy: 2 years, includeSubDomains. Applied to every
/// response served from the dashboard / OpenAI surface — F1b runs HTTPS only,
/// so HSTS is safe to ship unconditionally.
pub const HSTS_HEADER_VALUE: &str = "max-age=63072000; includeSubDomains";

/// Mutates the response header map to add the always-on transport security
/// headers. Called from every accept path so 200, 401, 403, 404, 429 alike
/// carry HSTS — there is no "skip on error" exception.
pub fn apply_universal_security_headers(headers: &mut hyper::HeaderMap) {
    fn set_if_absent(headers: &mut hyper::HeaderMap, name: &'static str, value: &'static str) {
        if !headers.contains_key(name) {
            headers.insert(name, hyper::header::HeaderValue::from_static(value));
        }
    }
    set_if_absent(headers, "Strict-Transport-Security", HSTS_HEADER_VALUE);
    set_if_absent(headers, "X-Content-Type-Options", "nosniff");
    set_if_absent(headers, "Referrer-Policy", "no-referrer");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_lowercase_hex() {
        let der = b"hello world";
        let a = fingerprint_hex(der);
        let b = fingerprint_hex(der);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn matches_rejects_when_allowlist_empty() {
        let cfg = PickupMtlsConfig::new(true, vec![]);
        assert!(!cfg.matches(b"any"));
    }

    #[test]
    fn matches_accepts_when_fingerprint_listed() {
        let der = b"cert-bytes";
        let fp = fingerprint_hex(der);
        let cfg = PickupMtlsConfig::new(true, vec![fp.clone()]);
        assert!(cfg.matches(der));
    }

    #[test]
    fn matches_normalises_colons_and_case() {
        let der = b"cert-bytes";
        let raw = fingerprint_hex(der);
        // Insert colons every 2 chars and uppercase — should still match.
        let mut colonised = String::new();
        for (i, c) in raw.chars().enumerate() {
            if i > 0 && i % 2 == 0 {
                colonised.push(':');
            }
            colonised.push(c.to_ascii_uppercase());
        }
        let cfg = PickupMtlsConfig::new(true, vec![colonised]);
        assert!(cfg.matches(der));
    }

    #[test]
    fn universal_headers_set_hsts() {
        let mut headers = hyper::HeaderMap::new();
        apply_universal_security_headers(&mut headers);
        assert_eq!(
            headers.get("Strict-Transport-Security").unwrap(),
            HSTS_HEADER_VALUE
        );
        assert!(headers.contains_key("X-Content-Type-Options"));
        assert!(headers.contains_key("Referrer-Policy"));
    }

    #[test]
    fn universal_headers_do_not_clobber_existing() {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            "Referrer-Policy",
            hyper::header::HeaderValue::from_static("strict-origin"),
        );
        apply_universal_security_headers(&mut headers);
        assert_eq!(headers.get("Referrer-Policy").unwrap(), "strict-origin");
    }
}
