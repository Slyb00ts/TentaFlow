// =============================================================================
// File: wire_pin.rs
// Purpose: One source-reading pin, shared by the payload modules that opt in.
//          Today that is `tentavm.rs`, `features.rs` and `code_studio.rs` —
//          three of the twenty-two modules in this crate. `tentanas.rs`,
//          `message_body.rs`, `types.rs`, `mesh.rs` and the rest are NOT
//          pinned; that is a statement of where the tool has been wired in so
//          far, not a rule anything enforces.
//
//          ciborium tags by NAME and encodes by TYPE, so the wire contract of
//          a module is the set of its declarations. A round-trip test cannot
//          see a rename — it re-encodes with the new name — so a module that
//          opts in hands its own source text to these helpers and pins the
//          digest of what they find.
//
//          COMMENTS ARE REMOVED FIRST, ONCE, by `read_source`, and every rule
//          below reads the line that is left. Nothing else in this file asks
//          whether a line "is a comment", because that question and the
//          question "is this line a declaration" are not the same one: a line
//          that ends in `/* … */` is both a comment and a field, and the
//          version that answered the first question dropped the field from the
//          digest, from the variant recount, from the empty-body proof and
//          from the every-field-is-`pub` rule at the same time.
//
//          THE PARSER FAILS CLOSED. After comments are gone, every line inside
//          a pinned item must be classifiable — an attribute, a field, a
//          variant, the closing brace — and a line that is none of those PANICS
//          with the line quoted. Skipping the unrecognised is what let a
//          lowercase variant through four rounds running; the digest may only
//          shrink when a human deletes something, never because the parser did
//          not understand a shape.
//
//          WHAT THE DIGEST COVERS: for every `pub struct` and every `pub enum`
//          declared at the top level of the file, the item's own attributes
//          and — in declaration ORDER — each field as
//          `attributes + name: Type`, or each variant as `attributes + Name`
//          followed by ` | ` and each of its fields. A struct-like variant
//          with an EMPTY body gets the marker ` | {}`, so it cannot collide
//          with the unit variant of the same name, which serde encodes
//          differently. Item attributes are in there because
//          `#[serde(rename_all)]`, `tag`, `untagged` and `transparent` rewrite
//          every key of an item at once, from above it.
//
//          The item's NAME is deliberately NOT part of `entries()`: each module
//          pins the list of names separately, so that reordering two struct
//          blocks reports as an order change rather than as a changed shape.
//
//          WHAT IT DOES NOT COVER. This list is what `assert_parseable` could
//          not turn into a proof; it is not a guarantee of completeness, and
//          the fail-closed rule above exists precisely because such a list has
//          been wrong before:
//          - declarations that are not at the top level of the file: another
//            file, or a nested `pub mod` in this one. `FeatureState` lives in
//            `features.rs` and is pinned by THAT module's test; a module's pin
//            never reaches past its own source. Level is read off the line's
//            INDENT, so a top-level item written indented — legal Rust, never
//            written by rustfmt — is skipped with it;
//          - anything that is not a `pub struct` or `pub enum` — type aliases
//            (`pub type VmRole = String;` is invisible), `impl` blocks,
//            constants;
//          - what a FIELD's type does on the wire once you follow it: the pin
//            records the text `Vec<VmEngine>`, not what `VmEngine` encodes to;
//          - values. Byte goldens in each module pin a handful of concrete
//            encodings; the digest pins declarations only;
//          - text inside a string literal, which is read as if it were code:
//            a fixture line beginning `pub struct ` in column 0 is collected
//            as a wire item. `read_source` tracks where literals START and END
//            — that part is not a limit, and a `/*` inside a fixture no longer
//            comments the file out — but it does not blank what is between
//            them, so a fixture that LOOKS like a declaration reads like one.
//            The failure is loud (an extra item breaks the pinned count), and
//            it is listed here because a module whose fixtures are Rust source
//            is the one place it can happen.
// Example: let items = wire_pin::wire_structs(SOURCE);
//          assert_eq!(wire_pin::name_digest(&items[0].entries()), 0x…);
// =============================================================================

/// One `pub struct` or `pub enum` of a payload module, reduced to what the wire
/// cares about.
pub struct WireItem {
    /// The bare identifier, for looking the item up in a pinned table.
    pub name: String,
    /// Attributes written ABOVE the item, normalized one per entry.
    pub attrs: Vec<String>,
    /// Fields of a struct, or variants of an enum, in declaration order.
    pub members: Vec<String>,
}

impl WireItem {
    /// Everything about this item that the wire can see, in one list ready to
    /// digest: its own attributes first, then its members.
    pub fn entries(&self) -> Vec<String> {
        let mut out = self.attrs.clone();
        out.extend(self.members.iter().cloned());
        out
    }
}

/// FNV-1a 64 over the entries joined by newlines. A hash, not a stored list:
/// one cheap assertion that fails on ANY rename, reorder, type change or
/// attribute change, and the caller prints the live entries so the diff is one
/// glance away.
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

/// One source line in the two forms the parser needs: `code` is the line with
/// every comment removed and its tail trimmed, `raw` is what the human wrote.
/// Every rule classifies `code`; every panic quotes `raw`, so the message shows
/// the line as it stands in the file.
struct Line {
    code: String,
    raw: String,
}

/// What the scanner is in the middle of when a line ends. All four states cross
/// line boundaries in Rust, so the scanner carries them from line to line —
/// a block comment, a string and a raw string all span lines, and a raw string
/// that the scanner mistook for a plain one hands the rest of the file to the
/// wrong state.
enum Scan {
    Code,
    /// Inside `/* … */`, at this nesting depth. Rust nests block comments.
    Block(usize),
    /// Inside `"…"`, where `\` escapes the next character.
    Text,
    /// Inside `r#…"…"#…` with this many hashes. There are NO escapes here:
    /// `r#"a" /*"#` holds a quote and a comment opener, and both are data.
    RawText(usize),
}

/// The file, line by line, with comments removed: `//` to the end of the line,
/// and `/* … */` including the nesting Rust allows and the spans that cross
/// lines. Neither form starts inside a string literal, because
/// `#[serde(rename = "a//b")]` is a wire key and the text after `//` is part
/// of it.
///
/// Literals are recognised the way rustc recognises them, which is the whole
/// reason `Scan` exists: a plain string ends at the first unescaped quote, a
/// raw string only at a quote followed by its own number of hashes, and a char
/// literal may BE a quote (`'"'`). Reading `r#"a" /*"#` as a plain string
/// leaves the scanner outside a literal it is still inside, and the next `/*`
/// then comments out real declarations — silently, because the count of items
/// the pin expects is the count it finds.
///
/// Indentation is preserved, because the collectors recognise fields and
/// variants by it, and one output line is produced for every input line, so
/// indices still address the file. A line that held nothing but a comment
/// comes out empty — that, and only that, is what `is_ignorable` then means.
fn read_source(source: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut state = Scan::Code;
    for raw in source.lines() {
        let chars: Vec<char> = raw.chars().collect();
        let mut code = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match state {
                Scan::Block(depth) => {
                    if c == '/' && chars.get(i + 1) == Some(&'*') {
                        state = Scan::Block(depth + 1);
                        i += 2;
                    } else if c == '*' && chars.get(i + 1) == Some(&'/') {
                        state = if depth == 1 {
                            Scan::Code
                        } else {
                            Scan::Block(depth - 1)
                        };
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                Scan::Text => {
                    code.push(c);
                    if c == '\\' {
                        if let Some(next) = chars.get(i + 1) {
                            code.push(*next);
                            i += 2;
                            continue;
                        }
                    }
                    if c == '"' {
                        state = Scan::Code;
                    }
                    i += 1;
                }
                Scan::RawText(hashes) => {
                    code.push(c);
                    i += 1;
                    if c == '"' {
                        let mut seen = 0;
                        while seen < hashes && chars.get(i + seen) == Some(&'#') {
                            seen += 1;
                        }
                        if seen == hashes {
                            for _ in 0..hashes {
                                code.push('#');
                            }
                            i += hashes;
                            state = Scan::Code;
                        }
                    }
                }
                Scan::Code => {
                    if c == '/' && chars.get(i + 1) == Some(&'/') {
                        break;
                    }
                    if c == '/' && chars.get(i + 1) == Some(&'*') {
                        state = Scan::Block(1);
                        i += 2;
                        continue;
                    }
                    if let Some((len, hashes)) = raw_text_opener(&chars, i) {
                        for c in &chars[i..i + len] {
                            code.push(*c);
                        }
                        state = Scan::RawText(hashes);
                        i += len;
                        continue;
                    }
                    if let Some(len) = char_literal_len(&chars, i) {
                        for c in &chars[i..i + len] {
                            code.push(*c);
                        }
                        i += len;
                        continue;
                    }
                    code.push(c);
                    if c == '"' {
                        state = Scan::Text;
                    }
                    i += 1;
                }
            }
        }
        out.push(Line {
            code: code.trim_end().to_string(),
            raw: raw.to_string(),
        });
    }
    out
}

/// The length of the raw-string opener starting at `at`, and the number of
/// hashes it declares — `r"`, `br"`, `cr#"`, `br##"` and so on — or None when
/// this is not one. Rust has THREE raw prefixes, `r`, `br` and `cr`; a list
/// with two of them is not a shorter list, it is a scanner that walks into a
/// literal it does not know it is in.
///
/// The identifier check is on the character before the whole PREFIX, not
/// before the `r`: `for"` must not open a literal, and `cr#"` must, and the
/// version that looked one character back from the `r` could not tell those
/// apart — it saw the `c` of `cr` and refused.
fn raw_text_opener(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let prefix = match (chars.get(at), chars.get(at + 1)) {
        (Some('r'), _) => 1,
        (Some('b'), Some('r')) | (Some('c'), Some('r')) => 2,
        _ => return None,
    };
    if at > 0 {
        let prev = chars[at - 1];
        if prev.is_alphanumeric() || prev == '_' {
            return None;
        }
    }
    let first_hash = at + prefix;
    let mut i = first_hash;
    while chars.get(i) == Some(&'#') {
        i += 1;
    }
    if chars.get(i) == Some(&'"') {
        Some((i + 1 - at, i - first_hash))
    } else {
        None
    }
}

/// The length of the char literal starting at `at`, or None when the quote
/// opens a lifetime instead. `'"'` is the one that matters here: read as an
/// ordinary quote it would put the scanner inside a string that never ends.
fn char_literal_len(chars: &[char], at: usize) -> Option<usize> {
    if chars.get(at) != Some(&'\'') {
        return None;
    }
    let body = if chars.get(at + 1) == Some(&'\\') {
        2
    } else {
        1
    };
    if chars.get(at + 1 + body) == Some(&'\'') {
        Some(body + 2)
    } else {
        None
    }
}

/// True for a line that has no code left once `read_source` removed its
/// comments: a blank line, or a comment and nothing else. A line that ends in
/// `/* … */` but declares a field is NOT ignorable, and that is the whole
/// difference between this predicate and the one it replaced.
fn is_ignorable(line: &Line) -> bool {
    line.code.is_empty()
}

/// Whitespace removed OUTSIDE literals and kept inside them. Squeezing blindly
/// would map `#[serde(rename = "item Id")]` and `#[serde(rename = "itemId")]`
/// onto the same entry — two different wire keys, one digest — which is the
/// single thing this function must never do.
///
/// It recognises the same three literal forms `read_source` does, and for the
/// same reason: a `"` inside `r#"a"b c"#` is data, and a scanner that toggles
/// on it walks out of the literal and squeezes the key's own spaces away.
fn squeeze(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some((len, hashes)) = raw_text_opener(&chars, i) {
            for c in &chars[i..i + len] {
                out.push(*c);
            }
            i += len;
            while i < chars.len() {
                let c = chars[i];
                out.push(c);
                i += 1;
                if c == '"' {
                    let mut seen = 0;
                    while seen < hashes && chars.get(i + seen) == Some(&'#') {
                        seen += 1;
                    }
                    if seen == hashes {
                        for _ in 0..hashes {
                            out.push('#');
                        }
                        i += hashes;
                        break;
                    }
                }
            }
            continue;
        }
        if let Some(len) = char_literal_len(&chars, i) {
            for c in &chars[i..i + len] {
                out.push(*c);
            }
            i += len;
            continue;
        }
        let c = chars[i];
        i += 1;
        if c == '"' {
            out.push(c);
            while i < chars.len() {
                let c = chars[i];
                out.push(c);
                i += 1;
                if c == '\\' {
                    if let Some(next) = chars.get(i) {
                        out.push(*next);
                        i += 1;
                    }
                    continue;
                }
                if c == '"' {
                    break;
                }
            }
            continue;
        }
        if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// Attributes collapsed to one token each, then the declaration with its own
/// whitespace normalized to single spaces.
fn normalize(attrs: &[String], decl: &str) -> String {
    let mut out = String::new();
    for attr in attrs {
        out.push_str(&squeeze(attr));
        out.push(' ');
    }
    let decl: String = decl.split_whitespace().collect::<Vec<_>>().join(" ");
    out.push_str(&decl);
    out
}

/// Reads a field block indented by `indent` until its closing brace, returning
/// the normalized fields and the index of that closing line. Inside a struct
/// body every field is `pub`; inside an enum variant every field is public by
/// construction, so both shapes are read the same way.
fn read_fields(lines: &[Line], from: usize, indent: &str) -> (Vec<String>, usize) {
    let closing = format!("{}{}", &indent[..indent.len() - 4], "}");
    let closing_comma = format!("{closing},");
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = from;
    while i < lines.len() {
        let code = &lines[i].code;
        if *code == closing || *code == closing_comma {
            break;
        }
        let trimmed = code.trim();
        if trimmed.starts_with("#[") {
            attrs.push(trimmed.to_string());
            i += 1;
            continue;
        }
        if !trimmed.is_empty() {
            let raw = lines[i].raw.trim();
            let decl = trimmed.strip_prefix("pub ").unwrap_or(trimmed);
            let decl = decl
                .strip_suffix(',')
                .unwrap_or_else(|| panic!("field line does not end in a comma: '{raw}'"));
            assert!(
                decl.contains(':'),
                "field line has no type: '{raw}' — a wrapped declaration would reach the \
                 digest truncated"
            );
            out.push(normalize(&attrs, decl));
            attrs.clear();
        }
        i += 1;
    }
    (out, i)
}

/// Reads an enum body from `from` until the bare `}` that closes it, returning
/// each variant as its attributes, its name and — for a struct-like variant —
/// its own fields.
fn read_variants(lines: &[Line], from: usize) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = from;
    while i < lines.len() && lines[i].code != "}" {
        let code = &lines[i].code;
        let trimmed = code.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if trimmed.starts_with("#[") {
            attrs.push(trimmed.to_string());
            i += 1;
            continue;
        }
        // Everything left has to be a variant, and the parser says so out loud
        // instead of skipping what it cannot place. Recognition is by INDENT
        // and SHAPE, never by the case of the first letter: `guestRole`,
        // `_Reserved` and `Żądanie` are all legal Rust variants that ciborium
        // tags by name exactly like `HostRole`.
        let raw = &lines[i].raw;
        assert!(
            code.starts_with("    ") && !code.starts_with("     "),
            "line inside an enum body is not at variant indentation, so the pin cannot \
             classify it: '{raw}'"
        );
        let first = trimmed.chars().next().unwrap_or(' ');
        assert!(
            first.is_alphabetic() || first == '_',
            "line inside an enum body does not begin a variant declaration: '{raw}'"
        );

        // A variant whose brace opens the line carries its fields below; any
        // other variant — unit, tuple, or a struct variant written on one
        // line — is complete on its declaration line, so the WHOLE line goes
        // into the digest.
        let entry = if trimmed.ends_with('{') {
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let mut entry = normalize(&attrs, &name);
            let (fields, end) = read_fields(lines, i + 1, "        ");
            if fields.is_empty() {
                // `One,` and `One { }` are DIFFERENT on the wire — serde emits
                // the bare string for a unit variant and a map for a struct
                // variant — so their entries must differ too. Building the
                // entry from the name alone made them identical, and the
                // variant count identical with it, which is how an empty
                // struct body used to change the wire invisibly.
                entry.push_str(" | {}");
            }
            for field in fields {
                entry.push_str(" | ");
                entry.push_str(&field);
            }
            i = end;
            entry
        } else {
            normalize(&attrs, trimmed.strip_suffix(',').unwrap_or(trimmed))
        };
        attrs.clear();
        out.push(entry);
        i += 1;
    }
    (out, i)
}

/// A second count over the same lines by a second rule: a line indented by
/// exactly four spaces that is not blank, not an attribute and not the closing
/// brace of a struct-like variant. Say plainly what it is and is not: it
/// proves that no line the COLLECTOR could see was dropped by the collector,
/// not that a kept line was read correctly, and it reads the same
/// comment-stripped text `read_variants` reads — a line neither of them sees
/// is a line neither of them counts. A variant the collector skips shows up
/// here as a mismatch; a variant the collector reads into the wrong entry does
/// not, which is why an empty struct body needed a fix in `read_variants` and
/// not another count.
fn count_variant_lines(lines: &[Line], from: usize) -> usize {
    let mut count = 0;
    let mut i = from;
    while i < lines.len() && lines[i].code != "}" {
        let code = &lines[i].code;
        let trimmed = code.trim();
        let structural =
            trimmed.is_empty() || trimmed.starts_with("#[") || trimmed == "}" || trimmed == "},";
        if !structural && code.starts_with("    ") && !code.starts_with("     ") {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Every top-level `pub <keyword>` item of `source`, with the attributes
/// written above it. `pub struct` bodies are read as fields, `pub enum` bodies
/// as variants.
fn items(source: &str, keyword: &str) -> Vec<WireItem> {
    let header = format!("pub {keyword} ");
    let lines = read_source(source);
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // Top level is a property of the LINE, not of the text a comment left
        // behind: `/* new */ pub struct X {` starts in column 0 even though its
        // code no longer does. So the raw line decides the level and the
        // trimmed code decides the shape. An INDENTED declaration is skipped
        // whether it sits in a nested module or — legal Rust, and rustfmt
        // never writes it — indented at the top level of the file; the header
        // lists that as a limit rather than pretending it cannot happen.
        let indented = lines[i].raw.starts_with(' ') || lines[i].raw.starts_with('\t');
        let code = lines[i].code.trim();
        if !indented && code.starts_with("#[") {
            // A wrapped attribute would leave its tail unparsed and its head
            // in `attrs`, so `#[serde(rename_all = …)]` split over two lines
            // could move every key of an item past the digest. Today rustc
            // happens to reject that shape before a `#[derive]`, but that is
            // rustc's rule about derive helpers, not ours, and it says nothing
            // about a non-serde attribute macro.
            let squeezed = squeeze(code);
            assert!(
                squeezed.ends_with(']'),
                "attribute above a wire item does not close on its own line, so the pin \
                 cannot read it: '{}'",
                lines[i].raw
            );
            attrs.push(code.to_string());
            i += 1;
            continue;
        }
        if let Some(rest) = code.strip_prefix(&header).filter(|_| !indented) {
            if code.ends_with('{') {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let normalized: Vec<String> = attrs.iter().map(|a| squeeze(a)).collect();
                let (members, end) = if keyword == "enum" {
                    read_variants(&lines, i + 1)
                } else {
                    read_fields(&lines, i + 1, "    ")
                };
                out.push(WireItem {
                    name,
                    attrs: normalized,
                    members,
                });
                i = end;
            }
        }
        if !is_ignorable(&lines[i]) {
            attrs.clear();
        }
        i += 1;
    }
    out
}

/// Every `pub struct` of `source`: its attributes, its name and its `pub`
/// fields as `attributes + "name: Type"`, in declaration order.
pub fn wire_structs(source: &str) -> Vec<WireItem> {
    items(source, "struct")
}

/// Every `pub enum` of `source`: its attributes, its name and its variants
/// with their own attributes and fields, in declaration order. A payload enum
/// and a domain enum are read the same way, because ciborium encodes them the
/// same way.
pub fn wire_enums(source: &str) -> Vec<WireItem> {
    items(source, "enum")
}

/// Every assumption the parsers rest on, proven for `source` instead of
/// trusted. There are six, and the count is the point: each time one of them
/// was left as a sentence in a comment, a mutation walked through it.
///
/// 1. every wire item is brace-shaped — a tuple or unit `pub struct`/`pub enum`
///    would be invisible to the collectors and still serialize;
/// 2. every field of every wire struct is `pub` — a private field would still
///    serialize and never reach the digest. The walk below reads the same
///    comment-stripped text the collectors read, so a field cannot hide behind
///    a trailing comment from this rule either;
/// 3. every attribute ABOVE THE TEST MODULE is a `derive` or a `serde`
///    attribute, so an attribute macro that rewrites the shape cannot pass for
///    one the digest already understands. Two honest limits: attributes inside
///    a `pub struct` body are not reached by this walk (the field collector
///    puts them in the digest, so adding one is never silent, but a
///    `#[cfg(feature = …)]` on a field still makes the shape depend on the
///    build while the digest claims one shape), and the test module is skipped
///    on purpose — see the comment at `wire_region_end`;
/// 4. every enum body line is classifiable, and the collector kept ALL of the
///    variants: `count_variant_lines` recounts them by a second rule and the
///    two numbers must agree. This is the one that was missing, and the reason
///    a variant named `guestRole` or `_Reserved` used to vanish;
/// 5. a field fits one line and ends in a comma — `read_fields` panics on the
///    rest, so calling the collectors here proves it;
/// 6. a struct-like variant is distinguishable from a unit one: an empty body
///    yields the marker `| {}` instead of a bare name, because serde encodes
///    `One,` and `One {}` differently and the digest has to as well.
///
/// What holds these up is `read_source`: comments are gone before any rule
/// runs, so a line that still has text in it is a declaration and reaches the
/// rule that either classifies it or panics. There is no predicate left that
/// can call a declaration a comment.
///
/// An item attribute split across two lines no longer relies on rustc noticing
/// it: `items()` asserts that an attribute closes on the line it opens.
pub fn assert_parseable(source: &str) {
    let structs = wire_structs(source);
    let enums = wire_enums(source);
    assert!(
        !structs.is_empty() || !enums.is_empty(),
        "the parser found no wire items at all"
    );

    let lines = read_source(source);
    // Everything from `mod tests {` down is test scaffolding: `#[test]`,
    // `#[ignore]`, `#[allow(…)]` and `#[rustfmt::skip]` live there and none of
    // them touches the wire. Failing a test ABOUT THE WIRE because somebody
    // added `#[ignore]` to a test is a false alarm, and a pin that cries wolf
    // gets deleted by the first person it inconveniences.
    let wire_region_end = lines
        .iter()
        .position(|l| {
            l.code.starts_with("#[cfg(test)]")
                || l.code.starts_with("mod tests {")
                || l.code.starts_with("pub mod tests {")
        })
        .unwrap_or(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let indented = lines[i].raw.starts_with(' ') || lines[i].raw.starts_with('\t');
        let code = lines[i].code.trim();
        let trimmed = code;
        let in_wire_region = i < wire_region_end;
        if in_wire_region && trimmed.starts_with("#[") {
            assert!(
                trimmed.starts_with("#[derive(") || trimmed.starts_with("#[serde("),
                "unrecognised attribute above a wire declaration — the pin cannot tell \
                 whether it rewrites the wire: '{}'",
                lines[i].raw.trim()
            );
        }
        for keyword in ["pub struct ", "pub enum "] {
            if !indented && code.starts_with(keyword) {
                assert!(
                    code.ends_with('{'),
                    "wire item is not brace-shaped, so the pin cannot see its members: '{}'",
                    lines[i].raw
                );
            }
        }
        if !indented && code.starts_with("pub struct ") {
            let mut j = i + 1;
            while j < lines.len() && lines[j].code != "}" {
                let body = &lines[j];
                // The trimmed line, not the indented one: a comment removed
                // from the head of a line leaves its spaces behind, and
                // `/* note */ pub x: u32,` is a `pub` field by every rule that
                // matters — including the collector's, which trims too.
                let t = body.code.trim();
                let skippable = t.is_empty() || t.starts_with("#[");
                assert!(
                    skippable || t.starts_with("pub "),
                    "non-pub field in a wire struct is invisible to the pin: '{}'",
                    body.raw
                );
                j += 1;
            }
            i = j;
        }
        if let Some(rest) = code.strip_prefix("pub enum ").filter(|_| !indented) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let item = enums
                .iter()
                .find(|item| item.name == name)
                .unwrap_or_else(|| panic!("enum '{name}' was not collected at all"));
            let collected = item.members.len();
            let counted = count_variant_lines(&lines, i + 1);
            // Proof 6: a variant whose line opens a brace must carry either
            // fields or the empty-body marker, never a bare name — that is the
            // only thing separating `One {}` from `One,` in the digest.
            let mut j = i + 1;
            let mut nth = 0usize;
            while j < lines.len() && lines[j].code != "}" {
                let t = lines[j].code.trim();
                if !t.is_empty() && !t.starts_with("#[") && t != "}" && t != "}," {
                    if lines[j].code.starts_with("    ") && !lines[j].code.starts_with("     ") {
                        if t.ends_with('{') {
                            assert!(
                                item.members[nth].contains(" | "),
                                "'{name}': the struct-like variant '{t}' reached the digest as \
                                 a bare name, which is exactly what a unit variant looks like"
                            );
                        }
                        nth += 1;
                    }
                }
                j += 1;
            }
            assert_eq!(
                collected, counted,
                "'{name}': the collector kept {collected} variants but the body declares \
                 {counted}. A variant the parser did not recognise would shrink the digest \
                 without changing a single pinned number, which is exactly the failure this \
                 recount exists to make impossible."
            );
        }
        i += 1;
    }
}
