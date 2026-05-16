// =============================================================================
// File: services/pickup_tokens/token.rs — PickupToken payload + HMAC sign/verify
// =============================================================================
//
// `PickupToken` is the credential a Core router emits at `service_call` time so
// the receiving service can fetch a raw camera frame via the Service-to-Core
// HTTP API. The wire format is `<payload_b64>.<signature_b64>` where
// `signature_b64 = base64(HMAC-SHA256(signing_key, payload_b64))`.
// One token == one frame == one service == one request_id. F1a `one_shot=true`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Signed payload carried inside the wire token. Every field is checked at
/// pickup time against the matching HTTP header so a leaked token tied to
/// service A cannot be replayed against service B.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenPayload {
    /// `frame_<uuid>` ref into `services::frame_storage::FrameStorage`.
    pub raw_ref: String,
    /// Target service that may consume this token.
    pub service_id: String,
    /// Unique per `service_call` — also used as audit correlation id.
    pub request_id: String,
    /// Absolute expiry in Unix milliseconds.
    pub expiry_unix_ms: u64,
    /// F1a always true. Reserved for future multi-pickup semantics.
    pub one_shot: bool,
}

/// Final on-the-wire representation. Producers send `wire()` over QUIC; pickup
/// handler splits on `.` and runs `verify_and_consume`.
#[derive(Debug, Clone)]
pub struct PickupToken {
    pub payload_b64: String,
    pub signature_b64: String,
}

impl PickupToken {
    /// `<payload_b64>.<signature_b64>` — single canonical string carrying
    /// everything the service needs.
    pub fn wire(&self) -> String {
        format!("{}.{}", self.payload_b64, self.signature_b64)
    }
}

/// Errors surfaced by `verify_and_consume`. The HTTP layer maps each to a
/// distinct status code + `frame_pickup_log.result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickupVerifyError {
    /// Wire string is not `payload_b64.signature_b64` or base64 decode failed.
    Malformed,
    /// HMAC mismatch — token forged or signing key mismatch.
    InvalidSignature,
    /// Token absent from the inflight store (server restart, replay after
    /// purge, or never issued).
    InvalidToken,
    /// Token already consumed exactly once — replay attempt.
    AlreadyConsumed,
    /// Past `expiry_unix_ms`.
    Expired,
}

impl PickupVerifyError {
    pub fn as_log_result(self) -> &'static str {
        match self {
            Self::Malformed | Self::InvalidSignature | Self::InvalidToken => "token_invalid",
            Self::AlreadyConsumed => "unauthorized",
            Self::Expired => "token_expired",
        }
    }
}

/// HMAC-SHA256 over `payload_b64` ASCII bytes. Returning a Vec keeps the API
/// independent of the underlying digest size (32 bytes).
pub(crate) fn hmac_sign(key: &[u8], payload_b64: &str) -> Vec<u8> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC key any size");
    mac.update(payload_b64.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Encode payload to canonical form and produce a wire token. Used by the
/// issuer; the HMAC step is split out so callers can also unit-test signing
/// in isolation.
pub(crate) fn sign_payload(key: &[u8], payload: &TokenPayload) -> PickupToken {
    let json = serde_json::to_vec(payload).expect("TokenPayload serializes");
    let payload_b64 = B64.encode(&json);
    let signature = hmac_sign(key, &payload_b64);
    let signature_b64 = B64.encode(signature);
    PickupToken {
        payload_b64,
        signature_b64,
    }
}

/// Split + base64-decode + HMAC-check against every key in `keys` in order.
/// Used to support the 2-key rotation window: callers pass `[current,
/// previous]` so tokens minted under the old key keep verifying until the
/// previous-key window expires. Constant-time HMAC compare is still
/// performed for every candidate key (no early-exit timing leak — the same
/// HMAC compute + ct_eq runs on every iteration).
pub(crate) fn parse_and_verify_multi<'a, I>(
    keys: I,
    wire: &str,
) -> Result<(TokenPayload, String), PickupVerifyError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let (payload_b64, sig_b64) = wire
        .split_once('.')
        .ok_or(PickupVerifyError::Malformed)?;
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(PickupVerifyError::Malformed);
    }
    let provided = B64
        .decode(sig_b64.as_bytes())
        .map_err(|_| PickupVerifyError::Malformed)?;
    let mut any_key_seen = false;
    let mut matched = false;
    for key in keys {
        any_key_seen = true;
        let expected = hmac_sign(key, payload_b64);
        if provided.len() == expected.len()
            && bool::from(provided.ct_eq(&expected))
        {
            matched = true;
        }
    }
    if !any_key_seen {
        return Err(PickupVerifyError::InvalidSignature);
    }
    if !matched {
        return Err(PickupVerifyError::InvalidSignature);
    }
    let json = B64
        .decode(payload_b64.as_bytes())
        .map_err(|_| PickupVerifyError::Malformed)?;
    let payload: TokenPayload =
        serde_json::from_slice(&json).map_err(|_| PickupVerifyError::Malformed)?;
    Ok((payload, wire.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        [7u8; 32]
    }

    fn payload() -> TokenPayload {
        TokenPayload {
            raw_ref: "frame_abc".into(),
            service_id: "svc-1".into(),
            request_id: "req-1".into(),
            expiry_unix_ms: 1_000_000,
            one_shot: true,
        }
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let t = sign_payload(&key(), &payload());
        let wire = t.wire();
        let (decoded, _) = parse_and_verify_multi(std::iter::once(key().as_slice()), &wire).expect("verify ok");
        assert_eq!(decoded, payload());
    }

    #[test]
    fn tampered_signature_rejected() {
        let t = sign_payload(&key(), &payload());
        let mut wire = t.wire();
        // Flip last base64 char of the signature.
        let last = wire.pop().unwrap();
        let flipped = if last == 'A' { 'B' } else { 'A' };
        wire.push(flipped);
        assert_eq!(
            parse_and_verify_multi(std::iter::once(key().as_slice()), &wire).unwrap_err(),
            PickupVerifyError::InvalidSignature
        );
    }

    #[test]
    fn wrong_key_rejected() {
        let t = sign_payload(&key(), &payload());
        let wire = t.wire();
        assert_eq!(
            parse_and_verify_multi(std::iter::once([0u8; 32].as_slice()), &wire).unwrap_err(),
            PickupVerifyError::InvalidSignature
        );
    }

    #[test]
    fn malformed_wire_rejected() {
        assert_eq!(
            parse_and_verify_multi(std::iter::once(key().as_slice()), "no-dot").unwrap_err(),
            PickupVerifyError::Malformed
        );
        assert_eq!(
            parse_and_verify_multi(std::iter::once(key().as_slice()), ".onlysig").unwrap_err(),
            PickupVerifyError::Malformed
        );
        assert_eq!(
            parse_and_verify_multi(std::iter::once(key().as_slice()), "onlypayload.").unwrap_err(),
            PickupVerifyError::Malformed
        );
    }
}
