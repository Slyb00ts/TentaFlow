// =============================================================================
// File: gen_rust.rs — Rust addon-SDK UI module generator
//
// Unlike the C# / Python targets, Rust addons can consume the canonical typed
// structs from `tentaflow-sdk-spec` directly (the very encoders every other
// SDK is byte-compared against). Re-generating parallel struct definitions
// would fork the source of truth, so this generator instead emits:
//   1. explicit, catalog-complete re-exports of every component, token enum,
//      inline struct and tagged union declared in the manifest (plus the
//      fixed envelope/message support surface) — the addon-sdk build breaks
//      the moment the spec drops or renames a catalog type;
//   2. conformance tests binding the manifest to the spec types: component
//      `TAG` constants must equal the manifest tags and every string-enum
//      variant must encode to the manifest wire name.
// =============================================================================

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::ManifestEnvelope;

/// Fixed (non-catalog) re-exports: the envelope, panel/slot/state messages
/// and CBOR support types the UI client needs. Mirrors the hand-written
/// support surface of the C# SDK (Component.cs / Messages.cs / Value.cs).
const SUPPORT_EXPORTS: &[(&str, &[&str])] = &[
    ("protocol::control", &["CborMap"]),
    ("protocol::ui::a11y", &["Accessibility", "EventKind", "Visibility"]),
    ("protocol::ui::action", &["Action", "ActionAck", "FormFieldMap"]),
    ("protocol::ui::bind", &["StatePath", "MAX_STATE_PATH_SEGMENTS"]),
    ("protocol::ui::command", &["Command"]),
    (
        "protocol::ui::component",
        &["Component", "FieldMap", "HandlerMap", "TestId", "TestIdError", "TEST_ID_MAX_LEN"],
    ),
    ("protocol::ui::error_code", &["ErrorCode"]),
    ("protocol::ui::event", &["Event", "Topic", "TopicSegment"]),
    (
        "protocol::ui::handler",
        &[
            "HandlerValidationError",
            "DEBOUNCE_MAX_MS",
            "HANDLER_MAX_RECURSION_DEPTH",
            "HANDLER_MAX_TOTAL_STEPS",
            "SEQUENCE_MAX_ITEMS",
        ],
    ),
    (
        "protocol::ui::panel",
        &[
            "CloseReason",
            "PanelClose",
            "PanelError",
            "PanelOpen",
            "PanelOpenContext",
            "PanelReady",
            "PanelReset",
            "PanelShell",
            "Viewport",
        ],
    ),
    (
        "protocol::ui::slot",
        &["CachePolicy", "SlotDecl", "SlotDefault", "SlotSemantics", "SlotVisibility", "StateEntry"],
    ),
    ("protocol::ui::slot_msg", &["SlotClear", "SlotContent", "SlotHide", "SlotShow"]),
    (
        "protocol::ui::state",
        &["PatchRejectReason", "PatchRejected", "StatePatch", "StateReset", "StateSnapshot"],
    ),
    (
        "protocol::ui::typed_field",
        &["IntoComponentError", "decode_from_value", "encode_to_value"],
    ),
    (
        "protocol::ui::ui_payload",
        &["Batch", "BatchMember", "UiPayload", "UiTag", "BATCH_MAX_MEMBERS"],
    ),
    ("protocol::value", &["Value"]),
];

/// Catalog section header → spec module holding the typed component structs.
fn section_module(section: &str) -> &'static str {
    match section {
        "§2 Molecules" => "protocol::ui::molecules",
        "§3 Layout" => "protocol::ui::layout",
        "§4 Data Display" => "protocol::ui::data",
        "§5 Form" => "protocol::ui::form",
        "§6 Action" => "protocol::ui::actions",
        "§7 Feedback" => "protocol::ui::feedback",
        "§8 Specialized" => "protocol::ui::specialized",
        other => panic!("gen_rust: unknown catalog section '{other}'"),
    }
}

/// String-enum name → spec module. Most `string_enum!` blocks live in
/// `tokens.rs`; the exceptions below are declared next to their consumers.
/// A wrong/missing mapping fails loudly at addon-sdk compile time.
fn enum_module(name: &str) -> &'static str {
    match name {
        "BytesBase" | "DurationStyle" | "DateStyle" | "TimeStyle" | "DateTimeStyle" => {
            "protocol::ui::value_format"
        }
        "TrendDirection" => "protocol::ui::inline",
        "IconName" => "protocol::ui::icon_name",
        "ResumeMode" | "SessionEndCode" | "GrantRationale" | "BackpressureSeverity" => {
            "protocol::control"
        }
        _ => "protocol::ui::tokens",
    }
}

/// Inline-struct name → spec module (default: `inline.rs`).
fn inline_module(name: &str) -> &'static str {
    match name {
        "FormFieldValue" | "FieldError" | "ParamEntry" => "protocol::ui::action",
        "PatchOp" => "protocol::ui::patch",
        _ => "protocol::ui::inline",
    }
}

/// Tagged-union name → spec module (default: `inline.rs`).
fn union_module(name: &str) -> &'static str {
    match name {
        "ResumeStatus" | "RejectReason" | "RateLimitScope" => "protocol::control",
        "ActionStatus" => "protocol::ui::action",
        "PathSegment" | "BindRef" | "BindSpec" => "protocol::ui::bind",
        "FormValidator" => "protocol::ui::form",
        "FailurePolicy" | "LocalAction" | "Handler" => "protocol::ui::handler",
        "PatchOpKind" => "protocol::ui::patch",
        "ValidationRule" | "StateCondition" => "protocol::ui::validation",
        "ValueFormat" => "protocol::ui::value_format",
        _ => "protocol::ui::inline",
    }
}

/// Produce the complete `components_g.rs` source from the manifest.
pub fn generate(manifest: &ManifestEnvelope) -> String {
    let mut out = String::with_capacity(128 * 1024);
    emit_header(&mut out);
    emit_reexports(&mut out, manifest);
    emit_conformance_tests(&mut out, manifest);
    out
}

fn emit_header(out: &mut String) {
    out.push_str("// Auto-generated by tentaflow-sdk-gen — DO NOT EDIT\n");
    out.push_str("// Regenerate: ./scripts/gen-rust.sh\n");
    out.push_str("//\n");
    out.push_str("// Typed UI catalog v1 bindings for Rust addons. Every catalog component,\n");
    out.push_str("// token enum, inline struct and tagged union is re-exported here from\n");
    out.push_str("// `tentaflow-sdk-spec` — the canonical encoders all SDKs are byte-compared\n");
    out.push_str("// against. NOTE: the catalog `Box` layout component shadows `std::boxed::Box`\n");
    out.push_str("// under a glob import; alias the module (`use ...::ui_v1 as ui;`) when the\n");
    out.push_str("// same scope also needs the std pointer type.\n\n");
}

/// Merge the fixed support surface with the manifest-driven catalog lists
/// into a name→module map, rejecting conflicting placements, then emit one
/// `pub use` block per spec module.
fn emit_reexports(out: &mut String, manifest: &ManifestEnvelope) {
    let mut by_name: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut add = |name: &str, module: &'static str| {
        if let Some(prev) = by_name.get(name) {
            if *prev != module {
                panic!("gen_rust: '{name}' maps to both '{prev}' and '{module}'");
            }
            return;
        }
        by_name.insert(name.to_string(), module);
    };

    for (module, names) in SUPPORT_EXPORTS {
        for n in *names {
            add(n, module);
        }
    }
    for c in &manifest.components {
        add(&c.name, section_module(&c.section));
    }
    for e in &manifest.enums {
        add(&e.name, enum_module(&e.name));
    }
    for s in &manifest.inline_structs {
        add(&s.name, inline_module(&s.name));
    }
    for u in &manifest.tagged_unions {
        add(&u.name, union_module(&u.name));
    }

    let mut by_module: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (name, module) in by_name {
        by_module.entry(module).or_default().push(name);
    }

    for (module, names) in by_module {
        let _ = writeln!(out, "pub use tentaflow_sdk_spec::{module}::{{");
        let mut line = String::from("   ");
        for name in &names {
            if line.len() + name.len() + 2 > 92 {
                let _ = writeln!(out, "{line}");
                line = String::from("   ");
            }
            let _ = write!(line, " {name},");
        }
        if line.trim() != "" {
            let _ = writeln!(out, "{line}");
        }
        out.push_str("};\n");
    }
    out.push('\n');
}

/// Emit `#[cfg(test)]` conformance tests binding the spec types to the
/// manifest: tag constants and string-enum wire names.
fn emit_conformance_tests(out: &mut String, manifest: &ManifestEnvelope) {
    out.push_str("#[cfg(test)]\n");
    out.push_str("mod catalog_conformance {\n");
    out.push_str("    use super::*;\n\n");
    out.push_str("    fn tstr(s: &str) -> Vec<u8> {\n");
    out.push_str("        let mut b = Vec::new();\n");
    out.push_str("        minicbor::Encoder::new(&mut b).str(s).expect(\"tstr encode\");\n");
    out.push_str("        b\n");
    out.push_str("    }\n\n");
    out.push_str("    fn enc<T: minicbor::Encode<()>>(v: &T) -> Vec<u8> {\n");
    out.push_str("        minicbor::to_vec(v).expect(\"encode\")\n");
    out.push_str("    }\n\n");

    out.push_str("    #[test]\n");
    out.push_str("    fn component_tags_match_catalog_manifest() {\n");
    for c in &manifest.components {
        let _ = writeln!(
            out,
            "        assert_eq!({name}::TAG, 0x{tag:04X}u16, \"{name}\");",
            name = c.name,
            tag = c.tag,
        );
    }
    out.push_str("    }\n\n");

    out.push_str("    #[test]\n");
    out.push_str("    fn enum_wire_names_match_catalog_manifest() {\n");
    for e in &manifest.enums {
        for v in &e.variants {
            let _ = writeln!(
                out,
                "        assert_eq!(enc(&{en}::{va}), tstr(\"{wire}\"), \"{en}::{va}\");",
                en = e.name,
                va = v.rust_name,
                wire = v.wire,
            );
        }
    }
    out.push_str("    }\n");
    out.push_str("}\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_manifest, ComponentEntry, EnumEntry, EnumVariant};

    #[test]
    fn generates_component_reexport_and_tag_assert() {
        let manifest = ManifestEnvelope {
            protocol_version: 1,
            components: vec![ComponentEntry {
                tag: 0x0201,
                name: "Text".into(),
                section: "§4 Data Display".into(),
                fields: vec![],
                handlers: vec![],
            }],
            enums: vec![],
            inline_structs: vec![],
            tagged_unions: vec![],
        };
        let code = generate(&manifest);
        assert!(code.contains("pub use tentaflow_sdk_spec::protocol::ui::data::{"));
        assert!(code.contains(" Text,"));
        assert!(code.contains("assert_eq!(Text::TAG, 0x0201u16, \"Text\");"));
    }

    #[test]
    fn generates_enum_wire_asserts_with_module_exceptions() {
        let manifest = ManifestEnvelope {
            protocol_version: 1,
            components: vec![],
            enums: vec![
                EnumEntry {
                    name: "Tone".into(),
                    variants: vec![EnumVariant {
                        rust_name: "Primary".into(),
                        wire: "primary".into(),
                    }],
                },
                EnumEntry {
                    name: "IconName".into(),
                    variants: vec![EnumVariant {
                        rust_name: "Plus".into(),
                        wire: "plus".into(),
                    }],
                },
            ],
            inline_structs: vec![],
            tagged_unions: vec![],
        };
        let code = generate(&manifest);
        assert!(code.contains("pub use tentaflow_sdk_spec::protocol::ui::tokens::{"));
        assert!(code.contains("pub use tentaflow_sdk_spec::protocol::ui::icon_name::{"));
        assert!(code.contains(
            "assert_eq!(enc(&Tone::Primary), tstr(\"primary\"), \"Tone::Primary\");"
        ));
        assert!(code.contains(
            "assert_eq!(enc(&IconName::Plus), tstr(\"plus\"), \"IconName::Plus\");"
        ));
    }

    #[test]
    fn full_manifest_generates_every_catalog_name() {
        let manifest = build_manifest();
        let code = generate(&manifest);
        for c in &manifest.components {
            assert!(
                code.contains(&format!(" {},", c.name)),
                "missing component re-export: {}",
                c.name
            );
        }
        for e in &manifest.enums {
            assert!(
                code.contains(&format!(" {},", e.name)),
                "missing enum re-export: {}",
                e.name
            );
        }
        for s in &manifest.inline_structs {
            assert!(
                code.contains(&format!(" {},", s.name)),
                "missing inline re-export: {}",
                s.name
            );
        }
        for u in &manifest.tagged_unions {
            assert!(
                code.contains(&format!(" {},", u.name)),
                "missing union re-export: {}",
                u.name
            );
        }
        // The envelope/support surface must always be present.
        assert!(code.contains(" Component,"));
        assert!(code.contains(" UiPayload,"));
        assert!(code.contains(" SlotContent,"));
        assert!(code.contains(" Value,"));
    }

    #[test]
    fn section_module_mapping_is_total_for_registry() {
        let manifest = build_manifest();
        for c in &manifest.components {
            // Panics inside section_module on an unknown section.
            let _ = section_module(&c.section);
        }
    }
}
