// =============================================================================
// File: protocol/frame/pipeline.rs — send / receive transform pipeline (UFP/2 §7.1)
// Purpose: orchestrate the canonical transform order across compression,
// encryption, and signing. This is the single function callers SHOULD use
// to prepare a non-fragmented envelope for transmission, and the single
// function receivers SHOULD use to recover plaintext from an arriving
// envelope (modulo replay / dedup / fragmentation, which land in 4c1e+4c1f).
//
// Sender order (§7.1):
//   1. CBOR-encode payload → `envelope.body = plaintext`
//   2. if IS_COMPRESSED: body = lz4_compress(body)
//   3. if IS_ENCRYPTED:  body = nonce || aead_encrypt(body, aad) || tag
//   4. if IS_SIGNED:     auth.signature = ed25519_sign(canonical_envelope)
//
// Receiver order (§7.1):
//   0. domain gate: this pipeline serves NON-FRAGMENTED envelopes only. If
//      IS_FRAGMENT is set the caller MUST route the envelope through the
//      reassembly pipeline (4c1e) FIRST; that pipeline then invokes this
//      function on the reassembled logical envelope. Returns
//      FragmentAssemblyError immediately on IS_FRAGMENT to make the routing
//      mistake loud rather than silently mis-validating a fragment.
//   1. if IS_SIGNED:     verify ed25519_signature
//   2. if IS_ENCRYPTED:  body = aead_decrypt(body, aad)
//   3. if IS_COMPRESSED: body = lz4_decompress(body)
//   4. caller does CBOR decode + dispatch
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §7.1.
// =============================================================================

use ed25519_dalek::SigningKey;

use super::aead::{decrypt_envelope_body, encrypt_envelope_body, AeadKey, NonceCounter};
use super::compress::{compress_body, decompress_body};
use super::envelope::Envelope;
use super::error::{FrameError, FrameErrorCode};
use super::flags::Flags;
use super::sign::{sign_envelope, verify_envelope};

/// Crypto material the sender pipeline may need. Pass `None` for absent
/// features (e.g. unsigned envelopes set `signing_key: None`).
pub struct SendCrypto<'a> {
    pub signing_key: Option<&'a SigningKey>,
    pub aead_key: Option<&'a AeadKey>,
    pub nonce_counter: Option<&'a mut NonceCounter>,
}

/// Crypto material the receiver pipeline may need. Pass `None` when the
/// corresponding feature is not in use.
pub struct ReceiveCrypto<'a> {
    pub aead_key: Option<&'a AeadKey>,
}

/// Prepare an envelope for transmission per §7.1 sender pipeline.
///
/// Pre-conditions:
/// - `envelope.body` already holds the CBOR-encoded payload bytes (caller
///   responsibility — pipeline does not generate the payload).
/// - Feature flags are set correctly: `IS_COMPRESSED`, `IS_ENCRYPTED`,
///   `IS_SIGNED` reflect what the caller WANTS to apply.
/// - For `IS_ENCRYPTED`: `crypto.aead_key` and `crypto.nonce_counter` MUST
///   be `Some`. The AAD computed during encryption includes the current
///   `flags`, so callers MUST set all flag bits before calling.
/// - For `IS_SIGNED`: `crypto.signing_key` MUST be `Some` and `auth` MUST
///   already carry `kind` ∈ {NodeIdentity, UserIdentity} with `subject_id`
///   matching the signing key's public key.
/// - `IS_FRAGMENT` MUST be 0 (fragmentation is the caller's responsibility
///   after this pipeline completes — see 4c1e).
///
/// On success the envelope is ready for canonical CBOR encoding and wire
/// transmission. On failure the envelope's body MAY be in a partially-
/// transformed state; callers should discard such envelopes.
pub fn send_envelope_pipeline(
    envelope: &mut Envelope,
    crypto: SendCrypto<'_>,
) -> Result<(), FrameError> {
    if envelope.flags.contains(Flags::IS_FRAGMENT) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "send_envelope_pipeline: IS_FRAGMENT set; call the fragmentation pipeline (4c1e) instead",
        )
        .with_path("envelope.flags"));
    }

    if envelope.flags.contains(Flags::IS_COMPRESSED) {
        let compressed = compress_body(&envelope.body)?;
        envelope.body = compressed;
    }

    if envelope.flags.contains(Flags::IS_ENCRYPTED) {
        let key = crypto.aead_key.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "send_envelope_pipeline: IS_ENCRYPTED set but no AEAD key provided",
            )
        })?;
        let counter = crypto.nonce_counter.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "send_envelope_pipeline: IS_ENCRYPTED set but no NonceCounter provided",
            )
        })?;
        encrypt_envelope_body(envelope, key, counter)?;
    }

    if envelope.flags.contains(Flags::IS_SIGNED) {
        let key = crypto.signing_key.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "send_envelope_pipeline: IS_SIGNED set but no signing key provided",
            )
        })?;
        sign_envelope(envelope, key)?;
    }

    Ok(())
}

/// Recover plaintext from a received envelope per §7.1 receiver pipeline.
///
/// Steps applied (the fragment gate is unconditional; remaining steps run
/// only when the corresponding flag is set):
/// 1. `IS_FRAGMENT=1`: REJECT immediately — fragmentation reassembly is the
///    caller's responsibility (see 4c1e). Non-fragmented pipeline only.
/// 2. `IS_SIGNED=1`: verify Ed25519 signature (§6.3). Reject on failure.
/// 3. `IS_ENCRYPTED=1`: AEAD-decrypt body in place.
/// 4. `IS_COMPRESSED=1`: lz4-decompress body in place.
///
/// On success `envelope.body` holds the recovered plaintext (CBOR-encoded
/// payload) ready for application decode and dispatch.
///
/// **IMPORTANT**: this pipeline does NOT enforce §11.3 auth invariants
/// (auth.kind ↔ flag/field consistency, source.id ↔ auth.subject_id binding,
/// channel/kind range checks, reserved flag bits = 0). Those live in 4c1g
/// `structural` validator and MUST run before this pipeline in any
/// production receive path. The pipeline assumes the envelope has already
/// passed structural validation.
pub fn receive_envelope_pipeline(
    envelope: &mut Envelope,
    crypto: ReceiveCrypto<'_>,
) -> Result<(), FrameError> {
    if envelope.flags.contains(Flags::IS_FRAGMENT) {
        return Err(FrameError::new(
            FrameErrorCode::FragmentAssemblyError,
            "receive_envelope_pipeline: IS_FRAGMENT set; call the reassembly pipeline (4c1e) first",
        )
        .with_path("envelope.flags"));
    }

    if envelope.flags.contains(Flags::IS_SIGNED) {
        verify_envelope(envelope)?;
    }

    if envelope.flags.contains(Flags::IS_ENCRYPTED) {
        let key = crypto.aead_key.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "receive_envelope_pipeline: IS_ENCRYPTED set but no AEAD key provided",
            )
        })?;
        decrypt_envelope_body(envelope, key)?;
    }

    if envelope.flags.contains(Flags::IS_COMPRESSED) {
        let plaintext = decompress_body(&envelope.body)?;
        envelope.body = plaintext;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::address::NodeAddress;
    use crate::protocol::frame::auth::Auth;
    use crate::protocol::frame::channel::{channels, Kind};
    use crate::protocol::frame::envelope::{MessageId, Priority, MESSAGE_ID_LEN, NODE_ID_LEN};
    use crate::protocol::frame::sign::public_key_bytes;
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    fn fresh_signing_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    fn aead_key_a() -> AeadKey {
        AeadKey::from_bytes([0xA5u8; 32])
    }

    fn sample_envelope() -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 0xDE;
        mid[15] = 0xAD;
        Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::FRONTEND,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE,
            MessageId(mid),
            1_700_000_000_000,
        )
    }

    fn payload() -> Vec<u8> {
        // Big enough to be worth compressing (and to exercise lz4 frame
        // encoder past its minimum block size).
        b"UFP/2 pipeline test payload - repeated to make compression meaningful. ".repeat(128)
    }

    #[test]
    fn roundtrip_plain_no_features() {
        let mut env = sample_envelope();
        let pt = payload();
        env.body = pt.clone();
        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        assert_eq!(env.body, pt, "plain pipeline leaves body untouched");
        receive_envelope_pipeline(&mut env, ReceiveCrypto { aead_key: None }).unwrap();
        assert_eq!(env.body, pt);
    }

    #[test]
    fn roundtrip_compressed_only() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_COMPRESSED);
        let pt = payload();
        env.body = pt.clone();
        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        assert_ne!(env.body, pt);
        assert!(env.body.len() < pt.len());
        receive_envelope_pipeline(&mut env, ReceiveCrypto { aead_key: None }).unwrap();
        assert_eq!(env.body, pt);
    }

    #[test]
    fn roundtrip_encrypted_only() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_ENCRYPTED);
        let pt = payload();
        env.body = pt.clone();
        let key = aead_key_a();
        let mut nc = NonceCounter::new([0u8; 4]);
        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: Some(&key),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();
        assert_ne!(env.body, pt);
        receive_envelope_pipeline(
            &mut env,
            ReceiveCrypto {
                aead_key: Some(&key),
            },
        )
        .unwrap();
        assert_eq!(env.body, pt);
    }

    #[test]
    fn roundtrip_signed_only() {
        let key = fresh_signing_key();
        let pubkey = public_key_bytes(&key);
        let mut env = sample_envelope();
        env.source = NodeAddress::node(pubkey);
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth::node_unsigned(pubkey, 0);
        env.body = payload();
        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: Some(&key),
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        assert!(env.auth.signature.is_some());
        receive_envelope_pipeline(&mut env, ReceiveCrypto { aead_key: None }).unwrap();
    }

    #[test]
    fn roundtrip_compressed_encrypted_signed_all_at_once() {
        let signing_key = fresh_signing_key();
        let pubkey = public_key_bytes(&signing_key);
        let aead_key = aead_key_a();
        let mut nc = NonceCounter::new([0xAAu8; 4]);

        let mut env = sample_envelope();
        env.source = NodeAddress::node(pubkey);
        env.flags = env
            .flags
            .with(Flags::IS_COMPRESSED)
            .with(Flags::IS_ENCRYPTED)
            .with(Flags::IS_SIGNED);
        env.auth = Auth::node_unsigned(pubkey, 11);
        let pt = payload();
        env.body = pt.clone();

        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: Some(&signing_key),
                aead_key: Some(&aead_key),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();

        // Wire bytes are now compressed → encrypted; signature is present.
        assert!(env.auth.signature.is_some());
        assert_ne!(env.body, pt);

        receive_envelope_pipeline(
            &mut env,
            ReceiveCrypto {
                aead_key: Some(&aead_key),
            },
        )
        .unwrap();
        assert_eq!(env.body, pt);
    }

    #[test]
    fn pipeline_order_is_compress_then_encrypt() {
        // If pipeline applied encrypt-then-compress, the compressed body
        // would shrink very little (encrypted data is near-incompressible).
        // Compress-then-encrypt preserves the compression ratio because
        // lz4 runs on the structured plaintext.
        let signing_key = fresh_signing_key();
        let pubkey = public_key_bytes(&signing_key);
        let aead_key = aead_key_a();
        let mut nc = NonceCounter::new([0u8; 4]);

        let mut env_a = sample_envelope();
        env_a.source = NodeAddress::node(pubkey);
        env_a.flags = env_a
            .flags
            .with(Flags::IS_COMPRESSED)
            .with(Flags::IS_ENCRYPTED);
        env_a.body = vec![0xCCu8; 16 * 1024];
        let plain_len = env_a.body.len();

        send_envelope_pipeline(
            &mut env_a,
            SendCrypto {
                signing_key: None,
                aead_key: Some(&aead_key),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();

        // Highly redundant 16 KiB plaintext → compress shrinks dramatically,
        // then encrypt adds nonce(12) + tag(16). Final wire size should be
        // well under the plaintext length.
        assert!(
            env_a.body.len() < plain_len / 4,
            "compress-then-encrypt failed to shrink redundant payload; got {} from {}",
            env_a.body.len(),
            plain_len
        );

        let _ = signing_key;
        let _ = nc;
    }

    #[test]
    fn send_rejects_fragment_flag() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(0);
        env.fragment_count = Some(2);
        let r = send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        );
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn receive_rejects_fragment_flag() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(0);
        env.fragment_count = Some(2);
        let r = receive_envelope_pipeline(&mut env, ReceiveCrypto { aead_key: None });
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
    }

    #[test]
    fn send_rejects_encrypted_without_key() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_ENCRYPTED);
        env.body = payload();
        let r = send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        );
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn receive_rejects_encrypted_without_key() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_ENCRYPTED);
        env.body = vec![0u8; 64]; // dummy bytes
        let r = receive_envelope_pipeline(&mut env, ReceiveCrypto { aead_key: None });
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    #[test]
    fn send_rejects_signed_without_key() {
        let mut env = sample_envelope();
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.auth = Auth::node_unsigned([0x33u8; NODE_ID_LEN], 0);
        env.body = payload();
        let r = send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        );
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn receive_pipeline_detects_tampered_signature_before_decrypt() {
        let signing_key = fresh_signing_key();
        let pubkey = public_key_bytes(&signing_key);
        let aead_key = aead_key_a();
        let mut nc = NonceCounter::new([0u8; 4]);

        let mut env = sample_envelope();
        env.source = NodeAddress::node(pubkey);
        env.flags = env.flags.with(Flags::IS_ENCRYPTED).with(Flags::IS_SIGNED);
        env.auth = Auth::node_unsigned(pubkey, 0);
        env.body = payload();
        send_envelope_pipeline(
            &mut env,
            SendCrypto {
                signing_key: Some(&signing_key),
                aead_key: Some(&aead_key),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();

        // Tamper signature only — pipeline must reject at step 1 (sig verify),
        // before wasting work on AEAD decrypt.
        let mut bad_sig = env.auth.signature.unwrap();
        bad_sig[0] ^= 0x01;
        env.auth.signature = Some(bad_sig);

        let r = receive_envelope_pipeline(
            &mut env,
            ReceiveCrypto {
                aead_key: Some(&aead_key),
            },
        );
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::InvalidSignature);
    }
}
