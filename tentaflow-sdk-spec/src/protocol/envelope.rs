// =============================================================================
// File: protocol/envelope.rs — generic Envelope<P> with channel/flags/priority
// Purpose: typed CBOR envelope per docs/ADDON_BINARY_PROTOCOL_v1.md §4.
// Map encoding with integer keys 0..10, canonical (sorted by key).
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::ids::{SessionId, TraceId};

/// Protocol version transmitted in handshake and every Envelope.
pub const PROTOCOL_VERSION: u16 = 1;

/// Newtype wrapping the wire protocol_version. Decoder enforces `== PROTOCOL_VERSION`
/// (RFC says hard versioning, no forward/backward compat — §1 rule 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtocolVersion(pub u16);

impl ProtocolVersion {
    pub const V1: Self = Self(PROTOCOL_VERSION);
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::V1
    }
}

impl<C> Encode<C> for ProtocolVersion {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u16(self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ProtocolVersion {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let v = d.u16()?;
        if v != PROTOCOL_VERSION {
            return Err(minicbor::decode::Error::message(
                "unsupported protocol_version (only v1 accepted in this build)",
            ));
        }
        Ok(Self(v))
    }
}

/// Logical communication channel (§3). Encoded as `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Channel {
    Ui = 0x01,
    HostFn = 0x02,
    Stream = 0x03,
    Mesh = 0x04,
    Control = 0x05,
}

impl Channel {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Ui),
            0x02 => Some(Self::HostFn),
            0x03 => Some(Self::Stream),
            0x04 => Some(Self::Mesh),
            0x05 => Some(Self::Control),
            _ => None,
        }
    }
}

impl<C> Encode<C> for Channel {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(self.as_u8())?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Channel {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let v = d.u8()?;
        Self::from_u8(v)
            .ok_or_else(|| minicbor::decode::Error::message("unknown Channel discriminant"))
    }
}

/// Per-envelope priority class (§4 field 8). Default = Normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Priority {
    Bulk = 0,
    #[default]
    Normal = 1,
    Interactive = 2,
    Control = 3,
}

impl Priority {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
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
        e.u8(self.as_u8())?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Priority {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let v = d.u8()?;
        Self::from_u8(v)
            .ok_or_else(|| minicbor::decode::Error::message("unknown Priority discriminant"))
    }
}

/// Bitset of envelope flags (§4.1). Stored as u32 with the constants below.
/// Bits 4..31 are reserved and MUST be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(pub u32);

impl Flags {
    pub const NONE: Self = Self(0);
    pub const RELIABLE: Self = Self(1 << 0);
    pub const IDEMPOTENT: Self = Self(1 << 1);
    pub const REJECT_ON_OVERLOAD: Self = Self(1 << 2);
    pub const AUDIT_REQUIRED: Self = Self(1 << 3);

    /// All bits defined in v1. Any other bit is reserved and MUST be zero on the wire.
    pub const RESERVED_MASK: u32 = !(0b1111u32);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn insert(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl<C> Encode<C> for Flags {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u32(self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Flags {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let v = d.u32()?;
        if v & Self::RESERVED_MASK != 0 {
            return Err(minicbor::decode::Error::message(
                "Envelope.flags has reserved bits set (bits 4..31 MUST be zero)",
            ));
        }
        Ok(Self(v))
    }
}

/// Generic envelope parameterised over the channel-specific payload type.
///
/// Layout matches §4. Optional fields (`Option<T>`) are absent in the encoded
/// CBOR map when None — CBOR `null` is NOT used. Integer key order (0..10) is
/// canonical because all keys encode in a single byte (≤ 23) and bytewise
/// lexicographic ordering matches numeric ordering.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Envelope<P> {
    #[n(0)]
    pub protocol_version: ProtocolVersion,
    #[n(1)]
    pub channel: Channel,
    #[n(2)]
    pub msg_id: u64,
    #[n(3)]
    pub correlation_id: Option<u64>,
    #[n(4)]
    pub ts_ms: i64,
    #[n(5)]
    pub session_id: SessionId,
    #[n(6)]
    pub trace_id: Option<TraceId>,
    #[n(7)]
    pub deadline_ms: Option<u32>,
    #[n(8)]
    pub priority: Priority,
    #[n(9)]
    pub flags: Flags,
    #[n(10)]
    pub payload: P,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::control::{ControlPayload, SessionEnd, SessionEndCode};

    fn sample() -> Envelope<ControlPayload> {
        Envelope {
            protocol_version: ProtocolVersion::V1,
            channel: Channel::Control,
            msg_id: 42,
            correlation_id: Some(7),
            ts_ms: 1_700_000_000_000,
            session_id: SessionId::from_bytes([0x11; 16]),
            trace_id: None,
            deadline_ms: Some(5000),
            priority: Priority::Control,
            flags: Flags::RELIABLE.insert(Flags::AUDIT_REQUIRED),
            payload: ControlPayload::SessionEnd(SessionEnd {
                code: SessionEndCode::UserInitiated,
                reason: "user closed".into(),
            }),
        }
    }

    #[test]
    fn envelope_roundtrip_is_bit_identical() {
        let env = sample();
        let mut buf1 = Vec::new();
        minicbor::encode(&env, &mut buf1).unwrap();

        let decoded: Envelope<ControlPayload> = minicbor::decode(&buf1).unwrap();
        assert_eq!(decoded, env);

        let mut buf2 = Vec::new();
        minicbor::encode(&decoded, &mut buf2).unwrap();
        assert_eq!(buf1, buf2, "re-encoding must produce bit-identical bytes");
    }

    #[test]
    fn channel_rejects_unknown_value() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.u8(0x99).unwrap();
        let res: Result<Channel, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn flags_reject_reserved_bits() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.u32(1 << 5).unwrap();
        let res: Result<Flags, _> = minicbor::decode(&buf);
        assert!(res.is_err(), "reserved bits MUST be zero");
    }
}
