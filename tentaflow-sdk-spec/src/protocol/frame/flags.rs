// =============================================================================
// File: protocol/frame/flags.rs — Envelope flags bitfield (UFP/2 §3.4)
// Purpose: u32 newtype wrapping the immutable end-to-end flag bits. Bits 0–6
// allocated; bits 7–31 reserved (MUST be 0 — receivers reject unknown bits).
// All flag bits are part of the signature scope; no bit is mutated by hops.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

/// Envelope flags. All bits are immutable end-to-end and covered by the
/// signature (§3.4 + §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(pub u32);

impl Flags {
    pub const NONE: Self = Self(0);

    pub const IS_ENCRYPTED: u32 = 0x0001;
    pub const IS_COMPRESSED: u32 = 0x0002;
    pub const IS_REQUIRES_ACK: u32 = 0x0004;
    pub const IS_FRAGMENT: u32 = 0x0008;
    pub const IS_LAST_FRAGMENT: u32 = 0x0010;
    pub const IS_SIGNED: u32 = 0x0020;
    pub const IS_BROADCAST: u32 = 0x0040;

    /// Mask of all currently-allocated bits (0–6).
    pub const ALLOCATED_MASK: u32 = Self::IS_ENCRYPTED
        | Self::IS_COMPRESSED
        | Self::IS_REQUIRES_ACK
        | Self::IS_FRAGMENT
        | Self::IS_LAST_FRAGMENT
        | Self::IS_SIGNED
        | Self::IS_BROADCAST;

    pub fn contains(&self, bit: u32) -> bool {
        (self.0 & bit) != 0
    }

    pub fn with(self, bit: u32) -> Self {
        Self(self.0 | bit)
    }

    pub fn without(self, bit: u32) -> Self {
        Self(self.0 & !bit)
    }

    /// True iff every set bit lies within the allocated mask. Reserved bits
    /// MUST be 0 (§3.4 forward-compatibility rule).
    pub fn reserved_bits_clear(&self) -> bool {
        (self.0 & !Self::ALLOCATED_MASK) == 0
    }

    /// AAD-stable flags for fragmented messages: the `IS_LAST_FRAGMENT` bit
    /// is masked off so every fragment of a logical message produces the
    /// same AAD (§7.2). Returns the flags value to feed into AEAD AAD.
    pub fn aad_flags(&self) -> u32 {
        self.0 & !Self::IS_LAST_FRAGMENT
    }
}

impl From<u32> for Flags {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<Flags> for u32 {
    fn from(f: Flags) -> Self {
        f.0
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
        Ok(Self(d.u32()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_contains_and_with() {
        let f = Flags::NONE.with(Flags::IS_ENCRYPTED).with(Flags::IS_SIGNED);
        assert!(f.contains(Flags::IS_ENCRYPTED));
        assert!(f.contains(Flags::IS_SIGNED));
        assert!(!f.contains(Flags::IS_COMPRESSED));
    }

    #[test]
    fn flags_without() {
        let f = Flags(Flags::IS_ENCRYPTED | Flags::IS_SIGNED).without(Flags::IS_ENCRYPTED);
        assert!(!f.contains(Flags::IS_ENCRYPTED));
        assert!(f.contains(Flags::IS_SIGNED));
    }

    #[test]
    fn reserved_bits_clear_passes_for_allocated_only() {
        let f = Flags::NONE
            .with(Flags::IS_ENCRYPTED)
            .with(Flags::IS_COMPRESSED)
            .with(Flags::IS_FRAGMENT)
            .with(Flags::IS_LAST_FRAGMENT)
            .with(Flags::IS_SIGNED)
            .with(Flags::IS_BROADCAST)
            .with(Flags::IS_REQUIRES_ACK);
        assert!(f.reserved_bits_clear());
    }

    #[test]
    fn reserved_bits_clear_fails_for_bit_7() {
        let f = Flags(0x0080);
        assert!(!f.reserved_bits_clear());
    }

    #[test]
    fn reserved_bits_clear_fails_for_high_bit() {
        let f = Flags(0x8000_0000);
        assert!(!f.reserved_bits_clear());
    }

    #[test]
    fn aad_flags_masks_last_fragment() {
        let f = Flags(Flags::IS_FRAGMENT | Flags::IS_LAST_FRAGMENT | Flags::IS_SIGNED);
        let aad = f.aad_flags();
        assert_eq!(aad & Flags::IS_LAST_FRAGMENT, 0);
        assert_eq!(aad & Flags::IS_FRAGMENT, Flags::IS_FRAGMENT);
        assert_eq!(aad & Flags::IS_SIGNED, Flags::IS_SIGNED);
    }

    #[test]
    fn flags_roundtrip() {
        let f = Flags(Flags::IS_ENCRYPTED | Flags::IS_SIGNED | Flags::IS_FRAGMENT);
        let mut b = Vec::new();
        minicbor::encode(&f, &mut b).unwrap();
        let d: Flags = minicbor::decode(&b).unwrap();
        assert_eq!(d, f);
    }
}
