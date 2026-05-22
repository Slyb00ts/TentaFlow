"""
Generate ComponentMeta consts for every typed component in tentaflow-sdk-spec.

Scans:
  src/protocol/ui/molecules/*.rs
  src/protocol/ui/layout/*.rs
  src/protocol/ui/data/*.rs
  src/protocol/ui/form/*.rs
  src/protocol/ui/actions/*.rs
  src/protocol/ui/feedback/*.rs
  src/protocol/ui/specialized/*.rs

Emits a single Rust file: src/protocol/ui/schema/data.rs
with `pub const <NAME>_SCHEMA: ComponentMeta = ...` per component and
`pub const ALL_COMPONENTS: &[&ComponentMeta] = &[...]`.
"""
import re
import sys
from pathlib import Path

SECTIONS = {
    "molecules": "MOLECULES",
    "layout": "LAYOUT",
    "data": "DATA",
    "form": "FORM",
    "actions": "ACTION",
    "feedback": "FEEDBACK",
    "specialized": "SPECIALIZED",
}

# Catalog-declared handler IDs per component (manual override; the catalog
# does not encode this in source struct fields). One source of truth synced
# with `docs/ADDON_UI_COMPONENT_CATALOG_v1.md` §2-§8 Handlers entries.
HANDLERS_OVERRIDES = {
    # §2 Molecules
    "WizardShell": ("step_change",),
    # §3 Layout
    "Tabs": ("select",),
    "NavTabs": ("select",),
    "Collapsible": ("open", "close"),
    "Pagination": ("change",),
    # §4 Data
    "Table": ("row_click", "row_double_click", "selection_change"),
    "List": ("item_click",),
    "Tree": ("expand", "collapse", "select"),
    "Chip": ("click", "remove"),
    "Tag": ("click",),
    "LineChart": ("point_hover", "range_select"),
    "BarChart": ("point_hover", "range_select"),
    "AreaChart": ("point_hover", "range_select"),
    "Heatmap": ("cell_click", "cell_hover"),
    # §5 Form
    "Input": ("input", "change", "submit", "focus", "blur"),
    "Textarea": ("input", "change", "focus", "blur"),
    "Select": ("change",),
    "MultiSelect": ("change",),
    "Combobox": ("change", "input"),
    "Autocomplete": ("change",),
    "Toggle": ("change",),
    "Checkbox": ("change",),
    "Radio": ("change",),
    "RadioGroup": ("change",),
    "RadioCardGroup": ("change",),
    "Slider": ("change", "commit"),
    "RangeSlider": ("change", "commit"),
    "SliderRow": ("change", "commit"),
    "DatePicker": ("change",),
    "DateRangePicker": ("change",),
    "TimePicker": ("change",),
    "DateTimePicker": ("change",),
    "FileInput": ("files_selected", "upload_progress", "upload_complete", "upload_error"),
    "Form": ("submit", "reset", "field_change"),
    # §6 Action
    "Button": ("click",),
    "IconButton": ("click",),
    "LinkButton": ("click",),
    "Link": ("click",),
    "MenuButton": ("select",),
    "Menu": ("select",),
    "SegmentedControl": ("change",),
    "FilterChips": ("change",),
    "Fab": ("click",),
    # §7 Feedback
    "Alert": ("dismiss",),
    "Modal": ("close",),
    "Drawer": ("close",),
    "OfflineBanner": ("retry",),
    "ConfirmationDialog": ("confirm", "cancel"),
    # §8 Specialized
    "VideoStream": ("play", "pause", "stream_error", "loaded"),
    "LiveCameraTile": ("click", "fullscreen"),
    "MapView": ("click", "marker_click", "zoom_end", "pan_end"),
    "CodeEditor": ("change", "blur", "save_shortcut"),
    "Audio": ("play", "pause", "ended"),
    "ImageGallery": ("image_click",),
    "StepProgress": ("step_click",),
    "VirtualizedLog": ("event_click", "scroll_top", "filter_change"),
}

# Manual default-on-decode overrides for cases the parser can't detect
# automatically (e.g. `let resolved = field.unwrap_or_else(...); field: resolved`
# instead of `field: field.unwrap_or_else(...)`).
DEFAULTS_OVERRIDES = {
    # Card.shadow has variant-dependent default applied via local binding.
    ("Card", "shadow"): "variant-dependent (Elevated → Subtle, others → None)",
}

# Override map for fields whose Rust type is plain `Component` or
# `Vec<Component>` but the catalog/spec mandates a specific nested tag.
# Keyed by (struct_name, field_name) → catalog target type (one of: a single
# type name like "Button", or pipe-separated alternatives "Foo|Bar").
COMPONENT_REF_OVERRIDES = {
    # §2 Molecules
    ("Header", "actions"): "Button",
    ("PageHeader", "actions"): "Button",
    ("SectionHeader", "actions"): "Button",
    ("EmptyState", "primary_action"): "Button",
    ("EmptyState", "secondary_action"): "Button",
    ("Toolbar", "search"): "SearchBox",
    ("Toolbar", "view_mode"): "SegmentedControl",
    ("Toolbar", "sort_control"): "Select",
    ("Toolbar", "trailing_actions"): "Button",
    ("ErrorBoundary", "actions"): "Button",
    ("WelcomeHero", "primary_action"): "Button",
    ("WelcomeHero", "secondary_action"): "Button",
    ("StatGroup", "stats"): "StatCard",
    ("Inspector", "actions"): "Button",
    # §3 Layout
    ("SectionCard", "header_actions"): "Button",
    # §4 Data
    ("Table", "empty_state"): "EmptyState",
    ("Table", "row_actions"): "Button",
    ("Table", "bulk_actions"): "Button",
    ("List", "empty_state"): "EmptyState",
    # §6 Action
    ("ButtonGroup", "buttons"): "Button",
    ("ActionBar", "leading_actions"): "Button",
    ("ActionBar", "trailing_actions"): "Button",
    ("WizardFooter", "back_action"): "Button",
    ("WizardFooter", "next_action"): "Button",
    ("WizardFooter", "cancel_action"): "Button",
    ("WizardFooter", "skip_action"): "Button",
    ("WizardFooter", "extra_actions"): "Button",
    # §7 Feedback
    ("Alert", "actions"): "Button",
    ("Banner", "action"): "Button",
    ("GateScreen", "actions"): "Button",
}

# Rust type → wire-string mapping. Order-sensitive (Vec/Option matched first).
PRIMS = {
    "bool", "u8", "u16", "u32", "u64", "i32", "i64", "f64",
}

# Known tstr-enum names: discover from tokens.rs.
ENUMS = set()

# Known inline-struct names: discover from inline.rs.
INLINE_STRUCTS = set()

# Map: lib.rs re-export name → Rust struct name (we keep them same).
# Component-ref tags: known TAG of typed components, populated as we scan.
COMPONENT_TAGS = {}  # name -> "0xNNNN"


def discover_enums(repo: Path) -> None:
    t = (repo / "src/protocol/ui/tokens.rs").read_text()
    for m in re.finditer(r"pub enum (\w+) \{", t):
        ENUMS.add(m.group(1))


def discover_inline_structs(repo: Path) -> None:
    t = (repo / "src/protocol/ui/inline.rs").read_text()
    for m in re.finditer(r"pub struct (\w+) \{", t):
        INLINE_STRUCTS.add(m.group(1))
    for m in re.finditer(r"pub enum (\w+) \{", t):
        INLINE_STRUCTS.add(m.group(1))


def apply_overrides(struct_name: str, field_name: str, wire: str) -> str:
    """Apply (struct, field) → ComponentRef override on top of the inferred wire."""
    override = COMPONENT_REF_OVERRIDES.get((struct_name, field_name))
    if not override:
        return wire
    if wire == "Component":
        return f"ComponentRef<{override}>"
    if wire == "Array<Component>":
        return f"Array<ComponentRef<{override}>>"
    if wire == "Option<Component>":
        return f"Option<ComponentRef<{override}>>"
    if wire == "Option<Array<Component>>":
        return f"Option<Array<ComponentRef<{override}>>>"
    return wire


def rust_type_to_wire(rust: str, doc: str = "") -> str:
    """Convert Rust type annotation to wire-string.

    `doc` is the trailing doc comment for the field (lowercase); used to
    detect ComponentRef<X> overrides for raw `Component` / `Vec<Component>`.
    """
    rust = rust.strip()
    # Strip `'static` references.
    if rust.startswith("&"):
        rust = rust[1:].strip()
    # Normalize qualified paths inside the type expression: strip any
    # `super::*::` / `crate::*::` prefix so `super::super::tokens::Spacing`
    # becomes `Spacing` (also inside generics).
    rust = re.sub(r"(?:[A-Za-z_][A-Za-z0-9_]*::)+([A-Za-z_][A-Za-z0-9_]*)", r"\1", rust)

    if rust.startswith("Option<") and rust.endswith(">"):
        inner = rust[len("Option<"):-1]
        return f"Option<{rust_type_to_wire(inner, doc)}>"
    if rust.startswith("Vec<") and rust.endswith(">"):
        inner = rust[len("Vec<"):-1]
        inner_wire = rust_type_to_wire(inner, doc)
        return f"Array<{inner_wire}>"
    if rust == "String":
        return "tstr"
    if rust == "BindRef":
        return "BindRef"
    if rust == "StatePath":
        return "StatePath"
    if rust == "Component":
        # Doc may say `ComponentRef<X>` (case-insensitive match, preserve case in X).
        m = re.search(r"componentref<(\w+)", doc, re.IGNORECASE)
        if m:
            return f"ComponentRef<{m.group(1)}>"
        return "Component"
    if rust == "CborMap":
        return "CborMap"
    if rust == "Value":
        return "Value"
    if rust in PRIMS:
        return rust
    if rust in ENUMS:
        return f"Enum<{rust}>"
    if rust in INLINE_STRUCTS:
        return f"Inline<{rust}>"
    # Fallback — assume inline struct name.
    return f"Inline<{rust}>"


COMPONENT_RE = re.compile(
    r"pub struct (?P<name>\w+) \{\s*(?P<body>.*?)\}\s*impl \1 \{\s*pub const TAG: u16 = (?P<tag>0x[0-9A-Fa-f]+);",
    re.DOTALL,
)

FIELD_RE = re.compile(
    r"^[ \t]*(?:///.*\n[ \t]*)*pub (?P<name>\w+|r#\w+): (?P<type>[^,\n]+),",
    re.MULTILINE,
)


def find_defaults_in_file(text: str, struct_name: str) -> dict:
    """Scan the file for `<field>: <field>.unwrap_or(EXPR)` and
    `<field>: <field>.unwrap_or_default()` lines INSIDE a known struct's
    decoder, returning {field_name: default_string}.

    Defaults are matched conservatively: only lines that read like decoder
    field-resolution returns. We can't easily tie a match to a specific
    struct without parsing the AST, so we anchor on the impl block boundary
    by string search for `impl <Struct> {` and the next `pub fn` after it.
    For simplicity we scan the entire `impl <Struct> { ... }` block.
    """
    defaults: dict = {}
    impl_marker = f"impl {struct_name} "
    start = text.find(impl_marker)
    if start < 0:
        return defaults
    depth = 0
    i = text.find("{", start)
    if i < 0:
        return defaults
    j = i
    while j < len(text):
        c = text[j]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                break
        j += 1
    body = text[i + 1 : j]
    # `<field>: <field>.unwrap_or_default()` → empty Vec / default trait.
    for m in re.finditer(
        r"(?m)^\s*(?P<field>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?P=field)\.unwrap_or_default\(\)",
        body,
    ):
        defaults[m.group("field")] = "default"
    # Block-form `<field>: { let v: ... = <field>.unwrap_or_default(); ... }` —
    # used by Table.row_actions/bulk_actions and similar wrappers.
    for m in re.finditer(
        r"(?P<field>[A-Za-z_][A-Za-z0-9_]*)\.unwrap_or_default\(\)",
        body,
    ):
        # Only count if the same name appears in a `<field>:` outer key earlier
        # in the same struct literal — a cheap proxy is to check the name is
        # a known local binding (filter false positives later via field list).
        defaults.setdefault(m.group("field"), "default")
    # `<field>: <field>.unwrap_or(EXPR)` — capture EXPR, balance one paren.
    for m in re.finditer(
        r"(?m)^\s*(?P<field>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?P=field)\.unwrap_or\(",
        body,
    ):
        # Walk parens to find matching close.
        p = m.end()
        depth_p = 1
        while p < len(body) and depth_p > 0:
            if body[p] == "(":
                depth_p += 1
            elif body[p] == ")":
                depth_p -= 1
                if depth_p == 0:
                    break
            p += 1
        expr = body[m.end():p].strip()
        defaults[m.group("field")] = expr
    # `<field>: <field>.unwrap_or_else(|| EXPR)` — capture EXPR after `||`.
    for m in re.finditer(
        r"(?m)^\s*(?P<field>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?P=field)\.unwrap_or_else\(\|\|\s*",
        body,
    ):
        p = m.end()
        depth_p = 1
        while p < len(body) and depth_p > 0:
            if body[p] == "(":
                depth_p += 1
            elif body[p] == ")":
                depth_p -= 1
                if depth_p == 0:
                    break
            p += 1
        expr = body[m.end():p].strip().rstrip(",").strip()
        # Filter out `ok_or_else(|| missing_field(...))` which is the
        # required-field error path, not a default.
        if "missing_field" in expr:
            continue
        defaults[m.group("field")] = expr
    return defaults


def parse_component(text: str, src_path: Path):
    """Yield (struct_name, tag, [(field_name, field_type, doc), ...]) tuples."""
    for m in COMPONENT_RE.finditer(text):
        name = m.group("name")
        tag = m.group("tag")
        body = m.group("body")
        defaults = find_defaults_in_file(text, name)
        handlers = HANDLERS_OVERRIDES.get(name, ())
        # Split body line-by-line to attach preceding doc comments.
        fields = []
        cur_doc = []
        # Field key index is positional — they follow source order, which
        # matches the wire `entries.push((<key>, ...))` pattern by convention.
        idx = 0
        for line in body.split("\n"):
            stripped = line.strip()
            if stripped.startswith("///"):
                cur_doc.append(stripped[3:].strip())
                continue
            fm = re.match(r"pub (r#)?(\w+):\s*(.+?),\s*$", stripped)
            if not fm:
                if stripped:
                    cur_doc = []
                continue
            fname = fm.group(2)
            ftype = fm.group(3).strip()
            doc = " ".join(cur_doc)
            cur_doc = []
            optional = ftype.startswith("Option<")
            wire = rust_type_to_wire(ftype, doc)
            disp_name = fname[2:] if fname.startswith("r#") else fname
            wire = apply_overrides(name, disp_name, wire)
            default = defaults.get(disp_name)
            # Manual overrides for indirect default patterns the parser misses.
            if (name, disp_name) in DEFAULTS_OVERRIDES:
                default = DEFAULTS_OVERRIDES[(name, disp_name)]
            # `required` = field MUST appear on wire (no default-on-decode,
            # not an Option<T>). Default-on-decode demotes to optional+default.
            required = (not optional) and (default is None)
            fields.append((idx, fname, wire, required, default))
            idx += 1
        yield (name, tag, fields, handlers)


def section_for(file_path: Path) -> str:
    for k in SECTIONS:
        if f"/{k}/" in str(file_path):
            return SECTIONS[k]
    raise SystemExit(f"unknown section for {file_path}")


def collect(repo: Path):
    discover_enums(repo)
    discover_inline_structs(repo)
    components = []
    for sub in SECTIONS:
        for p in sorted((repo / "src/protocol/ui" / sub).glob("*.rs")):
            if p.name == "mod.rs":
                continue
            text = p.read_text()
            for name, tag, fields, handlers in parse_component(text, p):
                components.append({
                    "name": name,
                    "tag": tag,
                    "section": section_for(p),
                    "fields": fields,
                    "handlers": handlers,
                })
                COMPONENT_TAGS[name] = tag
    return components


def emit(components, out_path: Path):
    lines = [
        "// =============================================================================",
        "// File: protocol/ui/schema/data.rs — codegen schema data (auto-generated)",
        "// Generated by scripts/gen_schema.py from typed component source files.",
        "// DO NOT EDIT BY HAND — re-run the generator after struct edits.",
        "// =============================================================================",
        "",
        "use super::types::{ComponentMeta, FieldMeta, section};",
        "",
    ]
    consts = []
    for c in components:
        const_name = f"{c['name'].upper()}_SCHEMA"
        consts.append(const_name)
        lines.append(f"pub const {const_name}: ComponentMeta = ComponentMeta {{")
        lines.append(f"    tag: {c['tag']},")
        lines.append(f"    name: \"{c['name']}\",")
        lines.append(f"    section: section::{c['section']},")
        if not c["fields"]:
            lines.append(f"    fields: &[],")
        else:
            lines.append(f"    fields: &[")
            for (idx, fname, wire, required, default) in c["fields"]:
                # Strip r# raw-identifier prefix from field display name.
                disp = fname[2:] if fname.startswith("r#") else fname
                req = "true" if required else "false"
                if default is None:
                    dflt = "None"
                else:
                    escaped = default.replace("\\", "\\\\").replace("\"", "\\\"")
                    dflt = f"Some(\"{escaped}\")"
                lines.append(
                    f"        FieldMeta {{ key: {idx}, name: \"{disp}\", wire: \"{wire}\", "
                    f"required: {req}, default: {dflt} }},"
                )
            lines.append(f"    ],")
        if c["handlers"]:
            hs = ", ".join(f'"{h}"' for h in c["handlers"])
            lines.append(f"    handlers: &[{hs}],")
        else:
            lines.append(f"    handlers: &[],")
        lines.append(f"}};")
        lines.append("")
    lines.append("/// All catalog components (ordered by file scan order).")
    lines.append("pub const ALL_COMPONENTS: &[&ComponentMeta] = &[")
    for c in components:
        lines.append(f"    &{c['name'].upper()}_SCHEMA,")
    lines.append("];")
    lines.append("")
    out_path.write_text("\n".join(lines))


if __name__ == "__main__":
    repo = Path(".")
    components = collect(repo)
    emit(components, repo / "src/protocol/ui/schema/data.rs")
    print(f"Emitted {len(components)} component schemas")
