"""
Parse tagged-union enum definitions and emit UnionMeta consts.

Reads `pub enum X { Variant1, Variant2 { f: T, ... }, ... }` blocks across:
  src/protocol/control.rs
  src/protocol/ui/action.rs
  src/protocol/ui/bind.rs
  src/protocol/ui/form/wrappers.rs
  src/protocol/ui/handler.rs
  src/protocol/ui/inline.rs
  src/protocol/ui/patch.rs
  src/protocol/ui/validation.rs
  src/protocol/ui/value_format.rs

For each enum:
  - Extracts variant names + fields ({ field: rust_type, ... } body).
  - Inspects the matching `impl<C> Encode<C> for X` block to map
    `X::Variant` → `e.str("kind")?.str("<wire_kind>")` discriminator value.
  - Emits a UnionMeta const + an `ALL_TAGGED_UNIONS` slice entry.

Output: src/protocol/ui/schema/data_unions.rs
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Iterable

# Mirror of TYPE_ALIASES + PRIMS from gen_schema.py so we keep wire-strings
# consistent between component/inline metadata and union metadata.
PRIMS = {
    "bool", "u8", "u16", "u32", "u64", "i32", "i64", "f64",
}
TYPE_ALIASES = {
    "LiveRegionPoliteness": "LiveRegion",
}
# String-enum names discovered live in gen_schema.py; we duplicate the
# discovery here to avoid pulling that script in.
ENUMS: set[str] = set()
# Inline struct + tagged-union names referenced as Inline<X>.
INLINE_KNOWN: set[str] = set()

# Explicit `wire_kind` overrides for unions whose Encode impl does not use
# the `e.str("kind")?.str("X")` inline pattern that `_parse_encode_kinds`
# recognises (e.g. computed helper methods or `e.str("kind")?; match { ... }`).
# Anything declared here bypasses the parser's autodiscovery.
WIRE_KIND_OVERRIDES: dict[tuple[str, str], str] = {
    # SelectValue uses `e.str("kind")?; match { ... e.str("X") }` pattern
    # (kind tag emitted separately from the literal).
    ("SelectValue", "Text"): "tstr",
    ("SelectValue", "UInt"): "u32",
    ("SelectValue", "Int"): "i32",
    ("SelectValue", "Bool"): "bool",
    # PathSegment same pattern as SelectValue.
    ("PathSegment", "Key"): "key",
    ("PathSegment", "Index"): "index",
    # AspectRatio emits via a computed `wire_kind()` const helper method.
    ("AspectRatio", "R1To1"):  "1:1",
    ("AspectRatio", "R16To9"): "16:9",
    ("AspectRatio", "R4To3"):  "4:3",
    ("AspectRatio", "R21To9"): "21:9",
    ("AspectRatio", "R3To2"):  "3:2",
    ("AspectRatio", "R2To1"):  "2:1",
    ("AspectRatio", "R9To16"): "9:16",
    ("AspectRatio", "R3To4"):  "3:4",
    ("AspectRatio", "Custom"): "custom",
}

# Tuple variants don't have field names in Rust source. Each (union, variant)
# here lists the actual CBOR map field name(s) used by the Encode impl,
# paired with the wire-type override for that payload slot.
# Format: { (union, variant): [(field_name, wire_type), ...] }
TUPLE_FIELD_OVERRIDES: dict[tuple[str, str], list[tuple[str, str]]] = {
    ("PathSegment", "Key"):     [("value", "tstr")],
    ("PathSegment", "Index"):   [("value", "u32")],
    ("BindRef", "Literal"):     [("value", "Value")],
    ("BindRef", "Bound"):       [("path", "StatePath")],
    ("Handler", "Local"):       [("action", "Inline<LocalAction>")],
    ("SelectValue", "Text"):    [("value", "tstr")],
    ("SelectValue", "UInt"):    [("value", "u32")],
    ("SelectValue", "Int"):     [("value", "i32")],
    ("SelectValue", "Bool"):    [("value", "bool")],
}

# Whitelist of tagged-union types to capture. Anything else is a derived
# `#[derive(Encode, Decode)]` struct/enum and lives in ALL_INLINE_STRUCTS.
TAGGED_UNIONS = [
    # (rust name, source path relative to crate root)
    ("ResumeStatus",      "src/protocol/control.rs"),
    ("RejectReason",      "src/protocol/control.rs"),
    ("RateLimitScope",    "src/protocol/control.rs"),
    ("ActionStatus",      "src/protocol/ui/action.rs"),
    ("PathSegment",       "src/protocol/ui/bind.rs"),
    ("BindRef",           "src/protocol/ui/bind.rs"),
    ("BindSpec",          "src/protocol/ui/bind.rs"),
    ("FormValidator",     "src/protocol/ui/form/wrappers.rs"),
    ("FailurePolicy",     "src/protocol/ui/handler.rs"),
    ("LocalAction",       "src/protocol/ui/handler.rs"),
    ("Handler",           "src/protocol/ui/handler.rs"),
    ("IconRef",           "src/protocol/ui/inline.rs"),
    ("AvatarRef",         "src/protocol/ui/inline.rs"),
    ("SelectValue",       "src/protocol/ui/inline.rs"),
    ("DimensionToken",    "src/protocol/ui/inline.rs"),
    ("AspectRatio",       "src/protocol/ui/inline.rs"),
    ("TableColumnWidth",  "src/protocol/ui/inline.rs"),
    ("HeatmapScale",      "src/protocol/ui/inline.rs"),
    ("DatePresetResolve", "src/protocol/ui/inline.rs"),
    ("BorderToken",       "src/protocol/ui/inline.rs"),
    ("SpaceValue",        "src/protocol/ui/inline.rs"),
    ("RadiusValue",       "src/protocol/ui/inline.rs"),
    ("SplitSize",         "src/protocol/ui/inline.rs"),
    ("GridCol",           "src/protocol/ui/inline.rs"),
    ("GridTrack",         "src/protocol/ui/inline.rs"),
    ("PatchOpKind",       "src/protocol/ui/patch.rs"),
    ("ValidationRule",    "src/protocol/ui/validation.rs"),
    ("StateCondition",    "src/protocol/ui/validation.rs"),
    ("ValueFormat",       "src/protocol/ui/value_format.rs"),
]


def _discover_known_types(repo: Path) -> None:
    """Populate ENUMS + INLINE_KNOWN to mirror gen_schema.py classification."""
    _string_enum_re = re.compile(
        r"string_enum!\s*\{(?:\s*///[^\n]*\n)*\s*pub enum (\w+) \{",
        re.DOTALL,
    )
    for src in [
        "src/protocol/ui/tokens.rs",
        "src/protocol/ui/value_format.rs",
        "src/protocol/ui/inline.rs",
        "src/protocol/ui/icon_name.rs",
        "src/protocol/control.rs",
    ]:
        for m in _string_enum_re.finditer((repo / src).read_text()):
            ENUMS.add(m.group(1))
    # `#[cbor(map)] pub struct X` blocks (treat as inline-known regardless of
    # which derive they use).
    for m in re.finditer(
        r"#\[cbor\(map\)\]\s*pub struct (\w+) ",
        (repo / "src/protocol/ui/inline.rs").read_text(),
    ):
        INLINE_KNOWN.add(m.group(1))
    # Tagged unions are themselves Inline<X> targets — preload from our whitelist.
    for (name, _) in TAGGED_UNIONS:
        INLINE_KNOWN.add(name)


def rust_type_to_wire(rust: str) -> str:
    rust = rust.strip()
    if rust.startswith("&"):
        rust = rust[1:].strip()
    rust = re.sub(r"(?:[A-Za-z_][A-Za-z0-9_]*::)+([A-Za-z_][A-Za-z0-9_]*)", r"\1", rust)
    for alias, real in TYPE_ALIASES.items():
        rust = re.sub(rf"\b{re.escape(alias)}\b", real, rust)
    if rust.startswith("Option<") and rust.endswith(">"):
        return f"Option<{rust_type_to_wire(rust[len('Option<'):-1])}>"
    if rust.startswith("Vec<") and rust.endswith(">"):
        return f"Array<{rust_type_to_wire(rust[len('Vec<'):-1])}>"
    if rust.startswith("Box<") and rust.endswith(">"):
        return rust_type_to_wire(rust[len("Box<"):-1])
    if rust == "String":
        return "tstr"
    if rust == "BindRef":
        return "BindRef"
    if rust == "StatePath":
        return "StatePath"
    if rust == "Value":
        return "Value"
    if rust == "CborMap":
        return "CborMap"
    if rust in PRIMS:
        return rust
    if rust in ENUMS:
        return f"Enum<{rust}>"
    return f"Inline<{rust}>"


def _find_enum_block(text: str, name: str) -> tuple[int, int]:
    """Return (start, end) byte offsets of the enum body INSIDE the braces."""
    m = re.search(rf"pub enum {re.escape(name)} \{{", text)
    if not m:
        raise SystemExit(f"enum {name} not found")
    depth = 0
    i = m.end() - 1
    body_start = i + 1
    while i < len(text):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return (body_start, i)
        i += 1
    raise SystemExit(f"enum {name}: unbalanced braces")


def _parse_variant_header(line: str) -> tuple[str, str] | None:
    """Return (variant_name, opener) where opener ∈ {"unit", "{", "("}, or None."""
    m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)\s*(\{|\(|,|$)", line.lstrip())
    if not m:
        return None
    nm = m.group(1)
    op = m.group(2)
    if op == "{":
        return (nm, "{")
    if op == "(":
        return (nm, "(")
    return (nm, "unit")


def _parse_enum_variants(text: str, body: str) -> list[tuple[str, list[tuple[str, str]]]]:
    """Return list of (variant_name, [(field_name, rust_type), ...]).

    Tuple variants are normalised to `[("0", rust_type)]` (synthetic name `0`).
    """
    out: list[tuple[str, list[tuple[str, str]]]] = []
    pos = 0
    while pos < len(body):
        # Skip whitespace + doc comments + attrs.
        while pos < len(body):
            ch = body[pos]
            if ch in " \t\n":
                pos += 1
                continue
            if body[pos:pos + 3] == "///":
                nl = body.find("\n", pos)
                pos = nl + 1 if nl >= 0 else len(body)
                continue
            if ch == "#":
                # Skip attribute line (could span multiple lines).
                end = body.find("\n", pos)
                pos = end + 1 if end >= 0 else len(body)
                continue
            break
        if pos >= len(body):
            break
        m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", body[pos:])
        if not m:
            break
        name = m.group(1)
        pos += len(name)
        # Determine variant shape.
        while pos < len(body) and body[pos] in " \t":
            pos += 1
        if pos >= len(body) or body[pos] == ",":
            out.append((name, []))
            pos += 1
            continue
        if body[pos] == "{":
            # Walk to matching '}'.
            depth = 1
            start = pos + 1
            pos += 1
            while pos < len(body) and depth > 0:
                if body[pos] == "{":
                    depth += 1
                elif body[pos] == "}":
                    depth -= 1
                pos += 1
            fields_body = body[start:pos - 1]
            fields = _parse_variant_fields(fields_body)
            # Eat optional trailing comma.
            while pos < len(body) and body[pos] in " \t\n,":
                pos += 1
            out.append((name, fields))
            continue
        if body[pos] == "(":
            depth = 1
            start = pos + 1
            pos += 1
            while pos < len(body) and depth > 0:
                if body[pos] == "(":
                    depth += 1
                elif body[pos] == ")":
                    depth -= 1
                pos += 1
            inner = body[start:pos - 1].strip()
            while pos < len(body) and body[pos] in " \t\n,":
                pos += 1
            # Tuple variants are uncommon — record as "0" → type.
            out.append((name, [("0", inner)]))
            continue
        # Unknown char — advance defensively.
        pos += 1
    return out


def _parse_variant_fields(body: str) -> list[tuple[str, str]]:
    """Parse `field_a: TypeA, field_b: TypeB,` body into [(name, type), ...]."""
    fields: list[tuple[str, str]] = []
    # Strip doc comments / attributes from body first.
    lines = []
    for ln in body.split("\n"):
        s = ln.strip()
        if not s or s.startswith("///") or s.startswith("#"):
            continue
        lines.append(s)
    joined = " ".join(lines)
    # Split on top-level commas (respecting nested <>/()).
    items: list[str] = []
    depth = 0
    buf = []
    for ch in joined:
        if ch in "<(":
            depth += 1
        elif ch in ">)":
            depth -= 1
        if ch == "," and depth == 0:
            items.append("".join(buf).strip())
            buf = []
        else:
            buf.append(ch)
    if buf:
        last = "".join(buf).strip()
        if last:
            items.append(last)
    for it in items:
        if ":" not in it:
            continue
        nm, ty = it.split(":", 1)
        fields.append((nm.strip(), ty.strip()))
    return fields


def _parse_encode_kinds(text: str, name: str) -> dict[str, str]:
    """Return {variant_name: wire_kind_str} by parsing the Encode impl block.

    Matches patterns like:
      X::Variant { ... } => { ... e.str("kind")?.str("<wire>")? ... }
      X::Variant(...)    => { ... e.str("kind")?.str("<wire>")? ... }
    """
    out: dict[str, str] = {}
    # Look at the body of the Encode impl.
    impl_re = re.compile(
        rf"impl<C> Encode<C> for {re.escape(name)} \{{",
    )
    m = impl_re.search(text)
    if not m:
        return out
    depth = 0
    i = m.end() - 1
    while i < len(text):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                break
        i += 1
    block = text[m.end():i]
    # Per-arm scan: walk each `Name::Variant ... => { ... }` arm and look
    # ONLY inside that arm body for either an inline `.str("kind")?.str("X")`
    # or a helper call like `emit_single_tstr(e, "X", ...)` /
    # `emit_unit_kind(e, "X")`. Fall back to snake_case if nothing matches.
    arm_re = re.compile(
        rf"{re.escape(name)}::(?P<variant>[A-Za-z_][A-Za-z0-9_]*)"
        r"\s*(?:\{[^{}=]*\}|\([^()]*\))?\s*=>\s*\{",
    )
    for am in arm_re.finditer(block):
        v = am.group("variant")
        if v in out:
            continue
        # Find matching close of the arm's `{`.
        i = am.end()
        depth = 1
        while i < len(block) and depth > 0:
            ch = block[i]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        arm_body = block[am.end():i]
        # 1) Inline `.str("kind")?.str("X")`.
        km = re.search(r'\.str\("kind"\)\?\s*\.str\("([^"]+)"\)', arm_body)
        if km:
            out[v] = km.group(1)
            continue
        # 2) Helper `emit_single_tstr(<>, "X", ...)` / `emit_unit_kind(<>, "X")`.
        hm = re.search(r'emit_(?:single_tstr|unit_kind|map_kind)\s*\([^,]+,\s*"([^"]+)"', arm_body)
        if hm:
            out[v] = hm.group(1)
            continue
    return out


def parse_union(repo: Path, name: str, rel_path: str) -> tuple[str, list[tuple[str, str, list[tuple[str, str]]]]]:
    """Parse one union → (discriminator_key, [(variant, wire_kind, fields), ...])."""
    text = (repo / rel_path).read_text()
    start, end = _find_enum_block(text, name)
    body = text[start:end]
    variants = _parse_enum_variants(text, body)
    kinds = _parse_encode_kinds(text, name)
    out: list[tuple[str, str, list[tuple[str, str]]]] = []
    for (vn, fs) in variants:
        # Manual overrides win unconditionally (covers computed `wire_kind()`
        # methods, `e.str("kind")?; match { ... }` patterns, etc.).
        kind = WIRE_KIND_OVERRIDES.get((name, vn))
        if kind is None:
            kind = kinds.get(vn)
        if kind is None:
            # No silent fallback — schema corruption masquerades as snake_case.
            raise SystemExit(
                f"gen_unions: cannot determine wire_kind for {name}::{vn} "
                f"(no inline `.str(\"kind\")?.str(...)?`, no helper match, "
                f"no entry in WIRE_KIND_OVERRIDES). Add an override."
            )
        # Tuple variants: replace synthetic ("0", rust_type) with explicit
        # CBOR field name(s) + wire types from TUPLE_FIELD_OVERRIDES.
        # `wire_fields` ends up as a list of (cbor_field_name, wire_string).
        if (name, vn) in TUPLE_FIELD_OVERRIDES:
            wire_fields = list(TUPLE_FIELD_OVERRIDES[(name, vn)])
        elif fs and all(fname == "0" or fname.isdigit() for (fname, _) in fs):
            raise SystemExit(
                f"gen_unions: tuple variant {name}::{vn} has no entry in "
                f"TUPLE_FIELD_OVERRIDES (Rust positional names cannot ship "
                f"as CBOR map keys)."
            )
        else:
            # Struct variant: convert each Rust type to its wire descriptor.
            wire_fields = [(fname, rust_type_to_wire(ftype)) for (fname, ftype) in fs]
        out.append((vn, kind, wire_fields))
    return ("kind", out)


def emit(repo: Path, unions: list[tuple[str, str, list[tuple[str, str, list[tuple[str, str]]]]]], out_path: Path) -> None:
    """unions: list of (name, discriminator_key, variant_meta_list)."""
    lines = [
        "// =============================================================================",
        "// File: protocol/ui/schema/data_unions.rs — tagged-union schema (auto-generated)",
        "// Generated by scripts/gen_unions.py from manual `Encode`/`Decode` impls.",
        "// DO NOT EDIT BY HAND — re-run the generator after enum edits.",
        "// =============================================================================",
        "",
        "use super::types::{FieldMeta, UnionMeta, VariantMeta};",
        "",
    ]
    union_consts: list[str] = []
    for (name, disc, variants) in unions:
        const_name = f"{name.upper()}_UNION"
        union_consts.append(const_name)
        # Emit per-variant FieldMeta slices as nested anonymous slices to
        # avoid threading more named consts. They're inlined via &[...].
        lines.append(f"pub const {const_name}: UnionMeta = UnionMeta {{")
        lines.append(f"    name: \"{name}\",")
        lines.append(f"    discriminator_key: \"{disc}\",")
        lines.append(f"    variants: &[")
        for (vn, kind, fields) in variants:
            lines.append(f"        VariantMeta {{")
            lines.append(f"            rust_name: \"{vn}\",")
            lines.append(f"            wire_kind: \"{kind}\",")
            if not fields:
                lines.append(f"            fields: &[],")
            else:
                lines.append(f"            fields: &[")
                for (idx, (fname, wire)) in enumerate(fields):
                    optional = wire.startswith("Option<")
                    req = "false" if optional else "true"
                    # Tagged-union variants don't carry integer keys — they
                    # are tstr-discriminated maps. Reuse FieldMeta.key as a
                    # positional source ordering for codegen.
                    lines.append(
                        f"                FieldMeta {{ key: {idx}, name: \"{fname}\", "
                        f"wire: \"{wire}\", required: {req}, default: None }},"
                    )
                lines.append(f"            ],")
            lines.append(f"        }},")
        lines.append(f"    ],")
        lines.append(f"}};")
        lines.append("")
    lines.append("/// All tagged unions captured by the schema registry.")
    lines.append("pub const ALL_TAGGED_UNIONS: &[&UnionMeta] = &[")
    for cn in union_consts:
        lines.append(f"    &{cn},")
    lines.append("];")
    lines.append("")
    out_path.write_text("\n".join(lines))


def main() -> None:
    repo = Path(".")
    _discover_known_types(repo)
    unions = []
    for (name, rel_path) in TAGGED_UNIONS:
        disc, variants = parse_union(repo, name, rel_path)
        unions.append((name, disc, variants))
    emit(repo, unions, repo / "src/protocol/ui/schema/data_unions.rs")
    total_variants = sum(len(v) for (_, _, v) in unions)
    print(f"Emitted {len(unions)} tagged unions, {total_variants} variants total")


if __name__ == "__main__":
    main()
