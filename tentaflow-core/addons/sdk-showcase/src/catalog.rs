// =============================================================================
// File: addons/sdk-showcase/src/catalog.rs
// Purpose: schema-driven sample generator for the SDK component catalog.
//          Walks tentaflow_sdk_spec::protocol::ui::schema registries
//          (ALL_COMPONENTS / ALL_ENUMS / ALL_INLINE_STRUCTS / ALL_TAGGED_UNIONS)
//          and builds one representative Component instance per catalog tag
//          with sample props, grouped per catalog section.
// =============================================================================

use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
use tentaflow_sdk_spec::protocol::ui::data::Text;
use tentaflow_sdk_spec::protocol::ui::layout::Stack;
use tentaflow_sdk_spec::protocol::ui::schema::{
    section, ComponentMeta, ALL_COMPONENTS, ALL_ENUMS, ALL_INLINE_STRUCTS, ALL_TAGGED_UNIONS,
};
use tentaflow_sdk_spec::protocol::ui::tokens::{FlexAlign, Spacing, TextStyle, Tone};
use tentaflow_sdk_spec::protocol::ui::typed_field::encode_to_value;
use tentaflow_sdk_spec::protocol::value::Value;

/// Nested component sampling depth cap — below this the generator emits a
/// plain Text leaf instead of recursing further.
const MAX_DEPTH: u32 = 4;

/// Component tags the JS sdk-runtime has NO renderer for yet — rendering any
/// of them throws "no renderer registered" and kills the whole slot, so the
/// catalog skips them and shows a per-tab info line instead.
/// MUST stay in sync with KNOWN_MISSING in
/// tentaflow-core/www/js/sdk-runtime/component-registry-completeness.test.js
/// and may only SHRINK (remove a tag here once its JS renderer ships).
const RENDERER_NOT_IMPLEMENTED: &[u16] = &[
    0x010A, // Sidebar
    0x010B, // Tabs
    0x0308, // TagInput
    0x0309, // MentionInput
    0x0601, // Canvas2D
    0x0602, // WebGLSurface
    0x0603, // WGPUSurface
    0x0701, // PermissionMatrix
    0x0702, // NetworkRuleEditor
    0x0703, // RelationGraph
    0x0704, // AlarmFeed
    0x0705, // WeeklyScheduleGrid
    0x0706, // AccessMatrix
    0x0707, // ReqCard
    0x0708, // DecisionRow
    0x0709, // Inbox
    0x070A, // RuntimeStatusGrid
];

/// Tab id → catalog section header. Returns None for non-catalog tabs.
pub fn section_for_tab(tab: &str) -> Option<&'static str> {
    match tab {
        "molecules" => Some(section::MOLECULES),
        "layout" => Some(section::LAYOUT),
        "data" => Some(section::DATA),
        "form" => Some(section::FORM),
        "action" => Some(section::ACTION),
        "feedback" => Some(section::FEEDBACK),
        "specialized" => Some(section::SPECIALIZED),
        _ => None,
    }
}

/// Build the catalog tab fragment for one section: a Stack interleaving a
/// caption (component name + tag) with a generated sample instance for every
/// component the schema declares in that section.
pub fn section_stack(tab: &str, section_header: &str) -> Component {
    let mut ctr: u64 = 0;
    let mut children: Vec<Component> = Vec::new();
    let mut hidden: u64 = 0;

    for meta in ALL_COMPONENTS.iter().filter(|m| m.section == section_header) {
        if RENDERER_NOT_IMPLEMENTED.contains(&meta.tag) {
            hidden += 1;
            continue;
        }
        ctr += 1;
        let caption = Text {
            content: BindRef::Literal(Value::Text(format!(
                "{} (0x{:04X})",
                meta.name, meta.tag
            ))),
            style: TextStyle::BodyStrong,
            tone: Some(Tone::Muted),
            align: None,
            wrap: None,
            max_lines: None,
            format: None,
        }
        .into_component(format!("cat-{}-hdr-{}", tab, ctr))
        .expect("Text caption encode");
        children.push(caption);
        children.push(sample_component(meta, 0, &mut ctr));
    }

    if hidden > 0 {
        let note = Text {
            content: BindRef::Literal(Value::Text(format!(
                "{} component{} hidden — JS renderer not implemented yet",
                hidden,
                if hidden == 1 { "" } else { "s" }
            ))),
            style: TextStyle::Caption,
            tone: Some(Tone::Muted),
            align: None,
            wrap: None,
            max_lines: None,
            format: None,
        }
        .into_component(format!("cat-{}-hidden-note", tab))
        .expect("Text hidden-note encode");
        children.push(note);
    }

    Stack {
        gap: Spacing::Lg,
        align: FlexAlign::Stretch,
        children,
        padding: Some(Spacing::Md),
    }
    .into_component(format!("catalog-{}", tab))
    .expect("Stack encode")
}

// =============================================================================
// Component instance synthesis
// =============================================================================

/// Build a sample Component for one schema entry. Every non-Option field gets
/// a synthesized value matching its wire type-string; Option fields are
/// omitted (decoders default them).
fn sample_component(meta: &ComponentMeta, depth: u32, ctr: &mut u64) -> Component {
    if depth > MAX_DEPTH {
        return text_leaf("nested sample", ctr);
    }
    let mut entries: Vec<(u8, Value)> = Vec::new();
    for f in meta.fields {
        if f.wire.starts_with("Option<") {
            continue;
        }
        entries.push((f.key, sample_value(f.wire, f.name, depth, ctr)));
    }
    *ctr += 1;
    Component {
        tag: meta.tag,
        id: format!("demo-{}-{}", meta.name.to_lowercase(), ctr),
        fields: FieldMap(entries),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

/// Minimal Text leaf used for nested `Component` fields and depth cutoff.
fn text_leaf(content: &str, ctr: &mut u64) -> Component {
    *ctr += 1;
    Text {
        content: BindRef::Literal(Value::Text(content.into())),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }
    .into_component(format!("demo-leaf-{}", ctr))
    .expect("Text leaf encode")
}

// =============================================================================
// Wire type-string sampling
// =============================================================================

/// Synthesize a sample Value for one wire type-string (see schema/types.rs
/// grammar). `field` drives small heuristics (numeric-looking BindRefs,
/// heading level range).
fn sample_value(wire: &str, field: &str, depth: u32, ctr: &mut u64) -> Value {
    if let Some(inner) = strip_generic(wire, "Option<") {
        return sample_value(inner, field, depth, ctr);
    }
    if let Some(inner) = strip_generic(wire, "Array<") {
        return Value::Array(vec![sample_value(inner, field, depth, ctr)]);
    }
    if let Some(name) = strip_generic(wire, "Enum<") {
        return enum_sample(name);
    }
    if let Some(name) = strip_generic(wire, "Inline<") {
        return inline_sample(name, depth, ctr);
    }
    if let Some(names) = strip_generic(wire, "ComponentRef<") {
        let first = names.split('|').next().unwrap_or(names);
        let comp = component_by_name(first)
            .map(|m| sample_component(m, depth + 1, ctr))
            .unwrap_or_else(|| text_leaf(first, ctr));
        return encode_to_value(&comp).unwrap_or(Value::Null);
    }
    match wire {
        "BindRef" => {
            let bind = BindRef::Literal(bind_literal(field));
            encode_to_value(&bind).unwrap_or(Value::Null)
        }
        "StatePath" => {
            let path = StatePath::new(vec![
                PathSegment::Key("demo".into()),
                PathSegment::Key(field.into()),
            ]);
            encode_to_value(&path).unwrap_or(Value::Null)
        }
        "tstr" => Value::Text(text_sample(field)),
        "bool" => Value::Bool(false),
        "u8" | "u16" | "u32" | "u64" => Value::U64(uint_sample(field)),
        "i32" | "i64" => Value::I64(1),
        "f64" => Value::F64(0.5),
        "Component" => {
            let comp = text_leaf("nested content", ctr);
            encode_to_value(&comp).unwrap_or(Value::Null)
        }
        "Value" => Value::Text("demo".into()),
        "CborMap" => Value::Map(Vec::new()),
        // Unknown type-string — keep the payload decodable.
        _ => Value::Null,
    }
}

fn strip_generic<'a>(wire: &'a str, prefix: &str) -> Option<&'a str> {
    wire.strip_prefix(prefix)?.strip_suffix('>')
}

/// Literal payload for BindRef sample — numeric for fields whose name implies
/// a number, text otherwise.
fn bind_literal(field: &str) -> Value {
    const NUMERIC_HINTS: &[&str] = &[
        "value", "current", "count", "percent", "progress", "rating", "total", "step",
    ];
    if NUMERIC_HINTS.iter().any(|h| field.contains(h)) {
        Value::U64(42)
    } else {
        Value::Text(text_sample(field))
    }
}

fn text_sample(field: &str) -> String {
    match field {
        "id" => "demo-id".into(),
        "src" | "url" | "href" => "https://example.invalid/demo".into(),
        _ => format!("Sample {}", field.replace('_', " ")),
    }
}

fn uint_sample(field: &str) -> u64 {
    match field {
        // Heading.level and friends are range-validated 1..=6.
        "level" => 2,
        "k" | "columns" | "cols" | "span" => 2,
        "max" | "total" => 100,
        _ => 1,
    }
}

// =============================================================================
// Enum / inline-struct / tagged-union sampling
// =============================================================================

fn enum_sample(name: &str) -> Value {
    ALL_ENUMS
        .iter()
        .find(|e| e.name == name)
        .and_then(|e| e.variants.first())
        .map(|(_, wire)| Value::Text((*wire).into()))
        .unwrap_or(Value::Null)
}

/// `Inline<X>` wires cover both derive-encoded inline structs (integer-keyed
/// CBOR maps) and manually encoded tagged unions (tstr-keyed maps with a
/// discriminator). Look up unions first, then inline structs.
fn inline_sample(name: &str, depth: u32, ctr: &mut u64) -> Value {
    if let Some(u) = ALL_TAGGED_UNIONS.iter().find(|u| u.name == name) {
        let variant = match u.variants.first() {
            Some(v) => v,
            None => return Value::Map(Vec::new()),
        };
        let mut entries: Vec<(Value, Value)> = vec![(
            Value::Text(u.discriminator_key.into()),
            Value::Text(variant.wire_kind.into()),
        )];
        for f in variant.fields {
            if f.wire.starts_with("Option<") {
                continue;
            }
            entries.push((
                Value::Text(f.name.into()),
                sample_value(f.wire, f.name, depth, ctr),
            ));
        }
        return canon_map(entries);
    }
    if let Some(i) = ALL_INLINE_STRUCTS.iter().find(|i| i.name == name) {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        for f in i.fields {
            if f.wire.starts_with("Option<") {
                continue;
            }
            entries.push((
                Value::U64(f.key as u64),
                sample_value(f.wire, f.name, depth, ctr),
            ));
        }
        return canon_map(entries);
    }
    Value::Map(Vec::new())
}

fn component_by_name(name: &str) -> Option<&'static ComponentMeta> {
    ALL_COMPONENTS.iter().find(|m| m.name == name).copied()
}

/// Sort map entries by the byte representation of their encoded keys so the
/// emitted CBOR stays canonical (RFC 8949 deterministic key order).
fn canon_map(mut entries: Vec<(Value, Value)>) -> Value {
    entries.sort_by_cached_key(|(k, _)| {
        let mut buf = Vec::new();
        let _ = minicbor::encode(k, &mut buf);
        buf
    });
    Value::Map(entries)
}
