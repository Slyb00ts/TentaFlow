// =============================================================================
// File: tentaflow-sdk-gen — codegen library
// Purpose: build `catalog-manifest/v1.cbor` artifacts from the
// `tentaflow-sdk-spec` registry and verify them via self-test.
//
// Wire format (manifest v1):
//   ManifestEnvelope = {
//     0: protocol_version (u16, always 1),
//     1: components       (array<ComponentEntry>),
//     2: enums            (array<EnumEntry>),
//     3: inline_structs   (array<InlineEntry>),
//     4: tagged_unions    (array<UnionEntry>),
//   }
//   UnionEntry = {
//     0: name (tstr), 1: discriminator_key (tstr),
//     2: variants (array<VariantEntry>),
//   }
//   VariantEntry = {
//     0: rust_name (tstr), 1: wire_kind (tstr),
//     2: fields (array<FieldEntry>),
//   }
//   ComponentEntry = {
//     0: tag (u16), 1: name (tstr), 2: section (tstr),
//     3: fields (array<FieldEntry>), 4: handlers (array<tstr>),
//   }
//   EnumEntry = { 0: name (tstr), 1: variants (array<EnumVariant>) }
//   EnumVariant = { 0: rust_name (tstr), 1: wire (tstr) }
//   InlineEntry = { 0: name (tstr), 1: fields (array<FieldEntry>) }
//   FieldEntry = {
//     0: key (u8), 1: name (tstr), 2: wire (tstr),
//     3: required (bool), 4?: default (tstr, only present when Some)
//   }
//
// CBOR encoding follows RFC 8949 §4.2.1 Core Deterministic Encoding:
// definite-length items only, integer minimum-width, map keys in
// sequential u8 order. Float widening is not applicable (no float
// payloads in the manifest).
// =============================================================================

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod gen_csharp;
pub mod gen_python;
pub mod gen_rust;
pub mod manifest;
pub mod message;
pub mod validation;

pub use manifest::{
    build_manifest, ComponentEntry, EnumEntry, EnumVariant, FieldEntry, InlineEntry,
    ManifestEnvelope, UnionEntry, VariantEntry, PROTOCOL_VERSION,
};
pub use message::{validate_component, MessageError};
pub use validation::{check_manifest, validate_manifest, ValidationError, ValidationReport};
