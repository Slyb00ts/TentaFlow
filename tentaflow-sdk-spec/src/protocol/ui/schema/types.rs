// =============================================================================
// File: protocol/ui/schema/types.rs — codegen schema metadata types
// Purpose: machine-readable description of every typed component, enum and
// inline struct in the catalog. Consumed by `tentaflow-sdk-gen` (Krok 2) to
// emit `catalog-manifest/v1.cbor` and generated SDK code (C# / Python /
// future targets) in Kroki 6-7.
//
// Wire types are described as short type-strings (e.g. `"BindRef"`,
// `"Array<ComponentRef<Button>>"`, `"Option<DimensionToken>"`). Keeping
// the descriptor as a string sidesteps the const-recursion limitations of
// nested `&'static WireType` and makes the schema trivially const-
// initialisable in plain Rust. Codegen tokenises the strings.
//
// Type-string grammar (used by codegen):
//   primitive       := "bool" | "u8" | "u16" | "u32" | "u64" | "i32" | "i64" | "f64" | "tstr"
//   binding         := "BindRef" | "StatePath"
//   enum_ref        := "Enum<" enum-name ">"
//   inline_ref      := "Inline<" inline-name ">"
//   component       := "Component"
//   component_ref   := "ComponentRef<" component-name { "|" component-name } ">"
//   value           := "Value" | "CborMap"
//   container       := "Array<" type ">" | "Option<" type ">"
//   type            := primitive | binding | enum_ref | inline_ref | component
//                    | component_ref | value | container
// =============================================================================

/// Description of a single field within a typed component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldMeta {
    /// CBOR integer key (matches `FieldMap` u8 index).
    pub key: u8,
    /// Field name in source struct (matches Rust field name).
    pub name: &'static str,
    /// Wire encoding as type-string. See module docs for grammar.
    pub wire: &'static str,
    /// True iff the catalog marks the field as required (no `or null`).
    pub required: bool,
    /// Catalog default value if any, encoded as a short human/debug string
    /// (e.g. `"Density::Default"`, `"10_000u32"`, `"true"`). Codegen uses
    /// this only for documentation; runtime defaults live in the typed
    /// decoder.
    pub default: Option<&'static str>,
}

/// Top-level catalog component schema (one per `0x????` tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentMeta {
    /// Catalog tag (e.g. `0x0401` for Button).
    pub tag: u16,
    /// Rust type name in `tentaflow-sdk-spec`.
    pub name: &'static str,
    /// Catalog section header (§2 Molecules, §3 Layout, …).
    pub section: &'static str,
    /// Per-field metadata. Keys match wire `FieldMap` indices but are NOT
    /// required to be sequential (host validator handles ordering).
    pub fields: &'static [FieldMeta],
    /// Handler IDs the catalog declares for this component
    /// (e.g. `&["click", "focus", "blur"]`). Empty if none.
    pub handlers: &'static [&'static str],
}

/// String-enum schema (one per `tokens.rs` `string_enum!` entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumMeta {
    pub name: &'static str,
    /// Rust variant → wire `tstr` form.
    pub variants: &'static [(&'static str, &'static str)],
}

/// Inline struct schema (catalog §1.5 reusable types or per-component leaves).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineMeta {
    pub name: &'static str,
    pub fields: &'static [FieldMeta],
}

/// Catalog section identifiers used by `ComponentMeta.section`.
pub mod section {
    pub const MOLECULES: &str = "§2 Molecules";
    pub const LAYOUT: &str = "§3 Layout";
    pub const DATA: &str = "§4 Data Display";
    pub const FORM: &str = "§5 Form";
    pub const ACTION: &str = "§6 Action";
    pub const FEEDBACK: &str = "§7 Feedback";
    pub const SPECIALIZED: &str = "§8 Specialized";
}
