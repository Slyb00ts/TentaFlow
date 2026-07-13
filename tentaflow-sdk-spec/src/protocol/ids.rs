// =============================================================================
// File: protocol/ids.rs — fixed-size binary identifier newtypes
// Purpose: typed wrappers for bstr 16 IDs (SessionId, TraceId, NodeId,
// DeviceId, ClientActionId) and bstr 32 (Hash32 for capability hashes).
// Decoder rejects wrong-length byte strings.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

macro_rules! fixed_bstr_newtype {
    ($name:ident, $len:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub const fn from_bytes(b: [u8; $len]) -> Self {
                Self(b)
            }

            pub const fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }
        }

        impl<C> Encode<C> for $name {
            fn encode<W: minicbor::encode::Write>(
                &self,
                e: &mut Encoder<W>,
                _ctx: &mut C,
            ) -> Result<(), minicbor::encode::Error<W::Error>> {
                e.bytes(&self.0)?;
                Ok(())
            }
        }

        impl<'b, C> Decode<'b, C> for $name {
            fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
                let b = d.bytes()?;
                if b.len() != $len {
                    return Err(minicbor::decode::Error::message(concat!(
                        stringify!($name),
                        " must be exactly ",
                        stringify!($len),
                        " bytes"
                    )));
                }
                let mut arr = [0u8; $len];
                arr.copy_from_slice(b);
                Ok($name(arr))
            }
        }
    };
}

fixed_bstr_newtype!(
    SessionId,
    16,
    "Stable session identifier (UUID v4); survives resume."
);
fixed_bstr_newtype!(
    TraceId,
    16,
    "Distributed-tracing identifier (W3C trace-id form)."
);
fixed_bstr_newtype!(NodeId, 16, "TentaFlow node identifier in the mesh.");
fixed_bstr_newtype!(
    DeviceId,
    16,
    "Mobile/device identifier used for session pinning."
);
fixed_bstr_newtype!(
    ClientActionId,
    16,
    "Idempotency key for an Action submitted by the client."
);
fixed_bstr_newtype!(
    Hash32,
    32,
    "SHA-256 hash used in handshake Capability.hash and similar bstr 32 fields."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_session_id() {
        let id = SessionId::from_bytes([0xAB; 16]);
        let mut buf = Vec::new();
        minicbor::encode(&id, &mut buf).unwrap();
        let decoded: SessionId = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn reject_wrong_length() {
        let mut buf = Vec::new();
        let short = [0u8; 8];
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.bytes(&short).unwrap();
        let res: Result<SessionId, _> = minicbor::decode(&buf);
        assert!(res.is_err(), "must reject non-16-byte SessionId");
    }
}
