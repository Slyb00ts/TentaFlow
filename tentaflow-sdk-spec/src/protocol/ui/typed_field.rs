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

/// Error type returned from per-component `into_component` builders. Wraps
/// `minicbor::encode::Error<core::convert::Infallible>` from
/// [`encode_to_value`] (Vec writer infallible — only inner Encode validation
/// can produce errors).
pub type IntoComponentError = minicbor::encode::Error<core::convert::Infallible>;

/// Tag-mismatch validator used by every per-tag typed component's
/// `try_from_component` (catalog §2-§7).
pub fn ensure_tag(
    actual: u16,
    expected: u16,
    name: &'static str,
) -> Result<(), minicbor::decode::Error> {
    if actual != expected {
        return Err(minicbor::decode::Error::message(format!(
            "{name}: Component.tag = 0x{actual:04X}, expected 0x{expected:04X}"
        )));
    }
    Ok(())
}

/// Validate that a `ComponentRef<X>` carries the exact catalog tag of `X`.
/// Used on the encode path before pushing a nested-component field into a
/// parent's FieldMap (catalog §6/§7 `ComponentRef<Button>` references).
pub fn ensure_ref_tag_encode(
    actual: u16,
    expected: u16,
    parent: &'static str,
    field: &'static str,
) -> Result<(), IntoComponentError> {
    if actual != expected {
        return Err(minicbor::encode::Error::message(format!(
            "{parent}.{field}: ComponentRef tag mismatch (expected 0x{expected:04X}, got 0x{actual:04X})"
        )));
    }
    Ok(())
}

/// Decode-side counterpart of [`ensure_ref_tag_encode`] — verifies that a
/// nested `Component` carries the catalog tag required for the parent field.
pub fn ensure_ref_tag_decode(
    actual: u16,
    expected: u16,
    parent: &'static str,
    field: &'static str,
) -> Result<(), minicbor::decode::Error> {
    if actual != expected {
        return Err(minicbor::decode::Error::message(format!(
            "{parent}.{field}: ComponentRef tag mismatch (expected 0x{expected:04X}, got 0x{actual:04X})"
        )));
    }
    Ok(())
}

/// Reject duplicate occurrence of a tstr-discriminated map key in tagged-union
/// decoders (chunks 1.1-1.7). Pattern: check slot before each assignment.
///
/// ```ignore
/// "kind" => {
///     assert_no_dup_tstr(&kind, "ValidationRule", "kind")?;
///     kind = Some(d.str()?.to_string());
/// }
/// ```
pub fn assert_no_dup_tstr<T>(
    slot: &Option<T>,
    comp: &'static str,
    key: &'static str,
) -> Result<(), minicbor::decode::Error> {
    if slot.is_some() {
        return Err(minicbor::decode::Error::message(format!(
            "{comp}: duplicate key '{key}'"
        )));
    }
    Ok(())
}

/// Standard "missing required field" error.
pub fn missing_field(comp: &'static str, field: &'static str) -> minicbor::decode::Error {
    minicbor::decode::Error::message(format!("{comp}: missing required field `{field}`"))
}

/// Standard "unknown field key" error.
pub fn unknown_field(comp: &'static str, key: u8) -> minicbor::decode::Error {
    minicbor::decode::Error::message(format!("{comp}: unknown field key {key}"))
}

/// Reject `FieldMap` payloads with duplicate u8 keys (catalog schema —
/// each field index MUST appear at most once). Called at the top of every
/// `try_from_component` decoder.
pub fn ensure_no_duplicate_keys(
    comp: &'static str,
    entries: &[(u8, Value)],
) -> Result<(), minicbor::decode::Error> {
    let mut lo: u128 = 0;
    let mut hi: u128 = 0;
    for (k, _) in entries {
        let bit = 1u128 << (*k & 0x7f);
        let target = if *k < 128 { &mut lo } else { &mut hi };
        if *target & bit != 0 {
            return Err(minicbor::decode::Error::message(format!(
                "{comp}: duplicate FieldMap key {k}"
            )));
        }
        *target |= bit;
    }
    Ok(())
}

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
