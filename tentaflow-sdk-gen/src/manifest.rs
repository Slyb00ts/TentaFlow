// =============================================================================
// File: manifest.rs — ManifestEnvelope + entry types + builder
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};
use tentaflow_sdk_spec::{ALL_COMPONENTS, ALL_ENUMS, ALL_INLINE_STRUCTS, ALL_TAGGED_UNIONS};

/// Manifest protocol version. Bump on breaking schema changes.
pub const PROTOCOL_VERSION: u16 = 1;

/// Top-level manifest payload. Encoded as a CBOR map with sequential u8 keys.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ManifestEnvelope {
    #[n(0)]
    pub protocol_version: u16,
    #[n(1)]
    pub components: Vec<ComponentEntry>,
    #[n(2)]
    pub enums: Vec<EnumEntry>,
    #[n(3)]
    pub inline_structs: Vec<InlineEntry>,
    #[n(4)]
    pub tagged_unions: Vec<UnionEntry>,
}

/// One tagged-union schema (catalog manual `Encode`/`Decode` enum like
/// `IconRef`, `DimensionToken`, `ValueFormat`, `ValidationRule`, …).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct UnionEntry {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub discriminator_key: String,
    #[n(2)]
    pub variants: Vec<VariantEntry>,
}

/// One variant of a tagged-union (the actual payload schema per `kind` value).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct VariantEntry {
    #[n(0)]
    pub rust_name: String,
    #[n(1)]
    pub wire_kind: String,
    #[n(2)]
    pub fields: Vec<FieldEntry>,
}

/// One catalog component (a typed `0x????` UI tag).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ComponentEntry {
    #[n(0)]
    pub tag: u16,
    #[n(1)]
    pub name: String,
    #[n(2)]
    pub section: String,
    #[n(3)]
    pub fields: Vec<FieldEntry>,
    #[n(4)]
    pub handlers: Vec<String>,
}

/// One catalog string-enum (`tokens.rs` `string_enum!` block).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct EnumEntry {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub variants: Vec<EnumVariant>,
}

/// One enum variant. `rust_name` is the Rust identifier, `wire` is the on-the-wire
/// tstr value emitted by the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct EnumVariant {
    #[n(0)]
    pub rust_name: String,
    #[n(1)]
    pub wire: String,
}

/// One catalog inline struct (a `#[cbor(map)]` reusable type defined in
/// `protocol/ui/inline.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct InlineEntry {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub fields: Vec<FieldEntry>,
}

/// Per-field schema. `default` is encoded only when `Some` (catalog convention:
/// CBOR omission = `None`, never explicit null).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEntry {
    pub key: u8,
    pub name: String,
    pub wire: String,
    pub required: bool,
    pub default: Option<String>,
}

impl<C> Encode<C> for FieldEntry {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        let n = if self.default.is_some() { 5 } else { 4 };
        e.map(n)?;
        e.u8(0)?.u8(self.key)?;
        e.u8(1)?.str(&self.name)?;
        e.u8(2)?.str(&self.wire)?;
        e.u8(3)?.bool(self.required)?;
        if let Some(d) = &self.default {
            e.u8(4)?.str(d)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FieldEntry {
    fn decode(
        d: &mut Decoder<'b>,
        _ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut key: Option<u8> = None;
        let mut name: Option<String> = None;
        let mut wire: Option<String> = None;
        let mut required: Option<bool> = None;
        let mut default: Option<String> = None;
        // Track seen keys + last key for duplicate detection and canonical
        // key-order enforcement (sequential u8, strictly increasing).
        let mut last_key: Option<u8> = None;
        for _ in 0..len {
            let k = d.u8()?;
            if let Some(prev) = last_key {
                if k <= prev {
                    return Err(minicbor::decode::Error::message(format!(
                        "FieldEntry: non-canonical key order (got {k} after {prev})"
                    )));
                }
            }
            last_key = Some(k);
            match k {
                0 => key = Some(d.u8()?),
                1 => name = Some(d.str()?.to_string()),
                2 => wire = Some(d.str()?.to_string()),
                3 => required = Some(d.bool()?),
                4 => default = Some(d.str()?.to_string()),
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown FieldEntry key: {other}"
                    )))
                }
            }
        }
        Ok(FieldEntry {
            key: key.ok_or_else(|| minicbor::decode::Error::message("FieldEntry missing key"))?,
            name: name.ok_or_else(|| minicbor::decode::Error::message("FieldEntry missing name"))?,
            wire: wire.ok_or_else(|| minicbor::decode::Error::message("FieldEntry missing wire"))?,
            required: required
                .ok_or_else(|| minicbor::decode::Error::message("FieldEntry missing required"))?,
            default,
        })
    }
}

/// Materialise a `ManifestEnvelope` from the in-process `tentaflow-sdk-spec`
/// registry. Ordering matches the registry slice order (file scan order) for
/// reproducible byte-stable output across runs.
pub fn build_manifest() -> ManifestEnvelope {
    let components = ALL_COMPONENTS
        .iter()
        .map(|c| ComponentEntry {
            tag: c.tag,
            name: c.name.to_string(),
            section: c.section.to_string(),
            fields: c
                .fields
                .iter()
                .map(|f| FieldEntry {
                    key: f.key,
                    name: f.name.to_string(),
                    wire: f.wire.to_string(),
                    required: f.required,
                    default: f.default.map(|s| s.to_string()),
                })
                .collect(),
            handlers: c.handlers.iter().map(|h| h.to_string()).collect(),
        })
        .collect();
    let enums = ALL_ENUMS
        .iter()
        .map(|e| EnumEntry {
            name: e.name.to_string(),
            variants: e
                .variants
                .iter()
                .map(|(rust, wire)| EnumVariant {
                    rust_name: rust.to_string(),
                    wire: wire.to_string(),
                })
                .collect(),
        })
        .collect();
    let inline_structs = ALL_INLINE_STRUCTS
        .iter()
        .map(|s| InlineEntry {
            name: s.name.to_string(),
            fields: s
                .fields
                .iter()
                .map(|f| FieldEntry {
                    key: f.key,
                    name: f.name.to_string(),
                    wire: f.wire.to_string(),
                    required: f.required,
                    default: f.default.map(|s| s.to_string()),
                })
                .collect(),
        })
        .collect();
    let tagged_unions = ALL_TAGGED_UNIONS
        .iter()
        .map(|u| UnionEntry {
            name: u.name.to_string(),
            discriminator_key: u.discriminator_key.to_string(),
            variants: u
                .variants
                .iter()
                .map(|v| VariantEntry {
                    rust_name: v.rust_name.to_string(),
                    wire_kind: v.wire_kind.to_string(),
                    fields: v
                        .fields
                        .iter()
                        .map(|f| FieldEntry {
                            key: f.key,
                            name: f.name.to_string(),
                            wire: f.wire.to_string(),
                            required: f.required,
                            default: f.default.map(|s| s.to_string()),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    ManifestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        components,
        enums,
        inline_structs,
        tagged_unions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_manifest_includes_full_registry() {
        let m = build_manifest();
        assert_eq!(m.protocol_version, PROTOCOL_VERSION);
        assert_eq!(m.components.len(), ALL_COMPONENTS.len());
        assert_eq!(m.enums.len(), ALL_ENUMS.len());
        assert_eq!(m.inline_structs.len(), ALL_INLINE_STRUCTS.len());
        assert_eq!(m.tagged_unions.len(), ALL_TAGGED_UNIONS.len());
    }

    #[test]
    fn manifest_roundtrip_via_cbor() {
        let m = build_manifest();
        let bytes = minicbor::to_vec(&m).expect("encode");
        let back: ManifestEnvelope = minicbor::decode(&bytes).expect("decode");
        assert_eq!(back, m);
    }

    #[test]
    fn manifest_bytes_pass_strict_canonical_validation() {
        // The end-to-end smoke test for Krok 4a — the manifest we ship must
        // satisfy the canonical-CBOR validator we built for the host wire.
        let m = build_manifest();
        let bytes = minicbor::to_vec(&m).expect("encode");
        tentaflow_sdk_spec::validate_canonical(&bytes)
            .expect("emitted manifest must be canonical CBOR");
    }

    #[test]
    fn manifest_encoding_is_byte_stable() {
        let m = build_manifest();
        let b1 = minicbor::to_vec(&m).unwrap();
        let b2 = minicbor::to_vec(&m).unwrap();
        assert_eq!(b1, b2, "manifest encoding must be deterministic");
    }

    #[test]
    fn field_entry_default_round_trip_with_and_without_default() {
        let with = FieldEntry {
            key: 7, name: "step".into(), wire: "f64".into(),
            required: false, default: Some("0.5".into()),
        };
        let without = FieldEntry {
            key: 0, name: "id".into(), wire: "tstr".into(),
            required: true, default: None,
        };
        let b1 = minicbor::to_vec(&with).unwrap();
        let b2 = minicbor::to_vec(&without).unwrap();
        assert_eq!(minicbor::decode::<FieldEntry>(&b1).unwrap(), with);
        assert_eq!(minicbor::decode::<FieldEntry>(&b2).unwrap(), without);
    }

    #[test]
    fn field_entry_decode_rejects_duplicate_keys_via_non_canonical_order() {
        // Encode FieldEntry with key 1 then key 1 again (duplicate, also not
        // strictly increasing → rejected as non-canonical key order).
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.map(3).unwrap();
        e.u8(0).unwrap().u8(1).unwrap();
        e.u8(1).unwrap().str("name").unwrap();
        e.u8(1).unwrap().str("dup").unwrap();
        let err = minicbor::decode::<FieldEntry>(&buf).unwrap_err();
        assert!(format!("{err}").contains("non-canonical key order"));
    }

    #[test]
    fn field_entry_decode_rejects_unknown_keys() {
        // Encode a FieldEntry-like map with extra key 99.
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.map(5).unwrap();
        e.u8(0).unwrap().u8(1).unwrap();
        e.u8(1).unwrap().str("name").unwrap();
        e.u8(2).unwrap().str("tstr").unwrap();
        e.u8(3).unwrap().bool(true).unwrap();
        e.u8(99).unwrap().str("nope").unwrap();
        let err = minicbor::decode::<FieldEntry>(&buf).unwrap_err();
        assert!(format!("{err}").contains("unknown FieldEntry key"));
    }
}
