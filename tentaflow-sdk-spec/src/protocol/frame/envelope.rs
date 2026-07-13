// =============================================================================
// File: protocol/frame/envelope.rs — UFP/2 Envelope (§3.2)
// Purpose: top-level wire envelope. Canonical CBOR map with integer keys.
// Fields 0–13 are immutable end-to-end; 14–15 present iff IS_FRAGMENT;
// 16 is mutable hop trail (unauthenticated diagnostic metadata per §5.5).
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::address::NodeAddress;
use super::auth::Auth;
use super::channel::{Channel, Kind};
use super::flags::Flags;

/// UFP/2 protocol version byte. MUST be 2 per §11.1 — no negotiated downgrade.
pub const FRAME_PROTOCOL_VERSION: u8 = 2;

/// Length of `message_id`, `correlation_id`, `trace_id`, `session_id` bstrs.
pub const MESSAGE_ID_LEN: usize = 16;

/// Trace identifier length (16 bytes, same shape as message_id).
pub const TRACE_ID_LEN: usize = 16;

/// Ed25519 pubkey length used by NodeAddress.id and Auth.subject_id.
pub const NODE_ID_LEN: usize = 32;

/// Ed25519 signature length used by Auth.signature.
pub const SIGNATURE_LEN: usize = 64;

/// Newtype enforcing `== FRAME_PROTOCOL_VERSION` on decode. UFP/2 has a single
/// live version; receivers reject any other byte with UnknownProtocolVersion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameProtocolVersion(pub u8);

impl FrameProtocolVersion {
    pub const V2: Self = Self(FRAME_PROTOCOL_VERSION);
}

impl Default for FrameProtocolVersion {
    fn default() -> Self {
        Self::V2
    }
}

impl<C> Encode<C> for FrameProtocolVersion {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FrameProtocolVersion {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let raw = d.u8()?;
        if raw != FRAME_PROTOCOL_VERSION {
            return Err(minicbor::decode::Error::message(
                "FrameProtocolVersion: unknown protocol_version (UFP/2 requires byte 2)",
            ));
        }
        Ok(Self(raw))
    }
}

/// 16-byte ULID (time-ordered message identifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(pub [u8; MESSAGE_ID_LEN]);

impl MessageId {
    pub const ZERO: Self = Self([0u8; MESSAGE_ID_LEN]);
}

impl<C> Encode<C> for MessageId {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes(&self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for MessageId {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let bytes = d.bytes()?;
        if bytes.len() != MESSAGE_ID_LEN {
            return Err(minicbor::decode::Error::message(
                "MessageId: bstr must be exactly 16 bytes",
            ));
        }
        let mut out = [0u8; MESSAGE_ID_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }
}

/// 16-byte trace identifier (same wire shape as MessageId, distinct logical role).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub [u8; TRACE_ID_LEN]);

impl<C> Encode<C> for TraceId {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes(&self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for TraceId {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let bytes = d.bytes()?;
        if bytes.len() != TRACE_ID_LEN {
            return Err(minicbor::decode::Error::message(
                "TraceId: bstr must be exactly 16 bytes",
            ));
        }
        let mut out = [0u8; TRACE_ID_LEN];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }
}

/// Delivery priority. Routing layers MAY use this to prioritise scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Priority {
    Bulk = 0,
    Normal = 1,
    Interactive = 2,
    Control = 3,
}

impl Priority {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Bulk),
            1 => Some(Self::Normal),
            2 => Some(Self::Interactive),
            3 => Some(Self::Control),
            _ => None,
        }
    }
}

impl<C> Encode<C> for Priority {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Priority {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let raw = d.u8()?;
        Self::from_u8(raw)
            .ok_or_else(|| minicbor::decode::Error::message("Priority: unknown discriminant"))
    }
}

/// Top-level UFP/2 wire envelope.
///
/// Field layout follows §3.2 verbatim. On the sender side, optional fields
/// are encoded as omitted CBOR keys when absent (canonical Faza 6 profile).
/// On the receiver side, this type is a **data carrier only** in chunk 4c1a:
/// the derived `minicbor::Decode` impl accepts explicit CBOR `null` for
/// `Option<T>` fields as if they were omitted, which §3.2 forbids. The
/// strict structural validator that rejects explicit-null map values,
/// unknown map keys, reserved flag bits, and other §3/§11.3 invariants
/// lands in 4c1g and MUST run before this type is used to decode untrusted
/// network input. Until 4c1g lands, do NOT expose `minicbor::decode::<Envelope>`
/// as a production receive gate (see `mod.rs` scope warning).
///
/// Mandatoriness summary:
/// - 0..=9: always present (header + body).
/// - 10..=12: optional carriers (correlation_id, trace_id, ttl_ms).
/// - 13: MANDATORY (auth is required on every envelope; see §11.3).
/// - 14, 15: present iff `flags & IS_FRAGMENT = 1`.
/// - 16: present iff at least one hop has forwarded the envelope (§5.5).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Envelope {
    #[n(0)]
    pub protocol_version: FrameProtocolVersion,
    #[n(1)]
    pub message_id: MessageId,
    #[n(2)]
    pub source: NodeAddress,
    #[n(3)]
    pub destination: NodeAddress,
    #[n(4)]
    pub created_at_ms: i64,
    #[n(5)]
    pub flags: Flags,
    #[n(6)]
    pub priority: Priority,
    #[n(7)]
    pub channel: Channel,
    #[n(8)]
    pub kind: Kind,
    #[cbor(n(9), with = "minicbor::bytes")]
    pub body: Vec<u8>,
    #[n(10)]
    pub correlation_id: Option<MessageId>,
    #[n(11)]
    pub trace_id: Option<TraceId>,
    #[n(12)]
    pub ttl_ms: Option<u32>,
    #[n(13)]
    pub auth: Auth,
    #[n(14)]
    pub fragment_index: Option<u16>,
    #[n(15)]
    pub fragment_count: Option<u16>,
    #[n(16)]
    pub forwarded_via: Option<Vec<NodeAddress>>,
}

impl Envelope {
    /// Minimum envelope skeleton suitable for tests / examples. Field 0
    /// defaults to V2, all optionals absent, body empty, Anonymous auth.
    pub fn minimal(
        source: NodeAddress,
        destination: NodeAddress,
        channel: Channel,
        kind: Kind,
        priority: Priority,
        flags: Flags,
        message_id: MessageId,
        created_at_ms: i64,
    ) -> Self {
        Self {
            protocol_version: FrameProtocolVersion::V2,
            message_id,
            source,
            destination,
            created_at_ms,
            flags,
            priority,
            channel,
            kind,
            body: Vec::new(),
            correlation_id: None,
            trace_id: None,
            ttl_ms: None,
            auth: Auth::anonymous(),
            fragment_index: None,
            fragment_count: None,
            forwarded_via: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::canonical::validate_canonical;
    use crate::protocol::frame::address::{NodeAddress, NodeAddressKind};
    use crate::protocol::frame::auth::{Auth, AuthKind};
    use crate::protocol::frame::channel::channels;

    fn sample_envelope() -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = 0x01;
        mid[15] = 0xFE;
        Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::MESH,
            Kind(0x0010),
            Priority::Normal,
            Flags::NONE,
            MessageId(mid),
            1_700_000_000_000,
        )
    }

    #[test]
    fn envelope_roundtrip_minimal() {
        let env = sample_envelope();
        let mut b = Vec::new();
        minicbor::encode(&env, &mut b).unwrap();
        let d: Envelope = minicbor::decode(&b).unwrap();
        assert_eq!(d, env);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn envelope_encoded_bytes_pass_canonical_validator() {
        let env = sample_envelope();
        let mut b = Vec::new();
        minicbor::encode(&env, &mut b).unwrap();
        validate_canonical(&b).expect("envelope CBOR must be canonical");
    }

    #[test]
    fn envelope_protocol_version_v2() {
        let env = sample_envelope();
        assert_eq!(env.protocol_version.0, 2);
    }

    #[test]
    fn envelope_decode_rejects_wrong_protocol_version() {
        let mut env = sample_envelope();
        env.protocol_version = FrameProtocolVersion(1);
        let mut b = Vec::new();
        minicbor::encode(&env, &mut b).unwrap();
        let r: Result<Envelope, _> = minicbor::decode(&b);
        assert!(r.is_err(), "decoder must reject protocol_version != 2");
    }

    #[test]
    fn envelope_roundtrip_with_optionals() {
        let mut env = sample_envelope();
        env.correlation_id = Some(MessageId([0xCCu8; MESSAGE_ID_LEN]));
        env.trace_id = Some(TraceId([0x77u8; TRACE_ID_LEN]));
        env.ttl_ms = Some(5_000);
        env.auth = Auth::node_unsigned([0xAAu8; NODE_ID_LEN], 12);
        env.body = b"hello world".to_vec();
        let mut b = Vec::new();
        minicbor::encode(&env, &mut b).unwrap();
        let d: Envelope = minicbor::decode(&b).unwrap();
        assert_eq!(d, env);
    }

    #[test]
    fn envelope_roundtrip_fragmented_with_hop_trail() {
        let mut env = sample_envelope();
        env.flags = Flags::NONE.with(Flags::IS_FRAGMENT);
        env.fragment_index = Some(2);
        env.fragment_count = Some(5);
        env.forwarded_via = Some(vec![
            NodeAddress::node([0x55u8; NODE_ID_LEN]),
            NodeAddress::node([0x66u8; NODE_ID_LEN]),
        ]);
        let mut b = Vec::new();
        minicbor::encode(&env, &mut b).unwrap();
        let d: Envelope = minicbor::decode(&b).unwrap();
        assert_eq!(d, env);
    }

    #[test]
    fn priority_rejects_unknown_discriminant() {
        let bad = [0x04u8];
        let r: Result<Priority, _> = minicbor::decode(&bad);
        assert!(r.is_err());
    }

    #[test]
    fn message_id_rejects_wrong_length_bytes() {
        // bstr of 8 bytes — not 16.
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.bytes(&[0u8; 8]).unwrap();
        let r: Result<MessageId, _> = minicbor::decode(&buf);
        assert!(r.is_err());
    }

    /// Probe minicbor's behaviour when an optional field carries explicit
    /// CBOR `null` instead of being omitted. UFP/2 spec §3.2 forbids explicit
    /// null, but strict enforcement is part of the 4c1g auth/structural
    /// validator. This test documents current Option<T> decode semantics so
    /// 4c1g knows whether it needs to add an extra rejection pass.
    #[test]
    fn explicit_null_for_optional_field_documented_behaviour() {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.bytes(&[0u8; MESSAGE_ID_LEN]).unwrap(); // valid 16-byte bstr — baseline
        let ok: Result<MessageId, _> = minicbor::decode(&buf);
        assert!(ok.is_ok());

        let mut buf2 = Vec::new();
        let mut e2 = minicbor::Encoder::new(&mut buf2);
        e2.null().unwrap();
        let null_into_messageid: Result<MessageId, _> = minicbor::decode(&buf2);
        assert!(
            null_into_messageid.is_err(),
            "MessageId must reject explicit CBOR null (it's a bstr, not Option)"
        );

        let mut buf3 = Vec::new();
        let mut e3 = minicbor::Encoder::new(&mut buf3);
        e3.null().unwrap();
        let null_into_opt: Result<Option<MessageId>, _> = minicbor::decode(&buf3);
        if null_into_opt.is_ok() {
            eprintln!(
                "note: minicbor Option<T> accepts explicit null as None. \
                 4c1g must add a structural pass to reject explicit-null map values."
            );
        }
    }

    #[test]
    fn auth_kind_anonymous_present_in_envelope() {
        let env = sample_envelope();
        assert_eq!(env.auth.kind, AuthKind::Anonymous);
    }

    #[test]
    fn source_and_destination_carry_distinct_node_ids() {
        let env = sample_envelope();
        assert_eq!(env.source.kind, NodeAddressKind::Node);
        assert_eq!(env.destination.kind, NodeAddressKind::Node);
        assert_ne!(env.source.id, env.destination.id);
    }
}
