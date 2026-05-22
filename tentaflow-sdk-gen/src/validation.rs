// =============================================================================
// File: validation.rs — standalone manifest invariant checker
// Purpose: verify a decoded `ManifestEnvelope` is internally consistent
// without any reference to the in-process `tentaflow-sdk-spec` registry.
// This is the exact validation contract a downstream C# / Python SDK
// generator (Krok 6) must rely on: given only the manifest bytes, all
// type references must resolve and every wire-string must parse.
//
// On success, returns a `ValidationReport` with structural counts. On any
// failure, returns a `ValidationError` describing the first defect (errors
// short-circuit — manifests are expected to be either fully valid or
// rejected outright).
// =============================================================================

use std::collections::HashSet;
use std::fmt;

use crate::manifest::{ComponentEntry, FieldEntry, InlineEntry, ManifestEnvelope, UnionEntry};

/// Structural summary returned by `validate_manifest` on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationReport {
    pub components: usize,
    pub enums: usize,
    pub inline_structs: usize,
    pub tagged_unions: usize,
    pub variants: usize,
    pub component_fields: usize,
    pub inline_fields: usize,
    pub variant_fields: usize,
}

/// Validation failure. The string describes the first defect found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

impl ValidationError {
    fn new(msg: impl Into<String>) -> Self {
        Self { message: msg.into() }
    }
}

const PROTOCOL_VERSION_EXPECTED: u16 = 1;

/// Allowed catalog section identifiers (mirrors `tentaflow_sdk_spec::section`).
const ALLOWED_SECTIONS: &[&str] = &[
    "§2 Molecules",
    "§3 Layout",
    "§4 Data Display",
    "§5 Form",
    "§6 Action",
    "§7 Feedback",
    "§8 Specialized",
];

/// Wire primitives recognised by the grammar (`types.rs`).
const PRIMITIVES: &[&str] = &[
    "bool", "u8", "u16", "u32", "u64", "i32", "i64", "f32", "f64", "tstr",
    "BindRef", "StatePath", "Component", "CborMap", "Value",
];

/// Recursive descent validator for a wire-string. Mirrors the grammar
/// documented in `tentaflow-sdk-spec::protocol::ui::schema::types`.
fn validate_wire(s: &str) -> Result<(), String> {
    if PRIMITIVES.contains(&s) {
        return Ok(());
    }
    for wrap in ["Option", "Array"] {
        let prefix = format!("{wrap}<");
        if let Some(inner) = s.strip_prefix(&prefix) {
            return inner
                .strip_suffix('>')
                .ok_or_else(|| format!("malformed {wrap} wrapper: '{s}'"))
                .and_then(validate_wire);
        }
    }
    for wrap in ["Enum", "Inline"] {
        let prefix = format!("{wrap}<");
        if let Some(inner) = s.strip_prefix(&prefix) {
            let name = inner
                .strip_suffix('>')
                .ok_or_else(|| format!("malformed {wrap} wrapper: '{s}'"))?;
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!("bad {wrap} target '{name}' in '{s}'"));
            }
            return Ok(());
        }
    }
    if let Some(inner) = s.strip_prefix("ComponentRef<") {
        let names = inner
            .strip_suffix('>')
            .ok_or_else(|| format!("malformed ComponentRef: '{s}'"))?;
        for n in names.split('|') {
            if n.is_empty()
                || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!("bad ComponentRef target '{n}' in '{s}'"));
            }
        }
        return Ok(());
    }
    Err(format!("unknown wire type: '{s}'"))
}

/// Walk a wire-string, calling `on_target(kind, target)` for each
/// `Enum<X>` / `Inline<X>` / `ComponentRef<X|Y|...>` reference encountered
/// inside any number of `Option<...>` / `Array<...>` wrappers.
fn for_each_target<F: FnMut(&str, &str)>(wire: &str, mut f: F) {
    let mut w = wire;
    while let Some(inner) = w
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
        .or_else(|| w.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')))
    {
        w = inner;
    }
    for kind in ["Enum", "Inline"] {
        let prefix = format!("{kind}<");
        if let Some(inner) = w.strip_prefix(&prefix) {
            if let Some(name) = inner.strip_suffix('>') {
                f(kind, name);
                return;
            }
        }
    }
    if let Some(inner) = w.strip_prefix("ComponentRef<") {
        if let Some(names) = inner.strip_suffix('>') {
            for n in names.split('|') {
                f("ComponentRef", n);
            }
        }
    }
}

/// Validate a decoded manifest in isolation. Returns a `ValidationReport`
/// on success, `ValidationError` on the first detected defect.
pub fn validate_manifest(m: &ManifestEnvelope) -> Result<ValidationReport, ValidationError> {
    if m.protocol_version != PROTOCOL_VERSION_EXPECTED {
        return Err(ValidationError::new(format!(
            "protocol_version: expected {PROTOCOL_VERSION_EXPECTED}, got {}",
            m.protocol_version,
        )));
    }

    // 1. Build target-name sets used by reference checks. Cross-registry
    //    uniqueness (inline_struct vs tagged_union) is enforced explicitly
    //    so an `Inline<X>` reference can never be ambiguous for codegen.
    let component_names: HashSet<&str> = m.components.iter().map(|c| c.name.as_str()).collect();
    let enum_names: HashSet<&str> = m.enums.iter().map(|e| e.name.as_str()).collect();
    let mut inline_names: HashSet<&str> = HashSet::new();
    for s in &m.inline_structs {
        if !inline_names.insert(s.name.as_str()) {
            return Err(ValidationError::new(format!(
                "inline_structs: duplicate name '{}'",
                s.name,
            )));
        }
    }
    for u in &m.tagged_unions {
        if !inline_names.insert(u.name.as_str()) {
            return Err(ValidationError::new(format!(
                "tagged_unions: name '{}' collides with an inline struct \
                (Inline<{}> would be ambiguous for codegen)",
                u.name, u.name,
            )));
        }
    }

    // 2. Components: no dup tags/names, valid section, unique field keys.
    let mut seen_tags: HashSet<u16> = HashSet::new();
    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut component_fields = 0usize;
    for c in &m.components {
        if !seen_tags.insert(c.tag) {
            return Err(ValidationError::new(format!(
                "components: duplicate tag 0x{:04X} (component '{}')",
                c.tag, c.name,
            )));
        }
        if !seen_names.insert(c.name.as_str()) {
            return Err(ValidationError::new(format!(
                "components: duplicate name '{}'",
                c.name,
            )));
        }
        if !ALLOWED_SECTIONS.contains(&c.section.as_str()) {
            return Err(ValidationError::new(format!(
                "components: '{}' has unknown section '{}'",
                c.name, c.section,
            )));
        }
        let ctx = format!("component '{}'", c.name);
        check_unique_field_keys(&c.fields, &ctx)?;
        check_unique_field_names(&c.fields, &ctx)?;
        for f in &c.fields {
            validate_field(f, &component_names, &enum_names, &inline_names, &ctx)?;
            component_fields += 1;
        }
    }

    // 3. Enums: no dup names, unique wire strings, unique variant rust_name.
    let mut seen_enum_names: HashSet<&str> = HashSet::new();
    for e in &m.enums {
        if !seen_enum_names.insert(e.name.as_str()) {
            return Err(ValidationError::new(format!(
                "enums: duplicate name '{}'",
                e.name,
            )));
        }
        let mut wires: HashSet<&str> = HashSet::new();
        let mut rusts: HashSet<&str> = HashSet::new();
        for v in &e.variants {
            if !wires.insert(v.wire.as_str()) {
                return Err(ValidationError::new(format!(
                    "enums: '{}' has duplicate wire string '{}' (variant {})",
                    e.name, v.wire, v.rust_name,
                )));
            }
            if !rusts.insert(v.rust_name.as_str()) {
                return Err(ValidationError::new(format!(
                    "enums: '{}' has duplicate rust_name '{}'",
                    e.name, v.rust_name,
                )));
            }
        }
    }

    // 4. Inline structs: unique field keys + unique field names. Dup-name
    //    across inline + tagged_union registries is enforced at section 1.
    let mut inline_fields = 0usize;
    for s in &m.inline_structs {
        let ctx = format!("inline struct '{}'", s.name);
        check_unique_field_keys(&s.fields, &ctx)?;
        check_unique_field_names(&s.fields, &ctx)?;
        for f in &s.fields {
            validate_field(f, &component_names, &enum_names, &inline_names, &ctx)?;
            inline_fields += 1;
        }
    }

    // 5. Tagged unions: unique wire_kind + unique rust_name per union,
    //    plus per-variant unique field keys/names.
    let mut variants = 0usize;
    let mut variant_fields = 0usize;
    for u in &m.tagged_unions {
        if u.discriminator_key.is_empty() {
            return Err(ValidationError::new(format!(
                "tagged_unions: '{}' has empty discriminator_key",
                u.name,
            )));
        }
        let mut seen_kinds: HashSet<&str> = HashSet::new();
        let mut seen_variants: HashSet<&str> = HashSet::new();
        for v in &u.variants {
            if !seen_kinds.insert(v.wire_kind.as_str()) {
                return Err(ValidationError::new(format!(
                    "tagged_unions: '{}' has duplicate wire_kind '{}' (variant {})",
                    u.name, v.wire_kind, v.rust_name,
                )));
            }
            if !seen_variants.insert(v.rust_name.as_str()) {
                return Err(ValidationError::new(format!(
                    "tagged_unions: '{}' has duplicate variant rust_name '{}'",
                    u.name, v.rust_name,
                )));
            }
            let ctx = format!("union variant '{}::{}'", u.name, v.rust_name);
            check_unique_field_keys(&v.fields, &ctx)?;
            check_unique_field_names(&v.fields, &ctx)?;
            for f in &v.fields {
                validate_field(f, &component_names, &enum_names, &inline_names, &ctx)?;
                variant_fields += 1;
            }
            variants += 1;
        }
    }

    Ok(ValidationReport {
        components: m.components.len(),
        enums: m.enums.len(),
        inline_structs: m.inline_structs.len(),
        tagged_unions: m.tagged_unions.len(),
        variants,
        component_fields,
        inline_fields,
        variant_fields,
    })
}

fn check_unique_field_keys(
    fields: &[FieldEntry],
    context: &str,
) -> Result<(), ValidationError> {
    let mut seen: HashSet<u8> = HashSet::new();
    for f in fields {
        if !seen.insert(f.key) {
            return Err(ValidationError::new(format!(
                "{context}: duplicate field key {} (field '{}')",
                f.key, f.name,
            )));
        }
    }
    Ok(())
}

fn check_unique_field_names(
    fields: &[FieldEntry],
    context: &str,
) -> Result<(), ValidationError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for f in fields {
        if !seen.insert(f.name.as_str()) {
            return Err(ValidationError::new(format!(
                "{context}: duplicate field name '{}' (key {})",
                f.name, f.key,
            )));
        }
    }
    Ok(())
}

fn validate_field(
    f: &FieldEntry,
    component_names: &HashSet<&str>,
    enum_names: &HashSet<&str>,
    inline_names: &HashSet<&str>,
    context: &str,
) -> Result<(), ValidationError> {
    if let Err(e) = validate_wire(&f.wire) {
        return Err(ValidationError::new(format!(
            "{context}: field '{}' wire '{}': {e}",
            f.name, f.wire,
        )));
    }
    let mut error: Option<String> = None;
    for_each_target(&f.wire, |kind, target| {
        if error.is_some() {
            return;
        }
        let resolved = match kind {
            "Enum" => enum_names.contains(target),
            "Inline" => inline_names.contains(target),
            "ComponentRef" => component_names.contains(target),
            _ => true,
        };
        if !resolved {
            error = Some(format!(
                "{context}: field '{}' wire '{}': {kind}<{target}> does not resolve",
                f.name, f.wire,
            ));
        }
    });
    if let Some(e) = error {
        return Err(ValidationError::new(e));
    }
    Ok(())
}

/// Convenience helper that delegates to `validate_manifest` and ignores the
/// detailed counts. Returns `Ok(())` on success.
pub fn check_manifest(m: &ManifestEnvelope) -> Result<(), ValidationError> {
    validate_manifest(m).map(|_| ())
}

// Re-export the unused fn / struct guards for downstream tests.
#[allow(dead_code)]
fn _ensure_used(_: &ComponentEntry, _: &InlineEntry, _: &UnionEntry) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_manifest, EnumEntry, EnumVariant};

    #[test]
    fn validates_in_process_manifest() {
        let m = build_manifest();
        let report = validate_manifest(&m).expect("manifest must validate");
        assert_eq!(report.components, m.components.len());
        assert_eq!(report.enums, m.enums.len());
        assert_eq!(report.inline_structs, m.inline_structs.len());
        assert_eq!(report.tagged_unions, m.tagged_unions.len());
        assert!(report.variants > 0);
        assert!(report.component_fields > 0);
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let mut m = build_manifest();
        m.protocol_version = 99;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("protocol_version"));
    }

    #[test]
    fn rejects_duplicate_component_tag() {
        let mut m = build_manifest();
        let first = m.components[0].clone();
        let mut dup = m.components[1].clone();
        dup.tag = first.tag;
        m.components.push(dup);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate tag"));
    }

    #[test]
    fn rejects_duplicate_component_name() {
        let mut m = build_manifest();
        let mut dup = m.components[1].clone();
        dup.name = m.components[0].name.clone();
        // Use an unallocated tag so the duplicate-name check fires before
        // the duplicate-tag check.
        dup.tag = 0xFFFE;
        m.components.push(dup);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate name"));
    }

    #[test]
    fn rejects_unknown_section() {
        let mut m = build_manifest();
        m.components[0].section = "§99 Bogus".into();
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("unknown section"));
    }

    #[test]
    fn rejects_unresolved_component_ref() {
        let mut m = build_manifest();
        // Find any component field with a ComponentRef and rewrite the target.
        let mut patched = false;
        'outer: for c in m.components.iter_mut() {
            for f in c.fields.iter_mut() {
                if f.wire.contains("ComponentRef<") {
                    f.wire = "ComponentRef<NotARealComponent>".into();
                    patched = true;
                    break 'outer;
                }
            }
        }
        assert!(patched, "expected at least one ComponentRef field");
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("ComponentRef<NotARealComponent>"));
    }

    #[test]
    fn rejects_unresolved_enum_target() {
        let mut m = build_manifest();
        let mut patched = false;
        'outer: for c in m.components.iter_mut() {
            for f in c.fields.iter_mut() {
                if f.wire.starts_with("Enum<") {
                    f.wire = "Enum<BogusEnum>".into();
                    patched = true;
                    break 'outer;
                }
            }
        }
        assert!(patched);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("Enum<BogusEnum>"));
    }

    #[test]
    fn rejects_unresolved_inline_target() {
        let mut m = build_manifest();
        let mut patched = false;
        'outer: for c in m.components.iter_mut() {
            for f in c.fields.iter_mut() {
                if f.wire.contains("Inline<") {
                    f.wire = "Option<Inline<BogusInline>>".into();
                    patched = true;
                    break 'outer;
                }
            }
        }
        assert!(patched);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("Inline<BogusInline>"));
    }

    #[test]
    fn rejects_malformed_wire_grammar() {
        let mut m = build_manifest();
        m.components[0].fields[0].wire = "Option<missing close".into();
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("malformed"));
    }

    #[test]
    fn rejects_unknown_wire_primitive() {
        let mut m = build_manifest();
        m.components[0].fields[0].wire = "u128".into();
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("unknown wire type"));
    }

    #[test]
    fn rejects_duplicate_field_key_in_component() {
        let mut m = build_manifest();
        if let Some(c) = m.components.iter_mut().find(|c| c.fields.len() >= 2) {
            let mut dup = c.fields[1].clone();
            dup.key = c.fields[0].key;
            c.fields.push(dup);
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate field key"));
    }

    #[test]
    fn rejects_duplicate_enum_wire_string() {
        let m = ManifestEnvelope {
            protocol_version: 1,
            components: vec![],
            enums: vec![EnumEntry {
                name: "Tone".into(),
                variants: vec![
                    EnumVariant { rust_name: "Primary".into(), wire: "primary".into() },
                    EnumVariant { rust_name: "Other".into(), wire: "primary".into() },
                ],
            }],
            inline_structs: vec![],
            tagged_unions: vec![],
        };
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate wire string"));
    }

    #[test]
    fn rejects_duplicate_wire_kind_in_union() {
        let mut m = build_manifest();
        if let Some(u) = m.tagged_unions.iter_mut().find(|u| u.variants.len() >= 2) {
            let other_kind = u.variants[1].wire_kind.clone();
            u.variants[0].wire_kind = other_kind;
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate wire_kind"));
    }

    #[test]
    fn rejects_duplicate_field_name_in_component() {
        let mut m = build_manifest();
        if let Some(c) = m.components.iter_mut().find(|c| c.fields.len() >= 2) {
            let mut dup = c.fields[1].clone();
            dup.name = c.fields[0].name.clone();
            dup.key = 250; // unique key so dup-key check doesn't fire first.
            c.fields.push(dup);
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate field name"));
    }

    #[test]
    fn rejects_duplicate_field_key_in_inline_struct() {
        let mut m = build_manifest();
        if let Some(s) = m.inline_structs.iter_mut().find(|s| s.fields.len() >= 2) {
            let mut dup = s.fields[1].clone();
            dup.key = s.fields[0].key;
            s.fields.push(dup);
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate field key"));
        assert!(err.message.contains("inline struct"));
    }

    #[test]
    fn rejects_duplicate_field_key_in_union_variant() {
        let mut m = build_manifest();
        let mut patched = false;
        'outer: for u in m.tagged_unions.iter_mut() {
            for v in u.variants.iter_mut() {
                if v.fields.len() >= 2 {
                    let mut dup = v.fields[1].clone();
                    dup.key = v.fields[0].key;
                    v.fields.push(dup);
                    patched = true;
                    break 'outer;
                }
            }
        }
        assert!(patched);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate field key"));
        assert!(err.message.contains("union variant"));
    }

    #[test]
    fn rejects_duplicate_enum_variant_rust_name() {
        let mut m = build_manifest();
        if let Some(e) = m.enums.iter_mut().find(|e| e.variants.len() >= 2) {
            let mut dup = e.variants[1].clone();
            dup.rust_name = e.variants[0].rust_name.clone();
            // Give it a unique wire so dup-wire check doesn't fire first.
            dup.wire = format!("__synthetic_{}", e.variants[0].wire);
            e.variants.push(dup);
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate rust_name"));
    }

    #[test]
    fn rejects_duplicate_union_variant_rust_name() {
        let mut m = build_manifest();
        if let Some(u) = m.tagged_unions.iter_mut().find(|u| u.variants.len() >= 2) {
            let mut dup = u.variants[1].clone();
            dup.rust_name = u.variants[0].rust_name.clone();
            // Unique wire_kind so the kind-dup check doesn't fire first.
            dup.wire_kind = format!("__synthetic_{}", u.variants[0].wire_kind);
            u.variants.push(dup);
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("duplicate variant rust_name"));
    }

    #[test]
    fn rejects_inline_struct_name_colliding_with_tagged_union() {
        let mut m = build_manifest();
        let union_name = m.tagged_unions[0].name.clone();
        // Append a synthetic inline struct with the same name.
        m.inline_structs.push(InlineEntry { name: union_name.clone(), fields: vec![] });
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("collides with an inline struct")
            || err.message.contains("inline_structs: duplicate name"));
    }

    #[test]
    fn for_each_target_walks_deeply_nested_wrappers() {
        // Direct unit-test of the walker via a synthetic minimal manifest.
        let m = ManifestEnvelope {
            protocol_version: 1,
            components: vec![ComponentEntry {
                tag: 0xFFAA, name: "X".into(), section: "§2 Molecules".into(),
                fields: vec![FieldEntry {
                    key: 0, name: "deeply".into(),
                    wire: "Option<Array<Option<Enum<MissingEnum>>>>".into(),
                    required: false, default: None,
                }],
                handlers: vec![],
            }],
            enums: vec![],
            inline_structs: vec![],
            tagged_unions: vec![],
        };
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("Enum<MissingEnum>"));
    }

    #[test]
    fn rejects_empty_discriminator_key() {
        let mut m = build_manifest();
        if let Some(u) = m.tagged_unions.iter_mut().next() {
            u.discriminator_key.clear();
        }
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.message.contains("empty discriminator_key"));
    }
}
