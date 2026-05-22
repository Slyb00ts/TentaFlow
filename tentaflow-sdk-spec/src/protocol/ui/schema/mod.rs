// =============================================================================
// File: protocol/ui/schema/mod.rs — codegen schema (machine-readable catalog)
// Consumed by `tentaflow-sdk-gen` (Krok 2) and SDK generator backends (Krok 6).
// Auto-generated `data.rs` is re-built by `scripts/gen_schema.py`.
// =============================================================================

pub mod data;
pub mod types;

pub use data::{ALL_COMPONENTS, ALL_ENUMS, ALL_INLINE_STRUCTS};
pub use types::{section, ComponentMeta, EnumMeta, FieldMeta, InlineMeta};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_has_no_duplicate_tags() {
        let mut seen: HashSet<u16> = HashSet::new();
        for c in ALL_COMPONENTS {
            assert!(
                seen.insert(c.tag),
                "duplicate tag 0x{:04X} (component '{}')",
                c.tag, c.name,
            );
        }
    }

    #[test]
    fn registry_has_no_duplicate_names() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for c in ALL_COMPONENTS {
            assert!(
                seen.insert(c.name),
                "duplicate component name '{}'",
                c.name,
            );
        }
    }

    #[test]
    fn registry_field_keys_are_unique_per_component() {
        for c in ALL_COMPONENTS {
            let mut seen: HashSet<u8> = HashSet::new();
            for f in c.fields {
                assert!(
                    seen.insert(f.key),
                    "{}: duplicate field key {} (field '{}')",
                    c.name, f.key, f.name,
                );
            }
        }
    }

    #[test]
    fn registry_section_strings_well_known() {
        let allowed = [
            section::MOLECULES, section::LAYOUT, section::DATA, section::FORM,
            section::ACTION, section::FEEDBACK, section::SPECIALIZED,
        ];
        for c in ALL_COMPONENTS {
            assert!(
                allowed.contains(&c.section),
                "{}: unknown section '{}'",
                c.name, c.section,
            );
        }
    }

    /// Recursive grammar validator for wire-string descriptors. See
    /// `types.rs` grammar comment.
    fn validate_wire(s: &str) -> Result<(), String> {
        let primitives = [
            "bool", "u8", "u16", "u32", "u64", "i32", "i64", "f32", "f64", "tstr",
            "BindRef", "StatePath", "Component", "CborMap", "Value",
        ];
        if primitives.contains(&s) {
            return Ok(());
        }
        for wrap in ["Option", "Array"] {
            let prefix = format!("{wrap}<");
            if let Some(inner) = s.strip_prefix(&prefix) {
                if let Some(inner) = inner.strip_suffix('>') {
                    return validate_wire(inner);
                }
                return Err(format!("malformed {wrap} wrapper: '{s}'"));
            }
        }
        for wrap in ["Enum", "Inline"] {
            let prefix = format!("{wrap}<");
            if let Some(inner) = s.strip_prefix(&prefix) {
                if let Some(name) = inner.strip_suffix('>') {
                    if name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Err(format!("bad {wrap} name: '{name}' in '{s}'"));
                    }
                    return Ok(());
                }
                return Err(format!("malformed {wrap} wrapper: '{s}'"));
            }
        }
        if let Some(inner) = s.strip_prefix("ComponentRef<") {
            if let Some(names) = inner.strip_suffix('>') {
                for n in names.split('|') {
                    if n.is_empty()
                        || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Err(format!("bad ComponentRef target '{n}' in '{s}'"));
                    }
                }
                return Ok(());
            }
            return Err(format!("malformed ComponentRef: '{s}'"));
        }
        Err(format!("unknown wire type: '{s}'"))
    }

    #[test]
    fn registry_wire_strings_conform_to_grammar() {
        for c in ALL_COMPONENTS {
            for f in c.fields {
                if let Err(e) = validate_wire(f.wire) {
                    panic!("{}: field '{}' wire '{}': {}", c.name, f.name, f.wire, e);
                }
            }
        }
        for s in ALL_INLINE_STRUCTS {
            for f in s.fields {
                if let Err(e) = validate_wire(f.wire) {
                    panic!(
                        "Inline<{}>: field '{}' wire '{}': {}",
                        s.name, f.name, f.wire, e,
                    );
                }
            }
        }
    }

    #[test]
    fn registry_component_ref_targets_exist_in_registry() {
        // For each ComponentRef<X> in any field, X must be a registered component.
        let names: std::collections::HashSet<&str> =
            ALL_COMPONENTS.iter().map(|c| c.name).collect();
        for c in ALL_COMPONENTS {
            for f in c.fields {
                // Walk inside Option<...> and Array<...> wrappers.
                let mut w = f.wire;
                while let Some(inner) = w
                    .strip_prefix("Option<").and_then(|s| s.strip_suffix('>'))
                    .or_else(|| w.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')))
                {
                    w = inner;
                }
                if let Some(inner) = w
                    .strip_prefix("ComponentRef<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    for target in inner.split('|') {
                        assert!(
                            names.contains(target),
                            "{}: field '{}' references unknown component '{}'",
                            c.name, f.name, target,
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn enum_registry_has_no_duplicate_names() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for e in ALL_ENUMS {
            assert!(seen.insert(e.name), "duplicate enum name '{}'", e.name);
        }
    }

    #[test]
    fn enum_registry_variants_have_unique_wire_strings() {
        for e in ALL_ENUMS {
            let mut seen: HashSet<&'static str> = HashSet::new();
            for (rust, wire) in e.variants {
                assert!(
                    seen.insert(wire),
                    "{}: duplicate wire string '{}' (variant {})",
                    e.name, wire, rust,
                );
            }
        }
    }

    #[test]
    fn inline_registry_has_no_duplicate_names() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for s in ALL_INLINE_STRUCTS {
            assert!(seen.insert(s.name), "duplicate inline struct name '{}'", s.name);
        }
    }

    #[test]
    fn inline_registry_field_keys_unique_per_struct() {
        for s in ALL_INLINE_STRUCTS {
            let mut seen: HashSet<u8> = HashSet::new();
            for f in s.fields {
                assert!(
                    seen.insert(f.key),
                    "{}: duplicate field key {} (field '{}')",
                    s.name, f.key, f.name,
                );
            }
        }
    }

    #[test]
    fn registry_referenced_enums_exist_in_registry() {
        let enum_names: HashSet<&str> = ALL_ENUMS.iter().map(|e| e.name).collect();
        let check = |wire: &'static str, ctx: String| {
            let mut w = wire;
            while let Some(inner) = w
                .strip_prefix("Option<").and_then(|s| s.strip_suffix('>'))
                .or_else(|| w.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')))
            {
                w = inner;
            }
            if let Some(name) = w.strip_prefix("Enum<").and_then(|s| s.strip_suffix('>')) {
                assert!(
                    enum_names.contains(name),
                    "{ctx}: Enum<{name}> not present in ALL_ENUMS",
                );
            }
        };
        for c in ALL_COMPONENTS {
            for f in c.fields {
                check(f.wire, format!("{}.{}", c.name, f.name));
            }
        }
        for s in ALL_INLINE_STRUCTS {
            for f in s.fields {
                check(f.wire, format!("{}.{}", s.name, f.name));
            }
        }
    }

    #[test]
    fn registry_referenced_inline_structs_exist_in_registry_or_tagged_union() {
        // Inline<X> may resolve to either a field-keyed inline struct (in
        // ALL_INLINE_STRUCTS) OR a manually-encoded tagged union which is
        // not yet captured by the registry. We accept the latter for now
        // via an allowlist, so that this test still catches typos.
        let inline_names: HashSet<&str> = ALL_INLINE_STRUCTS.iter().map(|s| s.name).collect();
        // Tagged-union inline types tracked manually until UnionMeta lands.
        let tagged_unions: HashSet<&str> = [
            "IconRef", "AvatarRef", "BreadcrumbItem", "SidebarItem", "SelectValue",
            "DimensionToken", "AspectRatio", "TableColumnWidth", "HeatmapScale",
            "DatePresetResolve", "BorderToken", "SplitSize", "GridCol", "GridTrack",
            // Lives outside inline.rs but is referenced as `Inline<ValueFormat>`.
            "ValueFormat",
            // Tagged union in validation.rs, referenced from Input/Textarea/TagInput
            // via `Inline<ValidationRule>`.
            "ValidationRule",
            // Tagged union in form/wrappers.rs, referenced from `Form.validators`.
            "FormValidator",
            // Tagged union in bind.rs, used inside StatePath array fields.
            "PathSegment",
            // Tagged union in handler.rs, referenced from BreadcrumbItem/SidebarItem.
            "LocalAction",
        ].into_iter().collect();
        let check = |wire: &'static str, ctx: String| {
            let mut w = wire;
            while let Some(inner) = w
                .strip_prefix("Option<").and_then(|s| s.strip_suffix('>'))
                .or_else(|| w.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')))
            {
                w = inner;
            }
            if let Some(name) = w.strip_prefix("Inline<").and_then(|s| s.strip_suffix('>')) {
                assert!(
                    inline_names.contains(name) || tagged_unions.contains(name),
                    "{ctx}: Inline<{name}> not present in ALL_INLINE_STRUCTS \
                    nor in the tagged-union allowlist",
                );
            }
        };
        for c in ALL_COMPONENTS {
            for f in c.fields {
                check(f.wire, format!("{}.{}", c.name, f.name));
            }
        }
        for s in ALL_INLINE_STRUCTS {
            for f in s.fields {
                check(f.wire, format!("{}.{}", s.name, f.name));
            }
        }
    }

    #[test]
    fn registry_enum_and_inline_targets_compile() {
        // Sanity: for every Enum<X> or Inline<X> referenced in any field,
        // X must be a plain identifier and (heuristic) belong to the set of
        // names that exist in `protocol::ui::tokens` / `protocol::ui::inline`.
        // We can't reflect that at compile time; this test guards the
        // grammar invariant that the name is well-formed (no `::`).
        for c in ALL_COMPONENTS {
            for f in c.fields {
                let mut w = f.wire;
                while let Some(inner) = w
                    .strip_prefix("Option<").and_then(|s| s.strip_suffix('>'))
                    .or_else(|| w.strip_prefix("Array<").and_then(|s| s.strip_suffix('>')))
                {
                    w = inner;
                }
                for tag in ["Enum<", "Inline<"] {
                    if let Some(name) = w.strip_prefix(tag).and_then(|s| s.strip_suffix('>')) {
                        assert!(
                            !name.contains("::") && !name.is_empty(),
                            "{}: field '{}' wire '{}': {} target must be plain identifier",
                            c.name, f.name, f.wire, tag.trim_end_matches('<'),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn registry_covers_all_chunks() {
        // Sanity counts per chunk — keeps the generator output honest.
        let by_section = |s: &'static str| {
            ALL_COMPONENTS.iter().filter(|c| c.section == s).count()
        };
        assert_eq!(by_section(section::MOLECULES), 12, "§2 Molecules");
        assert_eq!(by_section(section::LAYOUT), 18, "§3 Layout");
        assert_eq!(by_section(section::DATA), 38, "§4 Data Display");
        assert_eq!(by_section(section::FORM), 29, "§5 Form");
        assert_eq!(by_section(section::ACTION), 12, "§6 Action");
        assert_eq!(by_section(section::FEEDBACK), 15, "§7 Feedback");
        assert_eq!(by_section(section::SPECIALIZED), 14, "§8 Specialized");
    }
}
