// =============================================================================
// File: protocol/frame/channel.rs — channel + kind discriminators (UFP/2 §4)
// Purpose: u8 channel + u16 kind newtypes with per-channel kind range
// validation. Channels group related message types for routing, ACL, and
// rate-limit policies; kind is the per-channel message type.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

/// Channel discriminator. See `channels` module for the allocated set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Channel(pub u8);

/// Per-channel message kind discriminator. The valid range depends on the
/// channel — see `channels::valid_kind_range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kind(pub u16);

impl<C> Encode<C> for Channel {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Channel {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        Ok(Self(d.u8()?))
    }
}

impl<C> Encode<C> for Kind {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u16(self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Kind {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        Ok(Self(d.u16()?))
    }
}

/// Allocated channel constants + per-channel kind range table per §4.
pub mod channels {
    use super::Channel;
    use core::ops::RangeInclusive;

    pub const UI: Channel = Channel(0x01);
    pub const HOST_FUNCTION: Channel = Channel(0x02);
    pub const STREAM: Channel = Channel(0x03);
    pub const MESH: Channel = Channel(0x04);
    pub const CONTROL: Channel = Channel(0x05);
    pub const SYNC_LEDGER: Channel = Channel(0x06);
    pub const FRONTEND: Channel = Channel(0x07);
    pub const DOMAIN: Channel = Channel(0x08);
    pub const FRAME_BLOB: Channel = Channel(0x09);

    /// Inclusive valid kind range for a known channel. Returns `None` for
    /// unallocated channels (0x0A..=0xFF) — validator (4c1g) treats `None`
    /// as `UnknownChannel` per §11 error code 0x0003.
    pub fn valid_kind_range(c: Channel) -> Option<RangeInclusive<u16>> {
        match c.0 {
            0x01 => Some(0x0001..=0x07FF),
            0x02 => Some(0x0001..=0x00FF),
            0x03 => Some(0x0001..=0x00FF),
            0x04 => Some(0x0010..=0x004E),
            0x05 => Some(0x0001..=0x00FF),
            0x06 => Some(0x0001..=0x00FF),
            0x07 => Some(0x0001..=0xFFFF),
            0x08 => Some(0x0001..=0xFFFF),
            0x09 => Some(0x0001..=0x000F),
            _ => None,
        }
    }

    pub fn is_allocated(c: Channel) -> bool {
        valid_kind_range(c).is_some()
    }

    /// `Control / Hello` kind — the only `(channel, kind)` pair where
    /// `auth.kind = Anonymous` is permitted (§11.3 + §5 handshake bootstrap).
    pub const KIND_CONTROL_HELLO: super::Kind = super::Kind(0x0001);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_constants_match_spec() {
        assert_eq!(channels::UI.0, 0x01);
        assert_eq!(channels::HOST_FUNCTION.0, 0x02);
        assert_eq!(channels::STREAM.0, 0x03);
        assert_eq!(channels::MESH.0, 0x04);
        assert_eq!(channels::CONTROL.0, 0x05);
        assert_eq!(channels::SYNC_LEDGER.0, 0x06);
        assert_eq!(channels::FRONTEND.0, 0x07);
        assert_eq!(channels::DOMAIN.0, 0x08);
        assert_eq!(channels::FRAME_BLOB.0, 0x09);
    }

    #[test]
    fn kind_range_ui_covers_catalog_tags() {
        let r = channels::valid_kind_range(channels::UI).unwrap();
        assert!(r.contains(&0x0001));
        assert!(r.contains(&0x07FF));
        assert!(!r.contains(&0x0800));
    }

    #[test]
    fn kind_range_mesh_matches_legacy_discriminators() {
        let r = channels::valid_kind_range(channels::MESH).unwrap();
        assert!(r.contains(&0x0010)); // HEARTBEAT
        assert!(r.contains(&0x004C)); // SYNC_SNAPSHOT_RESPONSE
        assert!(r.contains(&0x004D)); // ROUTING_SYNC
        assert!(r.contains(&0x004E)); // ROBOTS_ANNOUNCE
        assert!(!r.contains(&0x000F));
        assert!(!r.contains(&0x004F));
    }

    #[test]
    fn unknown_channel_returns_none() {
        assert!(channels::valid_kind_range(Channel(0x0A)).is_none());
        assert!(channels::valid_kind_range(Channel(0xFF)).is_none());
    }

    #[test]
    fn channel_roundtrip() {
        let c = channels::UI;
        let mut b = Vec::new();
        minicbor::encode(&c, &mut b).unwrap();
        let d: Channel = minicbor::decode(&b).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn kind_roundtrip() {
        let k = Kind(0x07FF);
        let mut b = Vec::new();
        minicbor::encode(&k, &mut b).unwrap();
        let d: Kind = minicbor::decode(&b).unwrap();
        assert_eq!(d, k);
    }
}
