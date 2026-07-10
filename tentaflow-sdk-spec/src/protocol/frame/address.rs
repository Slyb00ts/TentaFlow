// =============================================================================
// File: protocol/frame/address.rs — NodeAddress (UFP/2 §3.3)
// Purpose: universal participant identifier. Ed25519 32-byte pubkey is the
// identity for nodes, users, services, addons. Anonymous and Broadcast use
// the all-zero 32-byte sentinel.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::envelope::NODE_ID_LEN;

/// Participant class. Ed25519 pubkey scheme is shared across kinds; the kind
/// byte tells the receiver how to interpret the identity (which registry to
/// look it up in, which trust pool, etc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeAddressKind {
    Anonymous = 0x00,
    Node = 0x01,
    User = 0x02,
    Service = 0x03,
    Addon = 0x04,
    Broadcast = 0x05,
}

impl NodeAddressKind {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(Self::Anonymous),
            0x01 => Some(Self::Node),
            0x02 => Some(Self::User),
            0x03 => Some(Self::Service),
            0x04 => Some(Self::Addon),
            0x05 => Some(Self::Broadcast),
            _ => None,
        }
    }
}

impl<C> Encode<C> for NodeAddressKind {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u8(*self as u8)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for NodeAddressKind {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let raw = d.u8()?;
        Self::from_u8(raw).ok_or_else(|| {
            minicbor::decode::Error::message("NodeAddressKind: unknown discriminant")
        })
    }
}

/// Canonical CBOR map representation of a participant address.
///
/// Wire schema (§3.3):
/// ```text
/// NodeAddress = {
///   0: kind  u8,
///   1: id    bstr(32),     ; Ed25519 pubkey OR all-zero sentinel
///   2: name  tstr          ; OPTIONAL, human label, NOT part of identity
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct NodeAddress {
    #[n(0)]
    pub kind: NodeAddressKind,
    #[cbor(n(1), with = "minicbor::bytes")]
    pub id: [u8; NODE_ID_LEN],
    #[n(2)]
    pub name: Option<String>,
}

impl NodeAddress {
    /// Sentinel id used by Anonymous and Broadcast kinds (32 zero bytes).
    pub const ZERO_ID: [u8; NODE_ID_LEN] = [0u8; NODE_ID_LEN];

    pub fn anonymous() -> Self {
        Self {
            kind: NodeAddressKind::Anonymous,
            id: Self::ZERO_ID,
            name: None,
        }
    }

    pub fn broadcast() -> Self {
        Self {
            kind: NodeAddressKind::Broadcast,
            id: Self::ZERO_ID,
            name: None,
        }
    }

    pub fn node(pubkey: [u8; NODE_ID_LEN]) -> Self {
        Self {
            kind: NodeAddressKind::Node,
            id: pubkey,
            name: None,
        }
    }

    pub fn user(pubkey: [u8; NODE_ID_LEN]) -> Self {
        Self {
            kind: NodeAddressKind::User,
            id: pubkey,
            name: None,
        }
    }

    /// True when the id field carries the all-zero sentinel. Required to be
    /// `true` for Anonymous and Broadcast kinds; required to be `false` for
    /// every other kind. Higher-level invariant validation lives in 4c1g.
    pub fn id_is_zero(&self) -> bool {
        self.id == Self::ZERO_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: NodeAddress) {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: NodeAddress = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn node_address_roundtrip_anonymous() {
        rt(NodeAddress::anonymous());
    }

    #[test]
    fn node_address_roundtrip_broadcast() {
        rt(NodeAddress::broadcast());
    }

    #[test]
    fn node_address_roundtrip_node_with_name() {
        let mut id = [0u8; NODE_ID_LEN];
        id[0] = 0xAA;
        id[31] = 0xFF;
        rt(NodeAddress {
            kind: NodeAddressKind::Node,
            id,
            name: Some("node-east-1".into()),
        });
    }

    #[test]
    fn node_address_roundtrip_user() {
        let id = [0x42u8; NODE_ID_LEN];
        rt(NodeAddress::user(id));
    }

    #[test]
    fn node_address_kind_rejects_unknown_discriminant() {
        let bad = [0xFFu8];
        let r: Result<NodeAddressKind, _> = minicbor::decode(&bad);
        assert!(r.is_err());
    }

    #[test]
    fn anonymous_id_is_zero() {
        assert!(NodeAddress::anonymous().id_is_zero());
    }

    #[test]
    fn node_id_non_zero() {
        let mut id = [0u8; NODE_ID_LEN];
        id[0] = 1;
        assert!(!NodeAddress::node(id).id_is_zero());
    }
}
