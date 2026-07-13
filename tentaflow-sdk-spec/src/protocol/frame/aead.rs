// =============================================================================
// File: protocol/frame/aead.rs — ChaCha20-Poly1305 AEAD (UFP/2 §7.2)
// Purpose: encrypt and decrypt envelope bodies under a per-pair session key.
// Wire layout when IS_ENCRYPTED is set: body = nonce(12) || ciphertext || tag(16).
// AAD covers canonical CBOR of envelope fields 0–8 + 10–12 (no body, no auth,
// no fragment fields, no forwarded_via). For fragmented messages, AAD uses
// `aad_flags = flags & !IS_LAST_FRAGMENT` so every fragment shares AAD.
//
// Nonce construction (§7.2): 4-byte random session prefix || 8-byte monotonic
// counter (big-endian). One NonceCounter per (key, direction) pair. Rotate
// before counter == 2^48 OR session age > 24h, whichever comes first.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §7.1 + §7.2 + §10.0.
// =============================================================================

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::address::NodeAddress;
use super::auth::Auth;
use super::envelope::Envelope;
use super::error::{FrameError, FrameErrorCode};
use super::flags::Flags;

/// Length of the AEAD nonce on the wire (RFC 8439 fixes this at 96 bits).
pub const AEAD_NONCE_LEN: usize = 12;

/// Length of the Poly1305 authentication tag.
pub const AEAD_TAG_LEN: usize = 16;

/// Length of the symmetric AEAD key.
pub const AEAD_KEY_LEN: usize = 32;

/// Length of the random session prefix portion of the nonce (§7.2).
pub const NONCE_PREFIX_LEN: usize = 4;

/// Length of the monotonic counter portion of the nonce (§7.2).
pub const NONCE_COUNTER_LEN: usize = 8;

/// Maximum counter value before key rotation MUST occur. 2^48 keeps a wide
/// safety margin below ChaCha20-Poly1305's 2^64 nonce ceiling.
pub const COUNTER_ROTATION_THRESHOLD: u64 = 1u64 << 48;

/// Maximum session age before key rotation MUST occur (24 hours).
pub const MAX_KEY_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// 256-bit AEAD key material. Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct AeadKey(pub [u8; AEAD_KEY_LEN]);

impl AeadKey {
    pub fn from_bytes(bytes: [u8; AEAD_KEY_LEN]) -> Self {
        Self(bytes)
    }
}

impl core::fmt::Debug for AeadKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AeadKey(<redacted>)")
    }
}

/// Per-session monotonic nonce generator (§7.2). One instance per direction
/// per session key. Senders advance the counter on each encrypt; receivers
/// do NOT consult a counter (they read the nonce from the wire). Persistence
/// is the caller's responsibility — see `next_nonce_with_persistence_hint`.
#[derive(Debug, Clone)]
pub struct NonceCounter {
    prefix: [u8; NONCE_PREFIX_LEN],
    counter: u64,
}

/// Returned by `next_nonce_with_persistence_hint` to advise the caller
/// whether they should flush the counter to durable storage before the next
/// encrypt call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistHint {
    /// No flush needed yet.
    NotYet,
    /// Caller SHOULD persist the counter to durable storage now. Triggered
    /// every `flush_every` increments (default 1024). Recovery semantics:
    /// on restart, advance the persisted counter by `flush_every` to skip
    /// any potentially-unflushed nonce values.
    FlushNow,
    /// Key rotation MUST happen before the next encrypt. The counter is at
    /// the rotation threshold; further encrypts under this key would risk
    /// nonce reuse if any earlier-session key material persisted.
    RotateKey,
}

impl NonceCounter {
    /// Create a counter with a freshly-randomised 4-byte prefix. Counter
    /// starts at 0. Caller supplies the entropy source.
    pub fn new(prefix: [u8; NONCE_PREFIX_LEN]) -> Self {
        Self { prefix, counter: 0 }
    }

    /// Resume from a previously-persisted counter value. Callers MUST add
    /// at least `flush_every` (per `next_nonce_with_persistence_hint`) to
    /// the persisted value before resuming, to skip any unflushed range.
    pub fn resume(prefix: [u8; NONCE_PREFIX_LEN], counter: u64) -> Self {
        Self { prefix, counter }
    }

    pub fn prefix(&self) -> [u8; NONCE_PREFIX_LEN] {
        self.prefix
    }

    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Yield the next 12-byte nonce. Errors if the counter would exceed
    /// `COUNTER_ROTATION_THRESHOLD` — caller MUST rotate the key.
    pub fn next_nonce(&mut self) -> Result<[u8; AEAD_NONCE_LEN], FrameError> {
        if self.counter >= COUNTER_ROTATION_THRESHOLD {
            return Err(FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "NonceCounter: counter reached rotation threshold (2^48); rotate key before encrypting again",
            ));
        }
        let mut nonce = [0u8; AEAD_NONCE_LEN];
        nonce[..NONCE_PREFIX_LEN].copy_from_slice(&self.prefix);
        nonce[NONCE_PREFIX_LEN..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter = self.counter.saturating_add(1);
        Ok(nonce)
    }

    /// Yield the next nonce plus a persistence hint per the `flush_every`
    /// cadence. Typical production setting is `flush_every = 1024`.
    pub fn next_nonce_with_persistence_hint(
        &mut self,
        flush_every: u64,
    ) -> Result<([u8; AEAD_NONCE_LEN], PersistHint), FrameError> {
        let nonce = self.next_nonce()?;
        let hint = if self.counter >= COUNTER_ROTATION_THRESHOLD {
            PersistHint::RotateKey
        } else if flush_every > 0 && self.counter % flush_every == 0 {
            PersistHint::FlushNow
        } else {
            PersistHint::NotYet
        };
        Ok((nonce, hint))
    }
}

/// Build the AAD bytes for a given envelope per §7.2.
///
/// AAD layout: canonical CBOR map containing keys 0–8 + 10–12 (only the
/// present optionals among 10–12). Key 9 `body`, key 13 `auth`, fragment
/// keys 14–15, and hop trail key 16 are EXCLUDED.
///
/// For fragmented envelopes (flags & IS_FRAGMENT), the `flags` value placed
/// in AAD has IS_LAST_FRAGMENT masked off (`Flags::aad_flags`) so that
/// every fragment of one logical message produces the same AAD.
pub fn compute_aad(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    // §7.2 split: only fragmented envelopes mask IS_LAST_FRAGMENT (so every
    // fragment of one logical message yields identical AAD). Non-fragmented
    // envelopes carry their flags as-is so AAD binds every bit, including
    // any accidental IS_LAST_FRAGMENT (which the 4c1g structural validator
    // will independently reject as an illegal flag combination per §3.4).
    let aad_flags = if envelope.flags.contains(Flags::IS_FRAGMENT) {
        Flags(envelope.flags.aad_flags())
    } else {
        envelope.flags
    };

    let key_count: u64 = 9
        + envelope.correlation_id.is_some() as u64
        + envelope.trace_id.is_some() as u64
        + envelope.ttl_ms.is_some() as u64;

    let mut buf = Vec::with_capacity(128);
    let mut e = minicbor::Encoder::new(&mut buf);
    e.map(key_count).map_err(encode_err)?;

    e.u8(0).map_err(encode_err)?;
    e.encode(&envelope.protocol_version).map_err(encode_err)?;

    e.u8(1).map_err(encode_err)?;
    e.encode(&envelope.message_id).map_err(encode_err)?;

    e.u8(2).map_err(encode_err)?;
    e.encode(&envelope.source).map_err(encode_err)?;

    e.u8(3).map_err(encode_err)?;
    e.encode(&envelope.destination).map_err(encode_err)?;

    e.u8(4).map_err(encode_err)?;
    e.i64(envelope.created_at_ms).map_err(encode_err)?;

    e.u8(5).map_err(encode_err)?;
    e.encode(&aad_flags).map_err(encode_err)?;

    e.u8(6).map_err(encode_err)?;
    e.encode(&envelope.priority).map_err(encode_err)?;

    e.u8(7).map_err(encode_err)?;
    e.encode(&envelope.channel).map_err(encode_err)?;

    e.u8(8).map_err(encode_err)?;
    e.encode(&envelope.kind).map_err(encode_err)?;

    if let Some(cid) = &envelope.correlation_id {
        e.u8(10).map_err(encode_err)?;
        e.encode(cid).map_err(encode_err)?;
    }
    if let Some(tid) = &envelope.trace_id {
        e.u8(11).map_err(encode_err)?;
        e.encode(tid).map_err(encode_err)?;
    }
    if let Some(ttl) = envelope.ttl_ms {
        e.u8(12).map_err(encode_err)?;
        e.u32(ttl).map_err(encode_err)?;
    }

    Ok(buf)
}

fn encode_err<E: core::fmt::Display>(err: minicbor::encode::Error<E>) -> FrameError {
    FrameError::new(
        FrameErrorCode::CanonicalEncoding,
        format!("compute_aad: encode failed: {}", err),
    )
}

/// AEAD-encrypt a plaintext body under the given key + nonce + envelope AAD.
/// Returns `nonce || ciphertext || tag` ready to place into `envelope.body`.
///
/// The caller is responsible for:
/// - Setting `flags & IS_ENCRYPTED` on the envelope BEFORE computing AAD.
/// - Deriving the nonce from a `NonceCounter` (one per session direction).
/// - Filling `envelope.body` with the returned bytes.
pub fn encrypt_body(
    plaintext: &[u8],
    key: &AeadKey,
    nonce: &[u8; AEAD_NONCE_LEN],
    aad: &[u8],
) -> Result<Vec<u8>, FrameError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key.0));
    let nonce_obj = Nonce::from_slice(nonce);
    let mut wire = Vec::with_capacity(AEAD_NONCE_LEN + plaintext.len() + AEAD_TAG_LEN);
    wire.extend_from_slice(nonce);
    let ct = cipher
        .encrypt(
            nonce_obj,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| {
            FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "encrypt_body: ChaCha20-Poly1305 encrypt failed",
            )
        })?;
    wire.extend_from_slice(&ct);
    Ok(wire)
}

/// AEAD-decrypt the body of an `IS_ENCRYPTED` envelope. Extracts the leading
/// 12-byte nonce, runs AEAD verify+decrypt with `aad`, returns plaintext.
///
/// Returns `DecryptionFailed` (§11 code 0x000A) on any failure (auth tag
/// mismatch, AAD mismatch, too-short body, malformed input).
pub fn decrypt_body(wire_body: &[u8], key: &AeadKey, aad: &[u8]) -> Result<Vec<u8>, FrameError> {
    if wire_body.len() < AEAD_NONCE_LEN + AEAD_TAG_LEN {
        return Err(FrameError::new(
            FrameErrorCode::DecryptionFailed,
            "decrypt_body: wire body too short to contain nonce+tag",
        ));
    }
    let (nonce_bytes, ct_with_tag) = wire_body.split_at(AEAD_NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key.0));
    let nonce_obj = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(
            nonce_obj,
            Payload {
                msg: ct_with_tag,
                aad,
            },
        )
        .map_err(|_| {
            FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "decrypt_body: ChaCha20-Poly1305 decrypt failed (bad key/nonce/AAD/tag)",
            )
        })
}

/// Convenience: end-to-end encrypt an envelope's body in place.
///
/// Requirements:
/// - `flags & IS_ENCRYPTED` MUST already be set (AAD includes flags; setting
///   IS_ENCRYPTED after AAD computation would invalidate the tag).
/// - The body MUST currently hold the plaintext (caller's responsibility).
///
/// After this call, `envelope.body` holds `nonce || ciphertext || tag`.
pub fn encrypt_envelope_body(
    envelope: &mut Envelope,
    key: &AeadKey,
    nonce_counter: &mut NonceCounter,
) -> Result<(), FrameError> {
    if !envelope.flags.contains(Flags::IS_ENCRYPTED) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "encrypt_envelope_body: IS_ENCRYPTED flag MUST be set before encryption",
        )
        .with_path("envelope.flags"));
    }
    let nonce = nonce_counter.next_nonce()?;
    let aad = compute_aad(envelope)?;
    let wire = encrypt_body(&envelope.body, key, &nonce, &aad)?;
    envelope.body = wire;
    Ok(())
}

/// Convenience: decrypt an envelope's body in place. Replaces the ciphertext
/// in `envelope.body` with the recovered plaintext on success.
pub fn decrypt_envelope_body(envelope: &mut Envelope, key: &AeadKey) -> Result<(), FrameError> {
    if !envelope.flags.contains(Flags::IS_ENCRYPTED) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "decrypt_envelope_body: IS_ENCRYPTED flag is not set",
        )
        .with_path("envelope.flags"));
    }
    let aad = compute_aad(envelope)?;
    let plaintext = decrypt_body(&envelope.body, key, &aad)?;
    envelope.body = plaintext;
    Ok(())
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;
    use crate::protocol::frame::channel::{channels, Kind};
    use crate::protocol::frame::envelope::{
        MessageId, Priority, TraceId, MESSAGE_ID_LEN, NODE_ID_LEN,
    };

    fn key1() -> AeadKey {
        AeadKey::from_bytes([0xA5u8; AEAD_KEY_LEN])
    }

    fn key2() -> AeadKey {
        AeadKey::from_bytes([0x5Au8; AEAD_KEY_LEN])
    }

    fn sample_encrypted_envelope() -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 0xCA;
        mid[15] = 0xFE;
        let mut env = Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::FRONTEND,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE.with(Flags::IS_ENCRYPTED),
            MessageId(mid),
            1_700_000_000_000,
        );
        env.body = b"sekret plaintext payload".to_vec();
        env
    }

    #[test]
    fn nonce_counter_starts_at_zero_and_increments() {
        let mut nc = NonceCounter::new([0xDEu8, 0xAD, 0xBE, 0xEF]);
        let n1 = nc.next_nonce().unwrap();
        let n2 = nc.next_nonce().unwrap();
        assert_eq!(&n1[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(&n1[4..], &0u64.to_be_bytes());
        assert_eq!(&n2[4..], &1u64.to_be_bytes());
        assert_eq!(nc.counter(), 2);
    }

    #[test]
    fn nonce_counter_never_repeats_for_same_session() {
        let mut nc = NonceCounter::new([0x11u8; 4]);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1024 {
            let n = nc.next_nonce().unwrap();
            assert!(seen.insert(n));
        }
    }

    #[test]
    fn nonce_counter_resume_skips_unflushed() {
        let mut nc = NonceCounter::resume([0u8; 4], 4096);
        let n = nc.next_nonce().unwrap();
        assert_eq!(&n[4..], &4096u64.to_be_bytes());
    }

    #[test]
    fn nonce_counter_persistence_hint_triggers_at_flush_every() {
        let mut nc = NonceCounter::new([0u8; 4]);
        let (_, h1) = nc.next_nonce_with_persistence_hint(4).unwrap();
        let (_, h2) = nc.next_nonce_with_persistence_hint(4).unwrap();
        let (_, h3) = nc.next_nonce_with_persistence_hint(4).unwrap();
        let (_, h4) = nc.next_nonce_with_persistence_hint(4).unwrap();
        assert_eq!(h1, PersistHint::NotYet);
        assert_eq!(h2, PersistHint::NotYet);
        assert_eq!(h3, PersistHint::NotYet);
        assert_eq!(h4, PersistHint::FlushNow);
    }

    #[test]
    fn nonce_counter_rotation_threshold_blocks_further_use() {
        let mut nc = NonceCounter::resume([0u8; 4], COUNTER_ROTATION_THRESHOLD);
        let r = nc.next_nonce();
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        let plaintext = env.body.clone();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        assert_ne!(env.body, plaintext);
        decrypt_envelope_body(&mut env, &key).unwrap();
        assert_eq!(env.body, plaintext);
    }

    #[test]
    fn decrypt_rejects_wrong_key() {
        let key_a = key1();
        let key_b = key2();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        encrypt_envelope_body(&mut env, &key_a, &mut nc).unwrap();
        let r = decrypt_envelope_body(&mut env, &key_b);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    #[test]
    fn decrypt_detects_destination_aad_tampering() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        // Tamper destination — AAD changes — tag mismatches.
        env.destination = NodeAddress::node([0xEEu8; NODE_ID_LEN]);
        let r = decrypt_envelope_body(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    #[test]
    fn decrypt_detects_message_id_aad_tampering() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        let mut tampered = env.message_id.0;
        tampered[0] ^= 0x80;
        env.message_id = MessageId(tampered);
        let r = decrypt_envelope_body(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    #[test]
    fn decrypt_detects_ciphertext_tampering() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        // Flip a ciphertext byte (past the 12-byte nonce, before the tag).
        if env.body.len() > 13 {
            env.body[13] ^= 0xFF;
        }
        let r = decrypt_envelope_body(&mut env, &key);
        assert!(r.is_err());
    }

    #[test]
    fn decrypt_rejects_too_short_body() {
        let key = key1();
        let aad = b"any";
        let r = decrypt_body(&[0u8; 5], &key, aad);
        assert!(r.is_err());
    }

    #[test]
    fn encrypt_rejects_when_is_encrypted_flag_clear() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        env.flags = Flags::NONE;
        let r = encrypt_envelope_body(&mut env, &key, &mut nc);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn aad_excludes_body_field() {
        let env_a = sample_encrypted_envelope();
        let mut env_b = sample_encrypted_envelope();
        env_b.body = b"different body bytes entirely".to_vec();
        let aad_a = compute_aad(&env_a).unwrap();
        let aad_b = compute_aad(&env_b).unwrap();
        assert_eq!(aad_a, aad_b, "AAD MUST NOT depend on body");
    }

    #[test]
    fn aad_excludes_auth_field() {
        let env_a = sample_encrypted_envelope();
        let mut env_b = sample_encrypted_envelope();
        env_b.auth = Auth::node_unsigned([0xFFu8; NODE_ID_LEN], 99);
        let aad_a = compute_aad(&env_a).unwrap();
        let aad_b = compute_aad(&env_b).unwrap();
        assert_eq!(aad_a, aad_b, "AAD MUST NOT depend on auth");
    }

    #[test]
    fn aad_excludes_forwarded_via_field() {
        let env_a = sample_encrypted_envelope();
        let mut env_b = sample_encrypted_envelope();
        env_b.forwarded_via = Some(vec![NodeAddress::node([0x77u8; NODE_ID_LEN])]);
        let aad_a = compute_aad(&env_a).unwrap();
        let aad_b = compute_aad(&env_b).unwrap();
        assert_eq!(aad_a, aad_b, "AAD MUST NOT depend on forwarded_via");
    }

    #[test]
    fn aad_masks_is_last_fragment_bit() {
        let mut env_first = sample_encrypted_envelope();
        env_first.flags = env_first.flags.with(Flags::IS_FRAGMENT);
        env_first.fragment_index = Some(0);
        env_first.fragment_count = Some(3);

        let mut env_last = sample_encrypted_envelope();
        env_last.flags = env_last
            .flags
            .with(Flags::IS_FRAGMENT)
            .with(Flags::IS_LAST_FRAGMENT);
        env_last.fragment_index = Some(2);
        env_last.fragment_count = Some(3);

        let aad_first = compute_aad(&env_first).unwrap();
        let aad_last = compute_aad(&env_last).unwrap();
        assert_eq!(
            aad_first, aad_last,
            "fragmented envelopes of one logical message MUST share AAD (IS_LAST_FRAGMENT masked off)"
        );
    }

    #[test]
    fn aad_includes_optional_fields_when_present() {
        let env_no_opts = sample_encrypted_envelope();
        let mut env_with_opts = sample_encrypted_envelope();
        env_with_opts.correlation_id = Some(MessageId([0xAAu8; MESSAGE_ID_LEN]));
        env_with_opts.trace_id = Some(TraceId([0xBBu8; 16]));
        env_with_opts.ttl_ms = Some(5_000);
        let aad_no_opts = compute_aad(&env_no_opts).unwrap();
        let aad_with_opts = compute_aad(&env_with_opts).unwrap();
        assert_ne!(aad_no_opts, aad_with_opts);
    }

    #[test]
    fn aead_key_debug_does_not_leak_bytes() {
        let key = key1();
        let dbg = format!("{:?}", key);
        assert!(!dbg.contains("A5"));
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn aead_key_zeroizes_on_drop() {
        // Indirect verification: build a key inside a scope, then check that
        // the type implements ZeroizeOnDrop (compile-time guarantee).
        fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}
        assert_zeroize_on_drop::<AeadKey>();
    }

    #[test]
    fn aad_non_fragmented_does_not_mask_last_fragment() {
        let env_a = sample_encrypted_envelope();
        let mut env_b = sample_encrypted_envelope();
        env_b.flags = env_b.flags.with(Flags::IS_LAST_FRAGMENT);
        let aad_a = compute_aad(&env_a).unwrap();
        let aad_b = compute_aad(&env_b).unwrap();
        assert_ne!(
            aad_a, aad_b,
            "non-fragmented envelopes MUST bind every flag bit including IS_LAST_FRAGMENT"
        );
    }

    #[test]
    fn aad_bytes_are_canonical_cbor() {
        use crate::protocol::canonical::validate_canonical;
        let env = sample_encrypted_envelope();
        let aad = compute_aad(&env).unwrap();
        validate_canonical(&aad).expect("AAD bytes must themselves be canonical CBOR");
    }

    #[test]
    fn aad_binds_priority_channel_kind_and_created_at() {
        let base = sample_encrypted_envelope();
        let base_aad = compute_aad(&base).unwrap();
        let mut v = sample_encrypted_envelope();
        v.priority = Priority::Control;
        assert_ne!(compute_aad(&v).unwrap(), base_aad);
        let mut v = sample_encrypted_envelope();
        v.channel = channels::DOMAIN;
        assert_ne!(compute_aad(&v).unwrap(), base_aad);
        let mut v = sample_encrypted_envelope();
        v.kind = Kind(0x1234);
        assert_ne!(compute_aad(&v).unwrap(), base_aad);
        let mut v = sample_encrypted_envelope();
        v.created_at_ms += 1;
        assert_ne!(compute_aad(&v).unwrap(), base_aad);
        let mut v = sample_encrypted_envelope();
        v.source = NodeAddress::node([0xEEu8; NODE_ID_LEN]);
        assert_ne!(compute_aad(&v).unwrap(), base_aad);
    }

    #[test]
    fn nonce_counter_returns_rotate_hint_when_threshold_reached() {
        let mut nc = NonceCounter::resume([0u8; 4], COUNTER_ROTATION_THRESHOLD - 1);
        let (_, hint) = nc.next_nonce_with_persistence_hint(1024).unwrap();
        assert_eq!(hint, PersistHint::RotateKey);
    }

    #[test]
    fn decrypt_detects_auth_tag_tampering() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        let last = env.body.len() - 1;
        env.body[last] ^= 0x01;
        let r = decrypt_envelope_body(&mut env, &key);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecryptionFailed);
    }

    /// Round-trip with all common feature bits exercised at once.
    #[test]
    fn encrypt_decrypt_with_optionals_and_signed_flag() {
        let key = key1();
        let mut nc = NonceCounter::new([0u8; 4]);
        let mut env = sample_encrypted_envelope();
        env.flags = env.flags.with(Flags::IS_SIGNED);
        env.correlation_id = Some(MessageId([0x99u8; MESSAGE_ID_LEN]));
        env.trace_id = Some(TraceId([0x77u8; 16]));
        env.ttl_ms = Some(30_000);
        env.auth = Auth::node_unsigned([0xCCu8; NODE_ID_LEN], 1);
        let plaintext = env.body.clone();
        encrypt_envelope_body(&mut env, &key, &mut nc).unwrap();
        decrypt_envelope_body(&mut env, &key).unwrap();
        assert_eq!(env.body, plaintext);
    }
}
