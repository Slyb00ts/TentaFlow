// =============================================================================
// File: protocol/frame/fragment.rs — UFP/2 fragmentation (§10)
// Purpose: split logical envelopes whose body exceeds the transport MTU into
// per-fragment envelopes (sender side) and reassemble incoming fragments
// into a logical envelope (receiver side).
//
// Sender (§10.0): compress (optional) → encrypt (optional) → split encrypted
// blob into N fragments → per-fragment sign (optional). Each fragment is a
// complete envelope with its own canonical-CBOR signature scope; receivers
// can independently verify each fragment before buffering.
//
// Receiver (§10.2): per-(source.id, message_id) reassembly buffer with
// write-once fragment slots; check dedup → verify signature → buffer →
// commit dedup runs under a per-key mutex so duplicate or conflicting
// fragments cannot race. Reassembled logical body then flows through the
// non-fragmented receive pipeline (4c1d) to decrypt + decompress.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §10.0–§10.4 + §9 (replay).
// =============================================================================

use std::collections::HashMap;
use std::sync::Mutex;

use ed25519_dalek::SigningKey;

use super::aead::{compute_aad, encrypt_body, AeadKey, NonceCounter, AEAD_NONCE_LEN, AEAD_TAG_LEN};
use super::compress::compress_body;
use super::envelope::{Envelope, MessageId};
use super::error::{FrameError, FrameErrorCode};
use super::flags::Flags;
use super::sign::sign_envelope;

/// Hard ceiling on the number of fragments per logical message (§10.3).
/// `fragment_count` is u16 so any value above this is unrepresentable.
pub const MAX_FRAGMENT_COUNT: u16 = u16::MAX;

/// Maximum bytes buffered per reassembly key (§10.3). Matches the default
/// lz4 decompression cap in `compress.rs`.
pub const MAX_REASSEMBLY_BYTES: usize = 64 * 1024 * 1024;

/// Default reassembly timeout: drop buffer 60 s after the first fragment
/// arrives if `fragment_count` fragments have not all landed (§10.3).
pub const REASSEMBLY_TIMEOUT_MS: u64 = 60_000;

/// Crypto required to fragment a logical envelope. Mirrors the
/// non-fragmented `SendCrypto` but enforces per-fragment signature semantics.
pub struct FragmentSendCrypto<'a> {
    pub signing_key: Option<&'a SigningKey>,
    pub aead_key: Option<&'a AeadKey>,
    pub nonce_counter: Option<&'a mut NonceCounter>,
}

/// Split a logical envelope into wire fragments per §10.0.
///
/// Pre-conditions on the input envelope:
/// - `flags & IS_FRAGMENT` MUST be 0 (caller passes the LOGICAL envelope;
///   this function sets IS_FRAGMENT on each output fragment).
/// - `fragment_index` and `fragment_count` MUST be `None`.
/// - `auth.signature` is ignored on input (per-fragment signatures replace
///   any logical-level signature).
/// - `flags` reflect the desired set of features (IS_COMPRESSED, IS_ENCRYPTED,
///   IS_SIGNED) for the whole logical message; they are propagated to every
///   fragment with IS_FRAGMENT added (and IS_LAST_FRAGMENT on the last).
///
/// `fragment_body_size` controls how many post-encryption bytes each
/// fragment's body carries. It MUST be ≥ 1. Typical values: 32 KiB for QUIC
/// stream chunks, 64 KiB for WebTransport datagrams.
pub fn split_envelope_into_fragments(
    logical: &Envelope,
    fragment_body_size: usize,
    crypto: FragmentSendCrypto<'_>,
) -> Result<Vec<Envelope>, FrameError> {
    if fragment_body_size == 0 {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "split_envelope_into_fragments: fragment_body_size MUST be >= 1",
        ));
    }
    if logical.flags.contains(Flags::IS_FRAGMENT) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "split_envelope_into_fragments: input envelope MUST NOT already have IS_FRAGMENT set",
        )
        .with_path("envelope.flags"));
    }
    if logical.fragment_index.is_some() || logical.fragment_count.is_some() {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "split_envelope_into_fragments: input envelope MUST NOT carry fragment_index/count",
        ));
    }

    let mut body = logical.body.clone();
    if logical.flags.contains(Flags::IS_COMPRESSED) {
        body = compress_body(&body)?;
    }
    if logical.flags.contains(Flags::IS_ENCRYPTED) {
        let key = crypto.aead_key.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "split_envelope_into_fragments: IS_ENCRYPTED set but no AEAD key provided",
            )
        })?;
        let counter = crypto.nonce_counter.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "split_envelope_into_fragments: IS_ENCRYPTED set but no NonceCounter provided",
            )
        })?;
        let nonce = counter.next_nonce()?;
        // §7.2: fragmented AAD includes IS_FRAGMENT in flags so the AEAD tag
        // is bound to the fact this was a fragmented message. We construct a
        // throwaway envelope with IS_FRAGMENT set (and IS_LAST_FRAGMENT clear,
        // which compute_aad would mask anyway) to compute the AAD that every
        // fragment will share. Receivers rebuild the same AAD from any
        // arriving fragment because compute_aad masks IS_LAST_FRAGMENT when
        // IS_FRAGMENT is set.
        let mut aad_view = logical.clone();
        aad_view.flags = aad_view
            .flags
            .with(Flags::IS_FRAGMENT)
            .without(Flags::IS_LAST_FRAGMENT);
        let aad = compute_aad(&aad_view)?;
        body = encrypt_body(&body, key, &nonce, &aad)?;
    }

    let total_bytes = body.len();
    let fragment_count_usize = if total_bytes == 0 {
        1
    } else {
        (total_bytes + fragment_body_size - 1) / fragment_body_size
    };
    if fragment_count_usize > MAX_FRAGMENT_COUNT as usize {
        return Err(FrameError::new(
            FrameErrorCode::FragmentAssemblyError,
            format!(
                "split_envelope_into_fragments: would emit {} fragments, exceeds MAX_FRAGMENT_COUNT={}",
                fragment_count_usize, MAX_FRAGMENT_COUNT
            ),
        ));
    }
    let fragment_count = fragment_count_usize as u16;

    let mut fragments = Vec::with_capacity(fragment_count_usize);
    for index_usize in 0..fragment_count_usize {
        let index = index_usize as u16;
        let start = index_usize * fragment_body_size;
        let end = (start + fragment_body_size).min(total_bytes);
        let chunk = body[start..end].to_vec();

        let is_last = index == fragment_count - 1;
        let mut frag_flags = logical.flags.with(Flags::IS_FRAGMENT);
        if is_last {
            frag_flags = frag_flags.with(Flags::IS_LAST_FRAGMENT);
        } else {
            frag_flags = frag_flags.without(Flags::IS_LAST_FRAGMENT);
        }

        let mut fragment = Envelope {
            protocol_version: logical.protocol_version,
            message_id: logical.message_id,
            source: logical.source.clone(),
            destination: logical.destination.clone(),
            created_at_ms: logical.created_at_ms,
            flags: frag_flags,
            priority: logical.priority,
            channel: logical.channel,
            kind: logical.kind,
            body: chunk,
            correlation_id: logical.correlation_id,
            trace_id: logical.trace_id,
            ttl_ms: logical.ttl_ms,
            auth: logical.auth.clone(),
            fragment_index: Some(index),
            fragment_count: Some(fragment_count),
            forwarded_via: None,
        };
        // Drop any inherited signature — sender will re-sign each fragment.
        fragment.auth.signature = None;
        if logical.flags.contains(Flags::IS_SIGNED) {
            let key = crypto.signing_key.ok_or_else(|| {
                FrameError::new(
                    FrameErrorCode::BodyValidationFailed,
                    "split_envelope_into_fragments: IS_SIGNED set but no signing key provided",
                )
            })?;
            sign_envelope(&mut fragment, key)?;
        }
        fragments.push(fragment);
    }
    Ok(fragments)
}

/// Outcome of inserting one fragment into the reassembly buffer.
#[derive(Debug)]
pub enum AcceptOutcome {
    /// More fragments needed; reassembly continues.
    Buffered,
    /// Duplicate of an already-buffered fragment with byte-identical body —
    /// silently idempotent (e.g. transport retransmit).
    Duplicate,
    /// All fragments now present; reassembly completed. Caller MUST now run
    /// `finalize_reassembled_envelope` on the returned envelope to AEAD-
    /// decrypt and lz4-decompress the body and strip the transport-level
    /// IS_FRAGMENT / IS_SIGNED markers. The standalone non-fragmented
    /// receive pipeline (`receive_envelope_pipeline`) rejects IS_FRAGMENT
    /// and is NOT the right function for reassembled envelopes.
    Reassembled(Envelope),
}

/// Per-key reassembly state. Created on first fragment arrival, evicted on
/// completion or timeout.
struct ReassemblyBuffer {
    fragment_count: u16,
    slots: Vec<Option<Vec<u8>>>,
    filled: u16,
    total_bytes: usize,
    first_seen_ms: u64,
    /// Cloned from the first accepted fragment. Used both as the template
    /// for the reassembled logical envelope AND as the canonical record
    /// against which later fragments' common immutable fields are checked
    /// (§10.1 common-field consistency).
    template: Envelope,
}

/// True iff `b` carries the same immutable header fields as `a`. Excludes
/// fields that legitimately differ per fragment: `body` (chunk), `flags`
/// `IS_LAST_FRAGMENT` bit, `fragment_index`, `auth.signature`, and
/// `forwarded_via` (mutable hop trail). Enforces §10.1.
fn fragments_share_common_header(a: &Envelope, b: &Envelope) -> bool {
    if a.protocol_version != b.protocol_version
        || a.message_id != b.message_id
        || a.source != b.source
        || a.destination != b.destination
        || a.created_at_ms != b.created_at_ms
        || a.priority != b.priority
        || a.channel != b.channel
        || a.kind != b.kind
        || a.correlation_id != b.correlation_id
        || a.trace_id != b.trace_id
        || a.ttl_ms != b.ttl_ms
        || a.fragment_count != b.fragment_count
    {
        return false;
    }
    let af = a.flags.0 & !Flags::IS_LAST_FRAGMENT;
    let bf = b.flags.0 & !Flags::IS_LAST_FRAGMENT;
    if af != bf {
        return false;
    }
    // auth: every field except `signature` must match (signatures legitimately
    // differ per fragment per §10.1).
    if a.auth.kind != b.auth.kind
        || a.auth.subject_id != b.auth.subject_id
        || a.auth.epoch != b.auth.epoch
        || a.auth.session_id != b.auth.session_id
    {
        return false;
    }
    true
}

/// Reassembly manager. Owns per-key buffers behind a per-key mutex chain so
/// concurrent fragment arrivals for one logical message serialise correctly
/// (§10.2 atomicity requirement).
pub struct ReassemblyManager {
    buffers: Mutex<HashMap<(Vec<u8>, MessageId), Mutex<ReassemblyBuffer>>>,
    max_reassembly_bytes: usize,
    reassembly_timeout_ms: u64,
}

impl Default for ReassemblyManager {
    fn default() -> Self {
        Self::new(MAX_REASSEMBLY_BYTES, REASSEMBLY_TIMEOUT_MS)
    }
}

impl ReassemblyManager {
    pub fn new(max_reassembly_bytes: usize, reassembly_timeout_ms: u64) -> Self {
        Self {
            buffers: Mutex::new(HashMap::new()),
            max_reassembly_bytes,
            reassembly_timeout_ms,
        }
    }

    /// Accept one incoming fragment. `now_ms` is the receiver's current
    /// monotonic timestamp (used only for reassembly timeout — replay
    /// protection lives in 4c1f and runs BEFORE this function).
    ///
    /// The fragment MUST already have passed signature verification at the
    /// transport layer (4c1d `receive_envelope_pipeline` is NOT suitable
    /// because it rejects IS_FRAGMENT — use `sign::verify_envelope` directly
    /// for each fragment before calling).
    pub fn accept_fragment(
        &self,
        fragment: Envelope,
        now_ms: u64,
    ) -> Result<AcceptOutcome, FrameError> {
        if !fragment.flags.contains(Flags::IS_FRAGMENT) {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: envelope does not have IS_FRAGMENT set",
            ));
        }
        let index = fragment.fragment_index.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: fragment_index missing on IS_FRAGMENT envelope",
            )
        })?;
        let count = fragment.fragment_count.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: fragment_count missing on IS_FRAGMENT envelope",
            )
        })?;
        if count == 0 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: fragment_count MUST be > 0",
            ));
        }
        if index >= count {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                format!(
                    "accept_fragment: fragment_index {} out of range (count {})",
                    index, count
                ),
            ));
        }
        let is_last = fragment.flags.contains(Flags::IS_LAST_FRAGMENT);
        if is_last && index != count - 1 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: IS_LAST_FRAGMENT set with fragment_index != fragment_count - 1",
            ));
        }
        if !is_last && index == count - 1 {
            return Err(FrameError::new(
                FrameErrorCode::BodyValidationFailed,
                "accept_fragment: last fragment_index without IS_LAST_FRAGMENT set",
            ));
        }

        let key = (fragment.source.id.to_vec(), fragment.message_id);

        // We need to atomically (a) sweep expired buffers, (b) ensure a buffer
        // exists for this key, and (c) take the per-key inner lock. The inner
        // lock guard borrows from the outer HashMap, so we must hold the outer
        // lock while we acquire and use the inner lock. The function holds the
        // outer lock for the whole accept call — concurrent fragments for
        // DIFFERENT keys serialise behind it briefly, which is acceptable for
        // the small critical sections involved.
        let mut outer = self
            .buffers
            .lock()
            .expect("reassembly outer mutex poisoned");

        // Opportunistic timeout sweep — drop any buffer whose first fragment
        // arrived more than `reassembly_timeout_ms` ago.
        let timeout = self.reassembly_timeout_ms;
        outer.retain(|_k, buf_mutex| {
            let first = buf_mutex.lock().map(|b| b.first_seen_ms).unwrap_or(now_ms);
            now_ms.saturating_sub(first) <= timeout
        });

        if !outer.contains_key(&key) {
            let buf = ReassemblyBuffer {
                fragment_count: count,
                slots: vec![None; count as usize],
                filled: 0,
                total_bytes: 0,
                first_seen_ms: now_ms,
                template: fragment.clone(),
            };
            outer.insert(key.clone(), Mutex::new(buf));
        }

        let mut inner = outer
            .get(&key)
            .expect("inserted above")
            .lock()
            .expect("reassembly inner mutex poisoned");

        if inner.fragment_count != count {
            let buffered = inner.fragment_count;
            drop(inner);
            outer.remove(&key);
            return Err(FrameError::new(
                FrameErrorCode::FragmentAssemblyError,
                format!(
                    "accept_fragment: fragment_count {} differs from buffered {} (§10.1 — buffer evicted)",
                    count, buffered
                ),
            ));
        }

        if !fragments_share_common_header(&inner.template, &fragment) {
            drop(inner);
            outer.remove(&key);
            return Err(FrameError::new(
                FrameErrorCode::FragmentAssemblyError,
                "accept_fragment: fragment immutable header fields differ from earlier fragment (§10.1)",
            ));
        }

        // Write-once slot semantics. Check duplicate/conflict BEFORE the
        // byte-cap check so a byte-identical retransmit succeeds even when
        // the buffer is at its size ceiling (the retransmit wouldn't add
        // bytes, but a cap check counting it would still reject).
        let slot_idx = index as usize;
        if let Some(existing) = &inner.slots[slot_idx] {
            if existing == &fragment.body {
                return Ok(AcceptOutcome::Duplicate);
            } else {
                drop(inner);
                outer.remove(&key);
                return Err(FrameError::new(
                    FrameErrorCode::FragmentAssemblyError,
                    format!(
                        "accept_fragment: conflicting fragment at index {} (bytes differ from earlier arrival)",
                        index
                    ),
                ));
            }
        }

        let chunk_len = fragment.body.len();
        if inner.total_bytes.saturating_add(chunk_len) > self.max_reassembly_bytes {
            return Err(FrameError::new(
                FrameErrorCode::FragmentAssemblyError,
                format!(
                    "accept_fragment: reassembly buffer would exceed {} bytes",
                    self.max_reassembly_bytes
                ),
            ));
        }

        inner.slots[slot_idx] = Some(fragment.body.clone());
        inner.filled += 1;
        inner.total_bytes = inner.total_bytes.saturating_add(chunk_len);

        if inner.filled < inner.fragment_count {
            return Ok(AcceptOutcome::Buffered);
        }

        // All slots filled → assemble logical body.
        let mut logical_body = Vec::with_capacity(inner.total_bytes);
        for slot in inner.slots.iter() {
            // Safety: filled == fragment_count means every slot is Some.
            logical_body.extend_from_slice(slot.as_ref().expect("complete reassembly"));
        }
        let mut logical = inner.template.clone();
        drop(inner);
        outer.remove(&key);

        // The reassembled envelope is shaped for the post-reassembly
        // decrypt+decompress stage:
        // - IS_FRAGMENT stays SET so compute_aad rebuilds the fragmented AAD
        //   the sender used (with IS_LAST_FRAGMENT masked).
        // - IS_SIGNED stays SET so compute_aad sees the same flags the sender
        //   saw when building AAD; `finalize_reassembled_envelope` strips both
        //   IS_FRAGMENT and IS_SIGNED after decrypt+decompress completes.
        // - IS_LAST_FRAGMENT cleared (already masked by AAD construction;
        //   keeping it set would advertise a stale per-fragment marker).
        // - fragment_index/count cleared (no longer meaningful).
        // - auth.signature cleared (per-fragment signatures already verified;
        //   no logical-level signature exists).
        logical.flags = logical.flags.without(Flags::IS_LAST_FRAGMENT);
        logical.fragment_index = None;
        logical.fragment_count = None;
        logical.body = logical_body;
        logical.auth.signature = None;

        Ok(AcceptOutcome::Reassembled(logical))
    }

    /// Number of live reassembly buffers (test/diagnostic helper).
    pub fn buffer_count(&self) -> usize {
        self.buffers.lock().map(|b| b.len()).unwrap_or(0)
    }
}

/// Required body capacity of the AEAD nonce + tag overhead, useful for
/// callers sizing per-channel fragment ceilings.
pub const AEAD_OVERHEAD_BYTES: usize = AEAD_NONCE_LEN + AEAD_TAG_LEN;

/// Post-reassembly finishing pipeline. Takes a reassembled envelope from
/// `ReassemblyManager::accept_fragment` (which preserves `IS_FRAGMENT` so
/// AAD reconstruction matches the sender) and runs decrypt + decompress
/// in the correct order per §7.1 / §10.0, then strips `IS_FRAGMENT` to
/// produce the final logical envelope for application dispatch.
///
/// `aead_key` MUST be the same per-pair session key the sender used; if
/// `IS_ENCRYPTED` is not set on the reassembled envelope, pass `None`.
pub fn finalize_reassembled_envelope(
    envelope: &mut Envelope,
    aead_key: Option<&AeadKey>,
) -> Result<(), FrameError> {
    if !envelope.flags.contains(Flags::IS_FRAGMENT) {
        return Err(FrameError::new(
            FrameErrorCode::BodyValidationFailed,
            "finalize_reassembled_envelope: envelope is not marked IS_FRAGMENT (was it reassembled?)",
        ));
    }

    if envelope.flags.contains(Flags::IS_ENCRYPTED) {
        let key = aead_key.ok_or_else(|| {
            FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "finalize_reassembled_envelope: IS_ENCRYPTED set but no AEAD key provided",
            )
        })?;
        let aad = compute_aad(envelope)?;
        let plaintext = super::aead::decrypt_body(&envelope.body, key, &aad)?;
        envelope.body = plaintext;
    }

    if envelope.flags.contains(Flags::IS_COMPRESSED) {
        let plain = super::compress::decompress_body(&envelope.body)?;
        envelope.body = plain;
    }

    // Strip both transport-level markers so the envelope handed to the
    // application looks like any other non-fragmented logical message:
    // - IS_FRAGMENT was preserved across reassembly only to make AAD
    //   reconstruction match the sender; now meaningless.
    // - IS_SIGNED was preserved for the same AAD reason; per-fragment
    //   signatures already validated the message authenticity before
    //   accept_fragment ran.
    envelope.flags = envelope
        .flags
        .without(Flags::IS_FRAGMENT)
        .without(Flags::IS_SIGNED);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::address::NodeAddress;
    use crate::protocol::frame::auth::Auth;
    use crate::protocol::frame::channel::{channels, Kind};
    use crate::protocol::frame::compress::decompress_body;
    use crate::protocol::frame::envelope::{MessageId, Priority, MESSAGE_ID_LEN, NODE_ID_LEN};
    use crate::protocol::frame::sign::{public_key_bytes, verify_envelope};
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    fn fresh_signing_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    fn sample_logical(payload: Vec<u8>) -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 0xF1;
        mid[15] = 0xA6;
        let mut env = Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::STREAM,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE,
            MessageId(mid),
            1_700_000_000_000,
        );
        env.body = payload;
        env
    }

    #[test]
    fn split_then_reassemble_plain_roundtrip() {
        let payload = b"plain payload across many fragments ".repeat(64);
        let logical = sample_logical(payload.clone());
        let frags = split_envelope_into_fragments(
            &logical,
            64,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        assert!(frags.len() > 1);
        for f in &frags {
            assert!(f.flags.contains(Flags::IS_FRAGMENT));
            assert_eq!(f.fragment_count.unwrap() as usize, frags.len());
        }
        let last = frags.last().unwrap();
        assert!(last.flags.contains(Flags::IS_LAST_FRAGMENT));

        let mgr = ReassemblyManager::default();
        let total = frags.len();
        for (i, f) in frags.into_iter().enumerate() {
            let r = mgr.accept_fragment(f, 1_000).unwrap();
            if i + 1 < total {
                matches!(r, AcceptOutcome::Buffered);
            } else if let AcceptOutcome::Reassembled(mut logical_back) = r {
                assert!(logical_back.flags.contains(Flags::IS_FRAGMENT));
                assert!(logical_back.fragment_index.is_none());
                assert!(logical_back.fragment_count.is_none());
                finalize_reassembled_envelope(&mut logical_back, None).unwrap();
                assert_eq!(logical_back.body, payload);
                assert!(!logical_back.flags.contains(Flags::IS_FRAGMENT));
            } else {
                panic!("last fragment must reassemble");
            }
        }
        assert_eq!(mgr.buffer_count(), 0);
    }

    #[test]
    fn split_then_reassemble_signed_roundtrip() {
        let key = fresh_signing_key();
        let pubkey = public_key_bytes(&key);
        let payload = b"signed payload across fragments ".repeat(32);
        let mut logical = sample_logical(payload.clone());
        logical.source = NodeAddress::node(pubkey);
        logical.flags = logical.flags.with(Flags::IS_SIGNED);
        logical.auth = Auth::node_unsigned(pubkey, 9);
        let frags = split_envelope_into_fragments(
            &logical,
            48,
            FragmentSendCrypto {
                signing_key: Some(&key),
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        for f in &frags {
            verify_envelope(f).expect("every fragment must verify independently");
        }
        let mgr = ReassemblyManager::default();
        let total = frags.len();
        let mut reassembled: Option<Envelope> = None;
        for (i, f) in frags.into_iter().enumerate() {
            let r = mgr.accept_fragment(f, 0).unwrap();
            if i + 1 == total {
                if let AcceptOutcome::Reassembled(env) = r {
                    reassembled = Some(env);
                }
            }
        }
        let mut env = reassembled.expect("reassembly completes");
        finalize_reassembled_envelope(&mut env, None).unwrap();
        assert_eq!(env.body, payload);
        assert!(!env.flags.contains(Flags::IS_FRAGMENT));
        assert!(!env.flags.contains(Flags::IS_SIGNED));
    }

    #[test]
    fn split_then_reassemble_encrypted_roundtrip_via_finalize() {
        let aead = AeadKey::from_bytes([0xC1u8; 32]);
        let mut nc = NonceCounter::new([0x77u8; 4]);
        let payload = b"encrypted payload across fragments ".repeat(48);
        let mut logical = sample_logical(payload.clone());
        logical.flags = logical.flags.with(Flags::IS_ENCRYPTED);
        let frags = split_envelope_into_fragments(
            &logical,
            64,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: Some(&aead),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();
        assert!(frags.len() > 1);
        let mgr = ReassemblyManager::default();
        let total = frags.len();
        let mut reassembled: Option<Envelope> = None;
        for (i, f) in frags.into_iter().enumerate() {
            let r = mgr.accept_fragment(f, 0).unwrap();
            if i + 1 == total {
                if let AcceptOutcome::Reassembled(env) = r {
                    reassembled = Some(env);
                }
            }
        }
        let mut env = reassembled.unwrap();
        finalize_reassembled_envelope(&mut env, Some(&aead)).unwrap();
        assert_eq!(env.body, payload);
        assert!(!env.flags.contains(Flags::IS_FRAGMENT));
    }

    #[test]
    fn split_then_reassemble_compressed_roundtrip_via_finalize() {
        let payload = vec![0xCCu8; 16 * 1024];
        let mut logical = sample_logical(payload.clone());
        logical.flags = logical.flags.with(Flags::IS_COMPRESSED);
        let frags = split_envelope_into_fragments(
            &logical,
            512,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let total = frags.len();
        let mut reassembled: Option<Envelope> = None;
        for (i, f) in frags.into_iter().enumerate() {
            let r = mgr.accept_fragment(f, 0).unwrap();
            if i + 1 == total {
                if let AcceptOutcome::Reassembled(env) = r {
                    reassembled = Some(env);
                }
            }
        }
        let mut env = reassembled.unwrap();
        finalize_reassembled_envelope(&mut env, None).unwrap();
        assert_eq!(env.body, payload);
        assert!(!env.flags.contains(Flags::IS_FRAGMENT));
    }

    #[test]
    fn split_then_reassemble_compressed_encrypted_signed_through_finalize() {
        let signing_key = fresh_signing_key();
        let pubkey = public_key_bytes(&signing_key);
        let aead = AeadKey::from_bytes([0x55u8; 32]);
        let mut nc = NonceCounter::new([0xBBu8; 4]);
        let payload = vec![0xABu8; 12 * 1024];
        let mut logical = sample_logical(payload.clone());
        logical.source = NodeAddress::node(pubkey);
        logical.flags = logical
            .flags
            .with(Flags::IS_COMPRESSED)
            .with(Flags::IS_ENCRYPTED)
            .with(Flags::IS_SIGNED);
        logical.auth = Auth::node_unsigned(pubkey, 5);
        let frags = split_envelope_into_fragments(
            &logical,
            256,
            FragmentSendCrypto {
                signing_key: Some(&signing_key),
                aead_key: Some(&aead),
                nonce_counter: Some(&mut nc),
            },
        )
        .unwrap();
        for f in &frags {
            verify_envelope(f).expect("each fragment carries its own valid signature");
        }
        let mgr = ReassemblyManager::default();
        let mut reassembled: Option<Envelope> = None;
        let total = frags.len();
        for (i, f) in frags.into_iter().enumerate() {
            let r = mgr.accept_fragment(f, 0).unwrap();
            if i + 1 == total {
                if let AcceptOutcome::Reassembled(env) = r {
                    reassembled = Some(env);
                }
            }
        }
        let mut env = reassembled.unwrap();
        finalize_reassembled_envelope(&mut env, Some(&aead)).unwrap();
        assert_eq!(env.body, payload);
    }

    #[test]
    fn out_of_order_reassembly_succeeds() {
        let payload = b"reorder me ".repeat(64);
        let logical = sample_logical(payload.clone());
        let frags = split_envelope_into_fragments(
            &logical,
            48,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        // Deliver in REVERSE order: last fragment first, then descending.
        let total = frags.len();
        let mut reassembled: Option<Envelope> = None;
        for (i, f) in frags.into_iter().rev().enumerate() {
            let r = mgr.accept_fragment(f, 0).unwrap();
            if i + 1 == total {
                if let AcceptOutcome::Reassembled(env) = r {
                    reassembled = Some(env);
                }
            }
        }
        let mut env = reassembled.expect("reassembly completes regardless of arrival order");
        finalize_reassembled_envelope(&mut env, None).unwrap();
        assert_eq!(env.body, payload);
    }

    #[test]
    fn reassembly_rejects_common_field_mismatch_destination() {
        let payload = b"abcdefgh".repeat(16);
        let logical = sample_logical(payload);
        let mut frags = split_envelope_into_fragments(
            &logical,
            16,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        frags[1].destination = NodeAddress::node([0xFFu8; NODE_ID_LEN]);
        let r = mgr.accept_fragment(frags[1].clone(), 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
        assert_eq!(mgr.buffer_count(), 0, "mismatch evicts buffer");
    }

    #[test]
    fn fragment_count_mismatch_evicts_buffer() {
        let payload = b"some payload".to_vec();
        let logical = sample_logical(payload);
        let mut frags = split_envelope_into_fragments(
            &logical,
            4,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        assert_eq!(mgr.buffer_count(), 1);
        frags[1].fragment_count = Some(99);
        let r = mgr.accept_fragment(frags[1].clone(), 0);
        assert!(r.is_err());
        assert_eq!(
            mgr.buffer_count(),
            0,
            "fragment_count mismatch MUST evict the buffer to prevent first-fragment poisoning"
        );
    }

    #[test]
    fn duplicate_fragment_accepted_when_buffer_at_byte_cap() {
        // Construct a 3-fragment message of 96 bytes total. Cap = 64 lets
        // only the first 2 fragments fit; a third NEW fragment would exceed
        // the cap. But a duplicate retransmit of an already-buffered slot
        // MUST be reported as `Duplicate` rather than rejected by the cap,
        // because the byte total wouldn't actually change.
        let payload = vec![0u8; 96];
        let logical = sample_logical(payload);
        let frags = split_envelope_into_fragments(
            &logical,
            32,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        assert_eq!(frags.len(), 3);
        let mgr = ReassemblyManager::new(64, REASSEMBLY_TIMEOUT_MS);
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        let _ = mgr.accept_fragment(frags[1].clone(), 0).unwrap();
        // Buffer now holds 64 bytes (slots 0 and 1). Retransmit of slot 0
        // hits the duplicate check FIRST, returns Duplicate without ever
        // running the byte-cap check.
        let r = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        assert!(matches!(r, AcceptOutcome::Duplicate));
        // Confirm a genuinely new third fragment would be rejected by the cap.
        let r2 = mgr.accept_fragment(frags[2].clone(), 0);
        assert!(r2.is_err());
        assert_eq!(r2.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
    }

    #[test]
    fn reassembly_rejects_common_field_mismatch_channel() {
        let payload = b"abcdefgh".repeat(16);
        let logical = sample_logical(payload);
        let mut frags = split_envelope_into_fragments(
            &logical,
            16,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        frags[1].channel = channels::DOMAIN;
        let r = mgr.accept_fragment(frags[1].clone(), 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
    }

    #[test]
    fn split_rejects_envelope_already_fragmented() {
        let mut logical = sample_logical(b"x".to_vec());
        logical.flags = logical.flags.with(Flags::IS_FRAGMENT);
        let r = split_envelope_into_fragments(
            &logical,
            64,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        );
        assert!(r.is_err());
    }

    #[test]
    fn split_rejects_zero_fragment_size() {
        let logical = sample_logical(b"hello".to_vec());
        let r = split_envelope_into_fragments(
            &logical,
            0,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        );
        assert!(r.is_err());
    }

    #[test]
    fn reassembly_duplicate_fragment_is_idempotent() {
        let payload = b"three fragments worth ".repeat(8);
        let logical = sample_logical(payload);
        let frags = split_envelope_into_fragments(
            &logical,
            32,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let first = frags[0].clone();
        let _ = mgr.accept_fragment(first.clone(), 0).unwrap();
        let r = mgr.accept_fragment(first, 0).unwrap();
        matches!(r, AcceptOutcome::Duplicate);
    }

    #[test]
    fn reassembly_conflicting_fragment_evicts_buffer() {
        let payload = b"AAAAAAAA BBBBBBBB CCCCCCCC".to_vec();
        let logical = sample_logical(payload);
        let mut frags = split_envelope_into_fragments(
            &logical,
            8,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        // Tamper fragment 0 to a different body — conflict.
        frags[0].body = b"XXXXXXXX".to_vec();
        let r = mgr.accept_fragment(frags[0].clone(), 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
        assert_eq!(mgr.buffer_count(), 0, "conflict evicts buffer");
    }

    #[test]
    fn reassembly_rejects_count_zero() {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 1;
        let mut env = Envelope::minimal(
            NodeAddress::node([1u8; NODE_ID_LEN]),
            NodeAddress::node([2u8; NODE_ID_LEN]),
            channels::STREAM,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE.with(Flags::IS_FRAGMENT),
            MessageId(mid),
            0,
        );
        env.fragment_index = Some(0);
        env.fragment_count = Some(0);
        let mgr = ReassemblyManager::default();
        let r = mgr.accept_fragment(env, 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn reassembly_rejects_index_out_of_range() {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 1;
        let mut env = Envelope::minimal(
            NodeAddress::node([1u8; NODE_ID_LEN]),
            NodeAddress::node([2u8; NODE_ID_LEN]),
            channels::STREAM,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE
                .with(Flags::IS_FRAGMENT)
                .with(Flags::IS_LAST_FRAGMENT),
            MessageId(mid),
            0,
        );
        env.fragment_index = Some(5);
        env.fragment_count = Some(3);
        let mgr = ReassemblyManager::default();
        let r = mgr.accept_fragment(env, 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::BodyValidationFailed);
    }

    #[test]
    fn reassembly_rejects_inconsistent_count_across_fragments() {
        let payload = b"some payload".to_vec();
        let logical = sample_logical(payload);
        let mut frags = split_envelope_into_fragments(
            &logical,
            4,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::default();
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        // Mutate count on a later fragment.
        frags[1].fragment_count = Some(99);
        let r = mgr.accept_fragment(frags[1].clone(), 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
    }

    #[test]
    fn reassembly_timeout_evicts_stale_buffers() {
        let payload = b"abcdef".repeat(8);
        let logical = sample_logical(payload);
        let frags = split_envelope_into_fragments(
            &logical,
            8,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::new(MAX_REASSEMBLY_BYTES, 10);
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        assert_eq!(mgr.buffer_count(), 1);
        // Submit another fragment far in the future — sweep evicts buffer first.
        let _ = mgr.accept_fragment(frags[1].clone(), 1_000).unwrap();
        assert_eq!(mgr.buffer_count(), 1, "new buffer created after eviction");
    }

    #[test]
    fn reassembly_buffer_byte_cap_blocks_oversize() {
        let payload = vec![0u8; 1024];
        let logical = sample_logical(payload);
        let frags = split_envelope_into_fragments(
            &logical,
            64,
            FragmentSendCrypto {
                signing_key: None,
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let mgr = ReassemblyManager::new(100, REASSEMBLY_TIMEOUT_MS);
        let _ = mgr.accept_fragment(frags[0].clone(), 0).unwrap();
        let r = mgr.accept_fragment(frags[1].clone(), 0);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::FragmentAssemblyError);
    }

    #[test]
    fn fragment_signatures_differ_across_fragments() {
        let key = fresh_signing_key();
        let pubkey = public_key_bytes(&key);
        let payload = b"yet more signed fragment bytes ".repeat(8);
        let mut logical = sample_logical(payload);
        logical.source = NodeAddress::node(pubkey);
        logical.flags = logical.flags.with(Flags::IS_SIGNED);
        logical.auth = Auth::node_unsigned(pubkey, 1);
        let frags = split_envelope_into_fragments(
            &logical,
            16,
            FragmentSendCrypto {
                signing_key: Some(&key),
                aead_key: None,
                nonce_counter: None,
            },
        )
        .unwrap();
        let sig_a = frags[0].auth.signature.unwrap();
        let sig_b = frags[1].auth.signature.unwrap();
        assert_ne!(sig_a, sig_b, "per-fragment signatures MUST differ");
    }
}
