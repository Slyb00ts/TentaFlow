// =============================================================================
// File: protocol/ui/typed_field.rs — typed↔Value helpers for FieldMap building
// Purpose: typed component structs (molecules, layout primitives, data display
// etc.) round-trip their fields through `Value` to populate `FieldMap` on the
// wire. Helpers below provide the bridge — `encode_to_value` writes a typed
// Encode impl to bytes and parses them back as a `Value`; `decode_from_value`
// is the reverse for the typed extraction path.
//
// These helpers preserve SEMANTIC value through `Value`, not raw CBOR bytes:
// a typed encode + Value decode is a CBOR-equivalent representation, but
// `Value` may normalise float widths or integer widths along the way. For
// canonical wire encoding the wire path is `Component::encode` directly,
// which writes the FieldMap entries in u8-key order with canonical CBOR;
// the typed `Value` representation here is only an intermediate for building
// `FieldMap` from typed component structs.
// =============================================================================

use minicbor::{Decode, Encode};

use crate::protocol::value::Value;

/// Encode any `Encode<()>` type to a CBOR `Value`. Used by typed component
/// builders to populate `FieldMap` entries.
///
/// Returns an error when the inner type's `Encode` impl rejects the value
/// (e.g. nested `FieldMap` with duplicate u8 keys, or `Command::Download`
/// with an invalid filename grammar). Errors are flattened into
/// `minicbor::encode::Error<core::convert::Infallible>` because the writer
/// (`Vec<u8>`) is itself infallible — only the inner Encode-side validation
/// produces errors here.
pub fn encode_to_value<T: Encode<()>>(
    v: &T,
) -> Result<Value, minicbor::encode::Error<core::convert::Infallible>> {
    let mut buf: Vec<u8> = Vec::new();
    minicbor::encode(v, &mut buf)?;
    minicbor::decode::<Value>(&buf).map_err(|err| {
        minicbor::encode::Error::message(format!(
            "encode_to_value: inner encode produced bytes that do not decode as Value: {err}"
        ))
    })
}

/// Decode a `Value` into any `Decode<'_, ()>` typed target. The inverse of
/// [`encode_to_value`].
pub fn decode_from_value<T>(v: &Value) -> Result<T, minicbor::decode::Error>
where
    T: for<'b> Decode<'b, ()>,
{
    let mut buf: Vec<u8> = Vec::new();
    minicbor::encode(v, &mut buf).map_err(|err| {
        minicbor::decode::Error::message(format!(
            "decode_from_value: Value re-encode failed: {err}"
        ))
    })?;
    minicbor::decode::<T>(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::tokens::Tone;

    #[test]
    fn roundtrip_simple_enum() {
        let v = encode_to_value(&Tone::Primary).unwrap();
        let t: Tone = decode_from_value(&v).unwrap();
        assert_eq!(t, Tone::Primary);
    }

    #[test]
    fn roundtrip_vec_of_u32() {
        let original: Vec<u32> = vec![1, 2, 3];
        let v = encode_to_value(&original).unwrap();
        let back: Vec<u32> = decode_from_value(&v).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn encode_propagates_inner_validation_error() {
        // Encoding a Command::NavigateExternal with non-https URL fails at the
        // Command::encode level; encode_to_value must surface that, not panic.
        use crate::protocol::ui::command::Command;
        use crate::protocol::ui::tokens::NavigateTarget;
        let bad = Command::NavigateExternal {
            url: "http://example.com".into(),
            target: NavigateTarget::NewTab,
        };
        let res = encode_to_value(&bad);
        assert!(res.is_err(), "non-https URL must propagate as Err");
    }
}
