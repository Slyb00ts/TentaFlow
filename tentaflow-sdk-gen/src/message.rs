// =============================================================================
// File: message.rs — schema-aware message validator (Faza 6 Krok 4b)
//
// Given a `ManifestEnvelope` and a decoded `Component` (typed envelope from
// `tentaflow-sdk-spec`), verify the component conforms to its catalog
// schema entry:
//   - Tag is registered in the manifest.
//   - Every present field key has a `FieldMeta` entry.
//   - Every present field value matches the wire-type descriptor.
//   - Every required field (no default) is present.
//   - Nested `Component` / `ComponentRef<X>` values recurse through the
//     same validator.
//   - `Enum<X>` values match a known wire string.
//   - `Inline<X>` values match either an inline-struct schema or a tagged-
//     union schema (discriminator key + variant fields).
//
// Pre-condition: the input bytes must already have passed
// `tentaflow_sdk_spec::validate_canonical` (Krok 4a). This layer does
// *not* re-check wire-level canonicality; it operates on the typed
// `Component` produced by the typed decoder.
//
// Output: `Ok(())` if the component is valid, or `Err(MessageError)`
// describing the first defect (validation short-circuits — host policy
// rejects malformed messages outright).
// =============================================================================

use std::fmt;

use tentaflow_sdk_spec::{Component, Value};

use crate::manifest::{
    ComponentEntry, EnumEntry, FieldEntry, InlineEntry, ManifestEnvelope, UnionEntry,
    VariantEntry,
};

/// Message-level validation failure. `path` records the dotted access
/// path from the top-level component (e.g. `"Header.actions[0]"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for MessageError {}

impl MessageError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into() }
    }
}

/// Maximum nesting depth for schema validation. Matches the canonical
/// CBOR validator constant (`tentaflow_sdk_spec::canonical::MAX_NESTING_DEPTH`).
/// Even for already-decoded `Component` values this guards against
/// in-process callers handing us a deeply nested tree that would stack-
/// overflow the validator.
pub const MAX_VALIDATION_DEPTH: u32 = 64;

/// Validate a typed component against the manifest. See module docs.
pub fn validate_component(
    manifest: &ManifestEnvelope,
    component: &Component,
) -> Result<(), MessageError> {
    let mut ctx = Ctx::new(manifest);
    ctx.validate_component(component, component.id.clone())
}

struct Ctx<'a> {
    manifest: &'a ManifestEnvelope,
    depth: u32,
}

impl<'a> Ctx<'a> {
    fn new(manifest: &'a ManifestEnvelope) -> Self {
        Self { manifest, depth: 0 }
    }

    fn enter(&mut self, path: &str) -> Result<(), MessageError> {
        if self.depth >= MAX_VALIDATION_DEPTH {
            return Err(MessageError::new(
                path,
                format!("schema nesting exceeds MAX_VALIDATION_DEPTH ({MAX_VALIDATION_DEPTH})"),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    fn exit(&mut self) {
        debug_assert!(self.depth > 0);
        self.depth -= 1;
    }

    fn lookup_component(&self, tag: u16) -> Option<&'a ComponentEntry> {
        self.manifest.components.iter().find(|c| c.tag == tag)
    }

    fn lookup_enum(&self, name: &str) -> Option<&'a EnumEntry> {
        self.manifest.enums.iter().find(|e| e.name == name)
    }

    fn lookup_inline(&self, name: &str) -> Option<&'a InlineEntry> {
        self.manifest.inline_structs.iter().find(|s| s.name == name)
    }

    fn lookup_union(&self, name: &str) -> Option<&'a UnionEntry> {
        self.manifest.tagged_unions.iter().find(|u| u.name == name)
    }

    fn validate_component(
        &mut self,
        component: &Component,
        path: String,
    ) -> Result<(), MessageError> {
        self.enter(&path)?;
        let result = (|| -> Result<(), MessageError> {
            let meta = self.lookup_component(component.tag).ok_or_else(|| {
                MessageError::new(
                    &path,
                    format!("unknown component tag 0x{:04X}", component.tag),
                )
            })?;
            self.validate_field_map(
                &path,
                meta.name.as_str(),
                &component.fields.0,
                &meta.fields,
            )
        })();
        self.exit();
        result
    }

    fn validate_field_map(
        &mut self,
        path: &str,
        owner_name: &str,
        entries: &[(u8, Value)],
        schema: &[FieldEntry],
    ) -> Result<(), MessageError> {
        // 0. No duplicate field keys at the typed layer. Canonical bytes
        //    already reject duplicates, but the typed API is also a
        //    public entry point; defence in depth.
        {
            let mut seen: std::collections::HashSet<u8> = std::collections::HashSet::new();
            for (k, _) in entries {
                if !seen.insert(*k) {
                    return Err(MessageError::new(
                        format!("{path}/{owner_name}"),
                        format!("duplicate field key {k} in {owner_name}"),
                    ));
                }
            }
        }
        // 1. Every present (key, value) must map to a known FieldMeta and
        //    its value must satisfy the wire-type descriptor.
        for (key, value) in entries {
            let field = schema.iter().find(|f| f.key == *key).ok_or_else(|| {
                MessageError::new(
                    format!("{path}/{owner_name}"),
                    format!("unknown field key {key} for {owner_name}"),
                )
            })?;
            let field_path = format!("{path}/{}", field.name);
            self.validate_value(&field_path, &field.wire, value)?;
        }
        // 2. Every required (no default) field must be present.
        for field in schema {
            if field.required && field.default.is_none() {
                let present = entries.iter().any(|(k, _)| *k == field.key);
                if !present {
                    return Err(MessageError::new(
                        format!("{path}/{}", field.name),
                        format!(
                            "missing required field '{}' (key {}) of {}",
                            field.name, field.key, owner_name,
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_value(
        &mut self,
        path: &str,
        wire: &str,
        value: &Value,
    ) -> Result<(), MessageError> {
        // Strip `Option<...>` wrapper: `None` semantics on the wire are
        // "field key absent", so an Option here unconditionally accepts
        // the inner type (omission is handled at the field-map level).
        if let Some(inner) = wire.strip_prefix("Option<").and_then(|s| s.strip_suffix('>')) {
            return self.validate_value(path, inner, value);
        }
        // Array<T>: every element must satisfy T.
        if let Some(inner) = wire.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')) {
            let Value::Array(items) = value else {
                return Err(MessageError::new(
                    path,
                    format!("expected Array (wire '{wire}'), got {}", value_kind(value)),
                ));
            };
            for (i, v) in items.iter().enumerate() {
                self.validate_value(&format!("{path}[{i}]"), inner, v)?;
            }
            return Ok(());
        }
        // ComponentRef<X|Y|...>: must be a typed Component, recursively
        // validated, whose tag matches one of the allowed type names.
        if let Some(inner) = wire.strip_prefix("ComponentRef<").and_then(|s| s.strip_suffix('>')) {
            let targets: Vec<&str> = inner.split('|').collect();
            // ComponentRef payload is a CBOR Component envelope. Our
            // canonical encoder emits Component via `Component::encode`
            // (a map). After decode-to-Value it appears as Value::Map
            // shaped `{ 0: tag, 1: id, 2: fields, ... }`. The simplest
            // host-side path is to decode the bytes back to a typed
            // Component via the wire encoders. But the validator runs
            // post-decode and `Value::Map` is what we see — accept that
            // shape and walk it as a typed component reconstruction.
            let nested = component_from_value(value).map_err(|e| {
                MessageError::new(path, format!("ComponentRef payload: {e}"))
            })?;
            // Tag must match the manifest entry for one of `targets`.
            let allowed_tags: Vec<u16> = targets
                .iter()
                .filter_map(|t| self.lookup_component_by_name(t).map(|c| c.tag))
                .collect();
            if !allowed_tags.contains(&nested.tag) {
                return Err(MessageError::new(
                    path,
                    format!(
                        "ComponentRef<{inner}>: nested component tag 0x{:04X} not in allowed set",
                        nested.tag,
                    ),
                ));
            }
            return self.validate_component(&nested, path.to_string());
        }
        // Enum<X>: must be a tstr matching one of X's variant wires.
        if let Some(name) = wire.strip_prefix("Enum<").and_then(|s| s.strip_suffix('>')) {
            let Value::Text(s) = value else {
                return Err(MessageError::new(
                    path,
                    format!("expected Enum<{name}> as tstr, got {}", value_kind(value)),
                ));
            };
            let e = self.lookup_enum(name).ok_or_else(|| {
                MessageError::new(path, format!("Enum<{name}> not registered in manifest"))
            })?;
            if !e.variants.iter().any(|v| v.wire == *s) {
                return Err(MessageError::new(
                    path,
                    format!("Enum<{name}>: value '{s}' is not a known variant"),
                ));
            }
            return Ok(());
        }
        // Inline<X>: either an inline-struct payload (field-keyed map) or
        // a tagged-union payload (discriminated map). Both are CBOR maps.
        if let Some(name) = wire.strip_prefix("Inline<").and_then(|s| s.strip_suffix('>')) {
            if let Some(inline) = self.lookup_inline(name) {
                return self.validate_inline(path, name, inline, value);
            }
            if let Some(union) = self.lookup_union(name) {
                return self.validate_union(path, name, union, value);
            }
            return Err(MessageError::new(
                path,
                format!("Inline<{name}> not registered in manifest"),
            ));
        }
        // Primitives + opaque types.
        match wire {
            "bool" => match value {
                Value::Bool(_) => Ok(()),
                _ => err(path, "bool", value),
            },
            "u8" => match value {
                Value::U64(n) if *n <= u8::MAX as u64 => Ok(()),
                _ => err(path, "u8", value),
            },
            "u16" => match value {
                Value::U64(n) if *n <= u16::MAX as u64 => Ok(()),
                _ => err(path, "u16", value),
            },
            "u32" => match value {
                Value::U64(n) if *n <= u32::MAX as u64 => Ok(()),
                _ => err(path, "u32", value),
            },
            "u64" => match value {
                Value::U64(_) => Ok(()),
                _ => err(path, "u64", value),
            },
            "i32" => match value {
                Value::I64(n) if i32::try_from(*n).is_ok() => Ok(()),
                Value::U64(n) if *n <= i32::MAX as u64 => Ok(()),
                _ => err(path, "i32", value),
            },
            "i64" => match value {
                Value::I64(_) => Ok(()),
                Value::U64(n) if *n <= i64::MAX as u64 => Ok(()),
                _ => err(path, "i64", value),
            },
            "f64" => match value {
                Value::F64(_) => Ok(()),
                _ => err(path, "f64", value),
            },
            "tstr" => match value {
                Value::Text(_) => Ok(()),
                _ => err(path, "tstr", value),
            },
            "BindRef" => match value {
                // BindRef is a tagged union; we resolve via the manifest.
                Value::Map(_) => self.validate_union_lookup(path, "BindRef", value),
                _ => err(path, "BindRef", value),
            },
            "StatePath" => match value {
                // StatePath is an array of PathSegment tagged-union values.
                Value::Array(items) => {
                    for (i, item) in items.iter().enumerate() {
                        self.validate_union_lookup(
                            &format!("{path}[{i}]"),
                            "PathSegment",
                            item,
                        )?;
                    }
                    Ok(())
                }
                _ => err(path, "StatePath", value),
            },
            "Component" => {
                let nested = component_from_value(value).map_err(|e| {
                    MessageError::new(path, format!("Component payload: {e}"))
                })?;
                self.validate_component(&nested, path.to_string())
            }
            "CborMap" => match value {
                Value::Map(_) => Ok(()),
                _ => err(path, "CborMap", value),
            },
            "Value" => Ok(()), // anything goes
            other => Err(MessageError::new(
                path,
                format!("unknown wire descriptor '{other}'"),
            )),
        }
    }

    fn lookup_component_by_name(&self, name: &str) -> Option<&'a ComponentEntry> {
        self.manifest.components.iter().find(|c| c.name == name)
    }

    fn validate_inline(
        &mut self,
        path: &str,
        name: &str,
        inline: &InlineEntry,
        value: &Value,
    ) -> Result<(), MessageError> {
        let Value::Map(pairs) = value else {
            return Err(MessageError::new(
                path,
                format!("Inline<{name}>: expected CBOR map, got {}", value_kind(value)),
            ));
        };
        // Inline structs use integer u8 keys.
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let Value::U64(n) = k else {
                return Err(MessageError::new(
                    path,
                    format!("Inline<{name}>: map key is not u8 (got {})", value_kind(k)),
                ));
            };
            let key = u8::try_from(*n).map_err(|_| {
                MessageError::new(
                    path,
                    format!("Inline<{name}>: map key {n} exceeds u8 range"),
                )
            })?;
            entries.push((key, v.clone()));
        }
        self.validate_field_map(path, name, &entries, &inline.fields)
    }

    fn validate_union(
        &mut self,
        path: &str,
        name: &str,
        union: &UnionEntry,
        value: &Value,
    ) -> Result<(), MessageError> {
        let Value::Map(pairs) = value else {
            return Err(MessageError::new(
                path,
                format!("Inline<{name}>: expected CBOR map (tagged union), got {}", value_kind(value)),
            ));
        };
        // Tagged unions use tstr keys; discriminator is `union.discriminator_key`.
        let disc_value = pairs
            .iter()
            .find_map(|(k, v)| match k {
                Value::Text(s) if s == &union.discriminator_key => Some(v),
                _ => None,
            })
            .ok_or_else(|| {
                MessageError::new(
                    path,
                    format!(
                        "Inline<{name}>: missing discriminator key '{}'",
                        union.discriminator_key,
                    ),
                )
            })?;
        let Value::Text(disc_str) = disc_value else {
            return Err(MessageError::new(
                path,
                format!(
                    "Inline<{name}>: discriminator '{}' is not tstr (got {})",
                    union.discriminator_key,
                    value_kind(disc_value),
                ),
            ));
        };
        let variant = union
            .variants
            .iter()
            .find(|v| v.wire_kind == *disc_str)
            .ok_or_else(|| {
                MessageError::new(
                    path,
                    format!(
                        "Inline<{name}>: unknown wire_kind '{}' for discriminator '{}'",
                        disc_str, union.discriminator_key,
                    ),
                )
            })?;
        // Strict per-variant payload check: only the discriminator key plus
        // fields declared by THIS variant are accepted. Foreign keys (e.g.
        // a `path` next to `kind: "literal"` in a `BindRef`) get rejected
        // here, matching the behaviour of the hand-written typed decoders.
        self.validate_union_variant(path, name, &union.discriminator_key, variant, pairs)
    }

    fn validate_union_variant(
        &mut self,
        path: &str,
        name: &str,
        discriminator_key: &str,
        variant: &VariantEntry,
        pairs: &[(Value, Value)],
    ) -> Result<(), MessageError> {
        // 1. Reject duplicate tstr keys inside the variant payload (canonical
        //    bytes catch this too; defence-in-depth at the typed layer).
        {
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for (k, _) in pairs {
                if let Value::Text(s) = k {
                    if !seen.insert(s.as_str()) {
                        return Err(MessageError::new(
                            path,
                            format!("Inline<{name}>::{}: duplicate key '{s}'", variant.rust_name),
                        ));
                    }
                }
            }
        }
        // 2. Every key must be either the discriminator OR a declared
        //    field name of THIS variant; values must match the field wire.
        for (k, v) in pairs {
            let Value::Text(s) = k else {
                return Err(MessageError::new(
                    path,
                    format!("Inline<{name}>::{}: non-tstr key", variant.rust_name),
                ));
            };
            if s == discriminator_key {
                continue;
            }
            let field = variant.fields.iter().find(|f| &f.name == s).ok_or_else(|| {
                MessageError::new(
                    path,
                    format!(
                        "Inline<{name}>::{}: unknown field '{}' (not in variant schema)",
                        variant.rust_name, s,
                    ),
                )
            })?;
            let fpath = format!("{path}/{}", field.name);
            self.validate_value(&fpath, &field.wire, v)?;
        }
        // 3. Every required field of the variant must be present by name.
        for f in &variant.fields {
            if f.required && f.default.is_none() {
                let present = pairs.iter().any(|(k, _)| match k {
                    Value::Text(s) => s == &f.name,
                    _ => false,
                });
                if !present {
                    return Err(MessageError::new(
                        format!("{path}/{}", f.name),
                        format!(
                            "Inline<{name}>::{}: missing required field '{}'",
                            variant.rust_name, f.name,
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_union_lookup(
        &mut self,
        path: &str,
        name: &str,
        value: &Value,
    ) -> Result<(), MessageError> {
        let union = self.lookup_union(name).ok_or_else(|| {
            MessageError::new(path, format!("tagged union '{name}' not registered"))
        })?;
        self.validate_union(path, name, union, value)
    }
}

fn err(path: &str, wire: &str, got: &Value) -> Result<(), MessageError> {
    Err(MessageError::new(
        path,
        format!("expected {wire}, got {}", value_kind(got)),
    ))
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::U64(_) => "u64",
        Value::I64(_) => "i64",
        Value::F64(_) => "f64",
        Value::Bytes(_) => "bstr",
        Value::Text(_) => "tstr",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
    }
}

/// Reconstruct a typed `Component` from a `Value::Map` payload produced
/// by decoding component bytes into the generic `Value` enum. The map is
/// expected to follow the canonical Component layout: `{ 0: tag (u16),
/// 1: id (tstr), 2: fields (map<u8, Value>), ... }`. Optional handler /
/// a11y / visibility / test_id fields are ignored here — the schema
/// validator only needs `tag` and `fields`.
fn component_from_value(value: &Value) -> Result<Component, String> {
    use tentaflow_sdk_spec::FieldMap;
    let Value::Map(pairs) = value else {
        return Err(format!("expected map (Component), got {}", value_kind(value)));
    };
    let mut tag: Option<u16> = None;
    let mut id: Option<String> = None;
    let mut fields: Vec<(u8, Value)> = Vec::new();
    for (k, v) in pairs {
        let Value::U64(key) = k else {
            return Err(format!("Component map key not u8 (got {})", value_kind(k)));
        };
        match *key {
            0 => {
                let Value::U64(t) = v else {
                    return Err(format!("Component.tag not u16 (got {})", value_kind(v)));
                };
                tag = Some(u16::try_from(*t).map_err(|_| "Component.tag out of u16 range")?);
            }
            1 => {
                let Value::Text(s) = v else {
                    return Err(format!("Component.id not tstr (got {})", value_kind(v)));
                };
                id = Some(s.clone());
            }
            2 => {
                let Value::Map(inner) = v else {
                    return Err(format!("Component.fields not map (got {})", value_kind(v)));
                };
                for (ik, iv) in inner {
                    let Value::U64(k) = ik else {
                        return Err(format!(
                            "Component.fields key not u8 (got {})",
                            value_kind(ik),
                        ));
                    };
                    let key = u8::try_from(*k).map_err(|_| "Component field key out of u8 range")?;
                    fields.push((key, iv.clone()));
                }
            }
            // Other keys (handlers, a11y, visibility, test_id) are
            // ignored by the schema validator.
            _ => {}
        }
    }
    Ok(Component {
        tag: tag.ok_or("Component missing tag (key 0)")?,
        id: id.ok_or("Component missing id (key 1)")?,
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_manifest;
    use tentaflow_sdk_spec::{ALL_COMPONENTS, FieldMap, Value};

    fn component_with(tag: u16, fields: Vec<(u8, Value)>) -> Component {
        Component {
            tag, id: "x".into(), fields: FieldMap(fields),
            handlers: None, bind: None, a11y: None, visibility: None, test_id: None,
        }
    }

    fn manifest() -> ManifestEnvelope {
        build_manifest()
    }

    #[test]
    fn rejects_unknown_tag() {
        let m = manifest();
        let c = component_with(0xFFFE, vec![]);
        let err = validate_component(&m, &c).unwrap_err();
        assert!(err.message.contains("unknown component tag"));
    }

    #[test]
    fn rejects_unknown_field_key_for_known_tag() {
        let m = manifest();
        let any_tag = ALL_COMPONENTS[0].tag;
        let c = component_with(any_tag, vec![(250, Value::U64(0))]);
        let err = validate_component(&m, &c).unwrap_err();
        assert!(err.message.contains("unknown field key"));
    }

    #[test]
    fn rejects_missing_required_field() {
        // Header (0x0001) has required `icon` at key 0 and `title` at key 1.
        let c = component_with(0x0001, vec![]);
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("missing required field"));
    }

    #[test]
    fn rejects_wrong_type_for_field() {
        // Header.title (key 1) is BindRef (tagged union); passing a bool
        // must be rejected.
        let c = component_with(
            0x0001,
            vec![
                (0, Value::Map(vec![ // icon: Inline<IconRef>
                    (Value::Text("kind".into()), Value::Text("named".into())),
                    (Value::Text("name".into()), Value::Text("check".into())),
                ])),
                (1, Value::Bool(true)), // title: BindRef expected, got bool
                (4, Value::Array(vec![])), // meta_chips
                (5, Value::Array(vec![])), // actions
                (6, Value::Text("default".into())), // density
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("BindRef"));
    }

    #[test]
    fn rejects_unknown_enum_variant() {
        // Header.density (key 6) is Enum<Density>; passing an unknown
        // value must be rejected.
        let c = component_with(
            0x0001,
            vec![
                (0, Value::Map(vec![
                    (Value::Text("kind".into()), Value::Text("named".into())),
                    (Value::Text("name".into()), Value::Text("check".into())),
                ])),
                (1, Value::Map(vec![
                    (Value::Text("kind".into()), Value::Text("literal".into())),
                    (Value::Text("value".into()), Value::Text("Hi".into())),
                ])),
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![])),
                (6, Value::Text("__bogus__".into())),
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("not a known variant"));
    }

    #[test]
    fn rejects_component_ref_with_wrong_tag() {
        // Header.actions (key 5) is Array<ComponentRef<Button>>; embedding
        // a Fab (0x040C) must be rejected.
        let fab_payload = Value::Map(vec![
            (Value::U64(0), Value::U64(0x040C)), // wrong tag
            (Value::U64(1), Value::Text("fab".into())),
            (Value::U64(2), Value::Map(vec![])),
        ]);
        let c = component_with(
            0x0001,
            vec![
                (0, Value::Map(vec![
                    (Value::Text("kind".into()), Value::Text("named".into())),
                    (Value::Text("name".into()), Value::Text("check".into())),
                ])),
                (1, Value::Map(vec![
                    (Value::Text("kind".into()), Value::Text("literal".into())),
                    (Value::Text("value".into()), Value::Text("Hi".into())),
                ])),
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![fab_payload])),
                (6, Value::Text("default".into())),
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("not in allowed set"));
    }

    #[test]
    fn accepts_well_formed_header() {
        // Build a Header value that satisfies every schema constraint.
        let icon = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("named".into())),
            (Value::Text("name".into()), Value::Text("check".into())),
        ]);
        let title = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("literal".into())),
            (Value::Text("value".into()), Value::Text("Hi".into())),
        ]);
        let c = component_with(
            0x0001,
            vec![
                (0, icon),
                (1, title),
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![])),
                (6, Value::Text("default".into())),
            ],
        );
        validate_component(&manifest(), &c).expect("valid Header must validate");
    }

    #[test]
    fn rejects_foreign_field_in_tagged_union_variant() {
        // BindRef::Literal has only { kind, value }. Adding a foreign
        // `path` key (which would belong to ::Bound) must be rejected.
        let icon = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("named".into())),
            (Value::Text("name".into()), Value::Text("check".into())),
        ]);
        let title = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("literal".into())),
            (Value::Text("value".into()), Value::Text("Hi".into())),
            (Value::Text("path".into()), Value::Array(vec![])), // foreign
        ]);
        let c = component_with(
            0x0001,
            vec![
                (0, icon),
                (1, title),
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![])),
                (6, Value::Text("default".into())),
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("unknown field 'path'"));
    }

    #[test]
    fn rejects_duplicate_field_key_in_component_field_map() {
        // Two entries at key 1 — typed `Component` validator must reject
        // even though canonical bytes would catch this earlier on the wire.
        let icon = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("named".into())),
            (Value::Text("name".into()), Value::Text("check".into())),
        ]);
        let title = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("literal".into())),
            (Value::Text("value".into()), Value::Text("Hi".into())),
        ]);
        let c = component_with(
            0x0001,
            vec![
                (0, icon),
                (1, title.clone()),
                (1, title), // duplicate
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![])),
                (6, Value::Text("default".into())),
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("duplicate field key"));
    }

    #[test]
    fn rejects_excessive_validation_depth() {
        // Build a chain of nested Header-via-AppShell-like Components past
        // the MAX_VALIDATION_DEPTH using ComponentRef recursion through
        // Card.children (which is Array<Component>, allows any nested tag).
        // Simpler: nest Card (0x0106) inside Card.children up to depth+1.
        fn card(children: Value) -> Component {
            Component {
                tag: 0x0106, id: "c".into(),
                fields: FieldMap(vec![
                    (0, Value::Text("filled".into())),       // variant
                    (5, Value::Map(vec![                     // border
                        (Value::Text("kind".into()), Value::Text("none".into())),
                    ])),
                    (6, Value::Text("none".into())),          // background
                    (8, children),                            // children
                    (9, Value::Bool(false)),                  // interactive
                    (10, Value::Bool(false)),                 // clickable
                ]),
                handlers: None, bind: None, a11y: None,
                visibility: None, test_id: None,
            }
        }
        fn card_as_value(children: Value) -> Value {
            let c = card(children);
            let mut field_map: Vec<(Value, Value)> = Vec::new();
            for (k, v) in &c.fields.0 {
                field_map.push((Value::U64(*k as u64), v.clone()));
            }
            Value::Map(vec![
                (Value::U64(0), Value::U64(c.tag as u64)),
                (Value::U64(1), Value::Text(c.id.clone())),
                (Value::U64(2), Value::Map(field_map)),
            ])
        }
        let mut inner = Value::Array(vec![]);
        for _ in 0..(MAX_VALIDATION_DEPTH + 2) {
            inner = Value::Array(vec![card_as_value(inner)]);
        }
        let top = card(inner);
        let err = validate_component(&manifest(), &top).unwrap_err();
        assert!(err.message.contains("MAX_VALIDATION_DEPTH"));
    }

    #[test]
    fn rejects_optional_field_with_wrong_type() {
        // Header.status_badge is Option<Inline<InlineBadge>>. If present,
        // it must be a map. Passing a u64 must be rejected.
        let icon = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("named".into())),
            (Value::Text("name".into()), Value::Text("check".into())),
        ]);
        let title = Value::Map(vec![
            (Value::Text("kind".into()), Value::Text("literal".into())),
            (Value::Text("value".into()), Value::Text("Hi".into())),
        ]);
        let c = component_with(
            0x0001,
            vec![
                (0, icon),
                (1, title),
                (2, Value::U64(42)), // status_badge: should be a map
                (4, Value::Array(vec![])),
                (5, Value::Array(vec![])),
                (6, Value::Text("default".into())),
            ],
        );
        let err = validate_component(&manifest(), &c).unwrap_err();
        assert!(err.message.contains("InlineBadge") || err.message.contains("map"));
    }
}
