// =============================================================================
// File: protocol/frame/sign.rs — Ed25519 signature scope (UFP/2 §6.3)
// Purpose: compute and verify the Ed25519 signature that covers every
// immutable field of an Envelope. The hop-mutable `forwarded_via` (field 16)
// is excluded; `auth.signature` (inside field 13) is replaced by a 64-byte
// zero placeholder so the signed bytes are deterministic and the receiver
// can reconstruct them.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §6.3 + §5.4 + §5.5.
// =============================================================================

use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, Verifier, VerifyingKey};

use super::auth::AuthKind;
use super::envelope::{Envelope, NODE_ID_LEN, SIGNATURE_LEN};
use super::error::{FrameError, FrameErrorCode};
use super::flags::Flags;

/// All-zero 64-byte placeholder substituted for `auth.signature` while
/// computing the canonical bytes to sign. Receiver MUST recompute the same
/// placeholder when verifying so the bytes match the sender's input.
pub const SIGNATURE_PLACEHOLDER: [u8; SIGNATURE_LEN] = [0u8; SIGNATURE_LEN];

/// Encode the canonical bytes that go into Ed25519 sign/verify.
///
/// The bytes are the CBOR encoding of the envelope with two transformations:
///   1. `auth.signature` is set to a 64-byte zero placeholder (regardless of
///      whether the original was present, absent, or carried any value).
///   2. `forwarded_via` (field 16) is omitted entirely.
///
/// This function is the single source of truth for the bytes that flow into
/// Ed25519. It is exported for use by the AEAD AAD module (4c1c) and by
/// tests that need to verify signature determinism.
pub fn canonical_envelope_for_signing(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let mut probe = envelope.clone();
    probe.auth.signature = Some(SIGNATURE_PLACEHOLDER);
    probe.forwarded_via = None;
    let mut out = Vec::with_capacity(256);
    minicbor::encode(&probe, &mut out).map_err(|e| {
        FrameError::new(
            FrameErrorCode::CanonicalEncoding,
            format!("encode envelope-for-signing failed: {e}"),
        )
    })?;
    Ok(out)
}

/// Sign the envelope in place. Requires `flags & IS_SIGNED = 1` and
/// `auth.kind ∈ {NodeIdentity, UserIdentity}` (the only kinds that carry a
/// UFP/2 Ed25519 signature per §11.3).
///
/// The `auth.subject_id` MUST already be populated and MUST match the public
/// key derived from `signing_key`. Mismatch is a hard rejection: this
/// function returns `BodyValidationFailed` rather than silently signing an
/// envelope that downstream verifiers will reject. Populating `subject_id`
/// before calling is the caller's responsibility.
///
/// On success the envelope's `auth.signature` field is filled.
pub fn sign_envelope(envelope: &mut Envelope, signing_key: &SigningKey) -> Result<(), FrameError> {
    if !envelope.flags.contains(Flags::IS_SIGNED) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "sign_envelope: IS_SIGNED flag MUST be set before signing",
        )
        .with_path("envelope.flags"));
    }
    match envelope.auth.kind {
        AuthKind::NodeIdentity | AuthKind::UserIdentity => {}
        other => {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                format!(
                    "sign_envelope: auth.kind={:?} cannot carry an Ed25519 signature \
                     (only NodeIdentity/UserIdentity per §11.3)",
                    other
                ),
            )
            .with_path("envelope.auth.kind"));
        }
    }
    let expected_pubkey = signing_key.verifying_key().to_bytes();
    match envelope.auth.subject_id {
        Some(declared) if declared == expected_pubkey => {}
        Some(_) => {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "sign_envelope: auth.subject_id does not match signing key's public key",
            )
            .with_path("envelope.auth.subject_id"));
        }
        None => {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "sign_envelope: auth.subject_id MUST be present before signing",
            )
            .with_path("envelope.auth.subject_id"));
        }
    }
    let to_sign = canonical_envelope_for_signing(envelope)?;
    let sig: DalekSignature = signing_key.sign(&to_sign);
    let mut out = [0u8; SIGNATURE_LEN];
    out.copy_from_slice(&sig.to_bytes());
    envelope.auth.signature = Some(out);
    Ok(())
}

/// Verify the envelope's signature. The verifier reads `auth.subject_id` to
/// determine which public key to use, reconstructs the canonical signed
/// bytes (placeholder + forwarded_via omitted), and runs Ed25519 verify.
///
/// Pre-conditions enforced by this function:
/// - `flags & IS_SIGNED = 1`. Unsigned envelopes go through a different code
///   path (channel/kind policy + transport binding check, see 4c1g).
/// - `auth.kind ∈ {NodeIdentity, UserIdentity}`.
/// - `auth.subject_id` and `auth.signature` are present.
///
/// Returns `Ok(())` on successful verify, or a `FrameError` carrying the
/// appropriate §11 error code on any failure.
///
/// **IMPORTANT: this function verifies the signature against `auth.subject_id`
/// but does NOT bind `auth.subject_id` to `source.id`.** Per UFP/2 §11.3, a
/// well-formed signed envelope MUST satisfy `source.id == auth.subject_id`
/// when `auth.kind ∈ {NodeIdentity, UserIdentity}` — otherwise an attacker
/// with their own valid Ed25519 keypair could sign an envelope whose `source`
/// field names a different node. That source/subject binding check lives in
/// the 4c1g structural validator (`auth_invariants` module) which MUST run
/// before this function in any production receive pipeline. Do NOT use
/// `verify_envelope` standalone as a complete authentication gate.
pub fn verify_envelope(envelope: &Envelope) -> Result<(), FrameError> {
    if !envelope.flags.contains(Flags::IS_SIGNED) {
        return Err(FrameError::new(
            FrameErrorCode::InvalidSignature,
            "verify_envelope: IS_SIGNED flag is not set",
        )
        .with_path("envelope.flags"));
    }
    match envelope.auth.kind {
        AuthKind::NodeIdentity | AuthKind::UserIdentity => {}
        other => {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                format!(
                    "verify_envelope: auth.kind={:?} does not carry Ed25519 signatures \
                     in UFP/2 (§11.3)",
                    other
                ),
            )
            .with_path("envelope.auth.kind"));
        }
    }
    let subject_bytes = envelope.auth.subject_id.ok_or_else(|| {
        FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "verify_envelope: auth.subject_id missing on signed envelope",
        )
        .with_path("envelope.auth.subject_id")
    })?;
    let signature_bytes = envelope.auth.signature.ok_or_else(|| {
        FrameError::new(
            FrameErrorCode::InvalidSignature,
            "verify_envelope: auth.signature missing on IS_SIGNED envelope",
        )
        .with_path("envelope.auth.signature")
    })?;
    let public_key = VerifyingKey::from_bytes(&subject_bytes).map_err(|e| {
        FrameError::new(
            FrameErrorCode::InvalidSignature,
            format!("verify_envelope: auth.subject_id is not a valid Ed25519 public key: {e}"),
        )
        .with_path("envelope.auth.subject_id")
    })?;
    let sig = DalekSignature::from_bytes(&signature_bytes);
    let to_verify = canonical_envelope_for_signing(envelope)?;
    public_key.verify(&to_verify, &sig).map_err(|_| {
        FrameError::new(
            FrameErrorCode::InvalidSignature,
            "verify_envelope: Ed25519 signature verification failed",
        )
        .with_path("envelope.auth.signature")
    })?;
    Ok(())
}

/// Convenience: build a UFP/2 signing key from 32 raw secret bytes.
/// Wraps `SigningKey::from_bytes` so callers don't need to import dalek
/// types directly when they hold raw key material.
pub fn signing_key_from_bytes(secret: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(secret)
}

/// Convenience: derive the 32-byte Ed25519 public key (used in
/// `auth.subject_id` and `NodeAddress.id`) from a signing key.
pub fn public_key_bytes(signing_key: &SigningKey) -> [u8; NODE_ID_LEN] {
    signing_key.verifying_key().to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::canonical::validate_canonical;
    use crate::protocol::frame::address::NodeAddress;
    use crate::protocol::frame::auth::Auth;
    use crate::protocol::frame::channel::channels;
    use crate::protocol::frame::envelope::{MessageId, Priority, MESSAGE_ID_LEN};
    use crate::protocol::frame::flags::Flags;
    use rand_core::OsRng;

    fn fresh_key() -> SigningKey {
        let mut rng = OsRng;
        SigningKey::generate(&mut rng)
    }

    fn sample_signed_envelope(key: &SigningKey) -> Envelope {
        let pubkey = public_key_bytes(key);
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 0x01;
        mid[15] = 0xFE;
        let mut env = Envelope::minimal(
            NodeAddress::node(pubkey),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::MESH,
            crate::protocol::frame::channel::Kind(0x0010),
            Priority::Normal,
            Flags::NONE.with(Flags::IS_SIGNED),
            MessageId(mid),
            1_700_000_000_000,
        );
        env.auth = Auth::node_unsigned(pubkey, 7);
        env
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).expect("sign succeeds");
        verify_envelope(&env).expect("verify succeeds on freshly-signed envelope");
    }

    #[test]
    fn sign_populates_signature() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        assert!(env.auth.signature.is_none());
        sign_envelope(&mut env, &key).unwrap();
        assert!(env.auth.signature.is_some());
        assert_ne!(env.auth.signature.unwrap(), SIGNATURE_PLACEHOLDER);
    }

    #[test]
    fn signature_scope_excludes_forwarded_via() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.forwarded_via = Some(vec![
            NodeAddress::node([0x55u8; NODE_ID_LEN]),
            NodeAddress::node([0x66u8; NODE_ID_LEN]),
        ]);
        verify_envelope(&env).expect("forwarded_via mutation MUST NOT break signature");
    }

    #[test]
    fn signature_detects_body_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.body = b"original".to_vec();
        sign_envelope(&mut env, &key).unwrap();
        env.body = b"tampered".to_vec();
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_destination_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.destination = NodeAddress::node([0xAAu8; NODE_ID_LEN]);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_flag_bit_flip() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.flags = env.flags.with(Flags::IS_BROADCAST);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_epoch_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.auth.epoch = Some(9999);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_message_id_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        let mut tampered_mid = env.message_id.0;
        tampered_mid[0] ^= 0xFF;
        env.message_id = MessageId(tampered_mid);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn verify_rejects_when_is_signed_flag_clear() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.flags = env.flags.without(Flags::IS_SIGNED);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        // IS_SIGNED clear → signature path rejects with InvalidSignature
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn sign_rejects_when_is_signed_flag_clear() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.flags = Flags::NONE;
        let r = sign_envelope(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn sign_rejects_anonymous_auth_kind() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.auth = Auth::anonymous();
        let r = sign_envelope(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn sign_rejects_subject_id_mismatch() {
        let key = fresh_key();
        let other_key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.auth.subject_id = Some(public_key_bytes(&other_key));
        let r = sign_envelope(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn verify_rejects_signature_from_other_key() {
        let key_a = fresh_key();
        let key_b = fresh_key();
        let mut env = sample_signed_envelope(&key_a);
        sign_envelope(&mut env, &key_a).unwrap();
        env.auth.subject_id = Some(public_key_bytes(&key_b));
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn verify_rejects_missing_signature() {
        let key = fresh_key();
        let env = sample_signed_envelope(&key);
        // flags say IS_SIGNED but signature absent — receiver MUST reject
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_source_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.source = NodeAddress::node([0xDEu8; NODE_ID_LEN]);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_channel_kind_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.channel = channels::FRONTEND;
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_created_at_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        sign_envelope(&mut env, &key).unwrap();
        env.created_at_ms += 1;
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_fragment_index_tampering() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(2);
        env.fragment_count = Some(5);
        sign_envelope(&mut env, &key).unwrap();
        env.fragment_index = Some(3);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn signature_detects_is_last_fragment_flip() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(4);
        env.fragment_count = Some(5);
        sign_envelope(&mut env, &key).unwrap();
        env.flags = env.flags.with(Flags::IS_LAST_FRAGMENT);
        let r = verify_envelope(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }

    #[test]
    fn user_identity_sign_verify_roundtrip() {
        let key = fresh_key();
        let pubkey = public_key_bytes(&key);
        let mut env = sample_signed_envelope(&key);
        env.auth = Auth::user_unsigned(pubkey, 3);
        env.source = NodeAddress::user(pubkey);
        sign_envelope(&mut env, &key).unwrap();
        verify_envelope(&env).expect("UserIdentity roundtrip succeeds");
    }

    #[test]
    fn canonical_signing_bytes_pass_canonical_validator() {
        let key = fresh_key();
        let env = sample_signed_envelope(&key);
        let bytes = canonical_envelope_for_signing(&env).unwrap();
        validate_canonical(&bytes).expect("signing bytes must themselves be canonical CBOR");
    }

    #[test]
    fn canonical_signing_bytes_identical_with_and_without_forwarded_via() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        let bytes_no_hops = canonical_envelope_for_signing(&env).unwrap();
        env.forwarded_via = Some(vec![NodeAddress::node([0x77u8; NODE_ID_LEN])]);
        let bytes_with_hops = canonical_envelope_for_signing(&env).unwrap();
        assert_eq!(bytes_no_hops, bytes_with_hops);
    }

    #[test]
    fn canonical_signing_bytes_replace_existing_signature_with_placeholder() {
        let key = fresh_key();
        let mut env = sample_signed_envelope(&key);
        let bytes_no_sig = canonical_envelope_for_signing(&env).unwrap();
        env.auth.signature = Some([0xAAu8; SIGNATURE_LEN]);
        let bytes_with_sig = canonical_envelope_for_signing(&env).unwrap();
        assert_eq!(bytes_no_sig, bytes_with_sig);
    }
}
