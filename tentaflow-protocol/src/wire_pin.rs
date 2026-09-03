// =============================================================================
// File: wire_pin.rs
// Purpose: One source-reading pin for every CBOR payload module. ciborium tags
//          by NAME and encodes by TYPE, so the wire contract of a module is the
//          set of its declarations: variant names and order, field names, field
//          types and the serde attributes that rewrite either of them. A
//          round-trip test cannot see a rename — it re-encodes with the new
//          name — so each payload module hands its own source text to these
//          helpers and pins the digest of what they find.
// Example: let names = wire_pin::payload_variants(SOURCE, "TentaVmPayload");
//          assert_eq!(wire_pin::name_digest(&names), 0x…);
// =============================================================================

/// FNV-1a 64 over the entries joined by newlines. A hash, not a stored list:
/// one cheap assertion that fails on ANY rename, reorder or type change, and
/// the caller prints the live entries so the diff is one glance away.
pub fn name_digest(entries: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in entries.join("\n").as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hex text to bytes, for the byte goldens.
pub fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// One declaration line reduced to what the wire cares about: attributes
/// collapsed to a single spaceless token each, then the declaration itself with
/// its whitespace normalized. `#[serde(rename = "x")]` and `#[serde(skip)]`
/// therefore travel into the digest instead of being skipped, which is the
/// whole point — both rewrite the wire while every name stays put.
fn normalize(attrs: &[String], decl: &str) -> String {
    let mut out = String::new();
    for attr in attrs {
        out.push_str(&attr.split_whitespace().collect::<String>());
        out.push(' ');
    }
    let decl: String = decl.split_whitespace().collect::<Vec<_>>().join(" ");
    out.push_str(&decl);
    out
}

/// True for a line that is a comment or blank — never part of the contract.
fn is_ignorable(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with("//")
}

/// Every variant of `enum <enum_name>` in `source`, in declaration order, each
/// rendered as its attributes + name + its own fields (with their attributes
/// and types). Variant names alone are not the contract: a payload variant is a
/// struct on the wire, so renaming one of its fields breaks the browser exactly
/// like renaming the variant.
///
/// Panics when the enum is absent, so a renamed payload enum fails loudly
/// instead of pinning an empty list.
pub fn payload_variants(source: &str, enum_name: &str) -> Vec<String> {
    let header = format!("pub enum {enum_name} {{");
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with(&header))
        .unwrap_or_else(|| panic!("enum '{enum_name}' not found — did it get renamed?"));

    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = start + 1;
    // The enum is declared at the top level, so its closing brace is the next
    // bare `}`. Reading to that instead of counting braces keeps a doc comment
    // that happens to contain one from derailing the scan.
    while i < lines.len() && lines[i] != "}" {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.starts_with("#[") {
            attrs.push(trimmed.to_string());
            i += 1;
            continue;
        }
        let is_variant = line.starts_with("    ")
            && !line.starts_with("     ")
            && trimmed.starts_with(|c: char| c.is_ascii_uppercase());
        if is_variant {
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let mut entry = normalize(&attrs, &name);
            attrs.clear();
            // A struct-like variant carries its own fields; a unit or tuple
            // variant is complete on its declaration line.
            if trimmed.ends_with('{') {
                let (fields, end) = read_fields(&lines, i + 1, "        ");
                for field in fields {
                    entry.push_str(" | ");
                    entry.push_str(&field);
                }
                i = end;
            }
            out.push(entry);
        } else if !is_ignorable(line) && !trimmed.starts_with("///") {
            attrs.clear();
        }
        i += 1;
    }
    out
}

/// Reads a field block indented by `indent` until its closing brace, returning
/// the normalized fields and the index of that closing line. Fields must be
/// `pub`-visible where `indent` is a struct body; inside an enum variant every
/// field is public by construction, so both shapes are read the same way.
fn read_fields(lines: &[&str], from: usize, indent: &str) -> (Vec<String>, usize) {
    let closing = format!("{}{}", &indent[..indent.len() - 4], "}");
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = from;
    while i < lines.len() {
        let line = lines[i];
        if line == closing || line == format!("{closing},") {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("#[") {
            attrs.push(trimmed.to_string());
            i += 1;
            continue;
        }
        if !is_ignorable(line) && !trimmed.starts_with("///") {
            let decl = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
            let decl = decl.strip_suffix(',').unwrap_or_else(|| {
                panic!("field line does not end in a comma: '{trimmed}'")
            });
            assert!(
                decl.contains(':'),
                "field line has no type: '{trimmed}' — a wrapped declaration would reach the \
                 digest truncated"
            );
            out.push(normalize(&attrs, decl));
            attrs.clear();
        }
        i += 1;
    }
    (out, i)
}

/// Every `pub struct` of `source` with its `pub` fields as attributes +
/// `name: Type`, in declaration order — the payload structs the browser decodes
/// by name and the wire encodes by type.
pub fn wire_structs(source: &str) -> Vec<(String, Vec<String>)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(rest) = line.strip_prefix("pub struct ") {
            if line.trim_end().ends_with('{') {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let (fields, end) = read_fields(&lines, i + 1, "    ");
                out.push((name, fields));
                i = end;
            }
        }
        i += 1;
    }
    out
}

/// The assumptions the two parsers rest on, proven for `source` instead of
/// trusted. Every wire struct must be brace-shaped (a tuple struct would be
/// invisible to `wire_structs` and still serialize) and every one of its fields
/// must be `pub` (a private field would still serialize and never reach the
/// digest). `wire_structs` itself panics on a field that wraps across lines or
/// carries no type, so calling it here covers those two as well.
pub fn assert_parseable(source: &str) {
    let structs = wire_structs(source);
    assert!(!structs.is_empty(), "the parser found no wire structs at all");

    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("pub struct ") {
            assert!(
                line.trim_end().ends_with('{'),
                "wire struct is not brace-shaped, so the pin cannot see its fields: '{line}'"
            );
            let mut j = i + 1;
            while j < lines.len() && lines[j] != "}" {
                let body = lines[j];
                let trimmed = body.trim();
                let skippable = trimmed.is_empty()
                    || trimmed.starts_with("//")
                    || trimmed.starts_with("#[");
                assert!(
                    skippable || body.starts_with("    pub "),
                    "non-pub field in a wire struct is invisible to the pin: '{body}'"
                );
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
}
