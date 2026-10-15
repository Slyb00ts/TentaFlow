// =============================================================================
// File: bus/schema_registry/text_guard.rs — hidden-character screen for schema text
// =============================================================================
// "Trojan Source" (owner decision 2026-10-12): a pattern's text is read by
// people in the dashboard and by tools in review, so a character that is
// invisible or reorders what is shown lets a schema mean one thing on screen
// and another to its parser. The screen runs once, on the WHOLE text, before
// any format parser sees it, for every kind and everywhere in the text
// (comments, string literals, names, whitespace between tokens). It is the one
// definition of the refused set; no format keeps a list of its own.
//
// The set lives in `hidden_characters.json` (ranges with names) and nowhere
// else: this module reads it, and the dashboard's copy
// (`www/js/modules/tentabus/hidden-characters.js`) is proved equal to it by a
// test that reads the same file. Refused:
//   - C0 controls except TAB, LF and CR (so NUL, ESC, VT, FF, BEL, ...);
//   - DEL and the C1 controls U+0080-U+009F (U+0085 NEXT LINE included);
//   - bidirectional controls: U+061C, U+200E, U+200F, U+202A-U+202E,
//     U+2066-U+2069;
//   - zero-width and invisible format characters: U+00AD, U+180E,
//     U+200B-U+200D, U+2060-U+2064;
//   - the whole of Unicode category Cf, by block, verified against Unicode 16
//     (the newest version available to the toolchain here, Python 3.14's
//     `unicodedata`; every Cf code point of it is in the table): U+0600-0605,
//     U+06DD, U+070F, U+0890-0891, U+08E2, U+2065-206F, U+FEFF, U+FFF9-FFFB,
//     U+110BD, U+110CD, U+13430-1343F, U+1BCA0-1BCA3, U+1D173-1D17A and the tag
//     block U+E0000-E007F;
//   - invisible characters outside Cf: the Hangul fillers U+115F, U+1160,
//     U+3164, U+FFA0, the combining grapheme joiner U+034F, and the Mongolian
//     free variation selectors U+180B-180D and U+180F;
//   - U+2028 and U+2029 (line and paragraph separator): they break a line on
//     screen and in some tools but are not a line end for the parsers.
// NOT refused, on purpose: the variation selectors U+FE00-FE0F and
// U+E0100-E01EF, which emoji presentation (U+FE0F) and CJK glyph selection
// legitimately use; and every ordinary letter, so Polish text and plain emoji
// pass. Emoji joined by U+200D are refused because that character is listed.
//
// U+FEFF is refused everywhere EXCEPT as the first character of an XSD:
// `quick-xml` (the XSD reader) strips a leading UTF-8 BOM and XML allows one, so
// editors that add it have always registered; anywhere else, and in every other
// kind (serde_json and the proto/thrift lexers reject it), it is hidden content.
//
// Positions are 1-based and count Unicode scalar values (not bytes, not UTF-16
// units, not grapheme clusters); a line ends at LF, so in CRLF text the CR is the
// last column of its line, and a lone CR starts no line.
//
// A character can also be named without standing in the text: a string escape
// (`"pac\u202Ejent"` in JSON, `&#x202E;` in XML, `\xE2\x80\xAE` in a .proto
// string) reaches the parser's output as the character. The screen therefore also
// reads the escapes of each format (`Syntax`) and refuses one that decodes to a
// refused character with the same sentence, pointing at the escape and adding
// "(written as an escape sequence)". Covered: JSON escapes (JSON Schema, Avro,
// HL7 profile), numeric character references of an XSD outside comments, CDATA
// and processing instructions, and byte escapes of a Protobuf string literal
// decoded as UTF-8. Not covered: Thrift has no escape that can decode to one
// (its lexer takes only `\\ \" \' \n \r \t`), Protobuf's `\u` is not an escape
// for its lexer, and an entity declared in a DTD (the XSD reader refuses DOCTYPE).
// An escape is refused only when it decodes to U+0100 or above, or to U+00AD or
// U+0085: below U+0100 the formats write ordinary controls and bytes that way
// (`[^\u0000-\u001F]` in a JSON pattern, `[^&#x7F;-&#x9F;]` in an XSD pattern,
// the bytes of an Avro `bytes` default, `"\x00\x01"` in a proto2 default), none
// of which can reorder or hide text on screen. A raw character is refused in
// every position all the same. Two kinds of position never show an escape decoded,
// so an escape there is not refused (a raw character still is): a JSON `pattern`
// value and `patternProperties` key, and the `value` of an XSD `xs:pattern`;
// schemas that defend against a zero-width space or a BOM need to name it there.
// An Avro `bytes` or `fixed` default cannot write the bytes 0x85 and 0xAD as
// `\u0085` / `\u00AD`: those are refused like in any other JSON text. A Protobuf
// string literal is decoded as UTF-8 only outside a `bytes` field's default, and
// adjacent literals (`"\xE2" "\x80\xAE"`) are joined first, as the lexer does.
// Known residual: an escape of a C0/C1 control below U+0100 passes in a key or a
// name; the dashboard marks it, the server does not refuse it.
// A refusal lists every place (raw characters and escapes together), at most
// `MAX_REFUSED_PLACES` and a count of the rest, so one text is never refused twice.
//
// A text stored before this screen existed (or applied from a peer, which
// stores the row as it came) is not rewritten. It fails on its next compile
// with this same refusal, which the bus reports as the "stored schema cannot be
// compiled" sentence (`bus/mod.rs` `STORED_SCHEMA_UNREADABLE`) followed by it;
// `registry::warn_about_stored_hidden_characters` lists such versions in the log
// at startup. A corrected version registers as usual: the compatibility check
// reads the old text without this screen, and a detail it builds from that text
// is passed through `sanitize`, so a refused character never reaches the UI or
// the audit trail as itself but as ⟦U+XXXX⟧. The comparison itself reads the old
// text with the refused characters REMOVED (`strip_hidden`): that version cannot
// compile any more, and removing them is exactly what the author's correction
// does, so a field `pac<U+202E>jent` corrected to `pacjent` is the same field
// and not a rename.
// Escapes are written out first (`reveal`), so a stored `"pac\u202Ejent"` is
// treated like the stored character. `hidden_spellings` lists, independent of the
// compatibility verdict, the names and values (not titles, descriptions or
// comments) of that old text that still carry one: the stored records keep them.
// =============================================================================

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

use super::SchemaError;

/// Stable opening of the refusal; the dashboard recognises it
/// (`schema-windows.js`) and `schema_error_phrases_are_stable` pins it.
pub(super) const HIDDEN_CHARACTER: &str =
    "the schema contains an invisible or direction-controlling character";

const TABLE: &str = include_str!("hidden_characters.json");

#[derive(Deserialize)]
struct Row<'a> {
    from: &'a str,
    to: &'a str,
    name: &'a str,
}

/// Inclusive code point range and the name a refusal gives it.
struct Span {
    from: u32,
    to: u32,
    name: &'static str,
}

/// The table, sorted and free of overlaps (a test pins both), parsed once.
fn spans() -> &'static [Span] {
    static SPANS: OnceLock<Vec<Span>> = OnceLock::new();
    SPANS.get_or_init(|| {
        let rows: Vec<Row<'static>> =
            serde_json::from_str(TABLE).expect("hidden_characters.json is valid JSON");
        rows.into_iter()
            .map(|r| Span {
                from: u32::from_str_radix(r.from, 16).expect("hex code point"),
                to: u32::from_str_radix(r.to, 16).expect("hex code point"),
                name: r.name,
            })
            .collect()
    })
}

/// The name of `c` when a schema may not contain it.
fn refused_name(c: char) -> Option<&'static str> {
    let cp = c as u32;
    // Printable ASCII, TAB, LF and CR: by far the most text, and never refused.
    if (0x20..0x7F).contains(&cp) || matches!(c, '\t' | '\n' | '\r') {
        return None;
    }
    let spans = spans();
    let at = spans.partition_point(|s| s.to < cp);
    spans.get(at).filter(|s| s.from <= cp).map(|s| s.name)
}

/// How a format writes one character in another way. A schema can name a
/// refused character without the character itself being in the text
/// (`"pac\u202Ejent"` in JSON, `&#x202E;` in XML, `\xE2\x80\xAE` in a .proto
/// string), and a parser then hands the decoded name or value on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syntax {
    /// JSON text: `\uXXXX` (and surrogate pairs) in strings.
    Json,
    /// XML: numeric character references outside comments, CDATA and
    /// processing instructions.
    Xml,
    /// Protobuf: hex and octal byte escapes and the control letters of a string
    /// literal, decoded as UTF-8 bytes. `\u` is NOT an escape there: the
    /// parser's lexer reads it as the letter `u`.
    Proto,
    /// Thrift: its lexer accepts only `\\ \" \' \n \r \t`, none of which can
    /// decode to a refused character.
    Thrift,
    /// A stored row whose kind is not known: it is not read as any format, so no
    /// escape is decoded (JSON rules would misread a .proto or XSD text).
    Unknown,
}

/// What a kind needs `text_guard` to know about its text.
#[derive(Clone, Copy, Debug)]
pub struct TextRules {
    /// Whether a U+FEFF at the very start is part of the format (XML allows a
    /// byte-order mark and its reader strips it). Every other kind, and every
    /// other position, treats it as hidden content.
    pub leading_bom: bool,
    pub syntax: Syntax,
}

impl TextRules {
    pub const JSON: TextRules = TextRules {
        leading_bom: false,
        syntax: Syntax::Json,
    };
    pub const XML: TextRules = TextRules {
        leading_bom: true,
        syntax: Syntax::Xml,
    };
    pub const PROTO: TextRules = TextRules {
        leading_bom: false,
        syntax: Syntax::Proto,
    };
    pub const THRIFT: TextRules = TextRules {
        leading_bom: false,
        syntax: Syntax::Thrift,
    };
}

/// A refused character a text writes as an escape: where the escape stands (in
/// bytes) and what it decodes to.
struct EscapeHit {
    start: usize,
    end: usize,
    character: char,
    name: &'static str,
}

fn hex_value(b: &[u8]) -> Option<u32> {
    b.iter().try_fold(0u32, |acc, d| {
        char::from(*d).to_digit(16).map(|v| acc * 16 + v)
    })
}

/// Whether an escape that decodes to `cp` is refused. Below U+0100 an escape is
/// how a format writes an ordinary control or byte; U+00AD and U+0085 stay
/// refused because they are invisible or end a line on screen.
fn refused_escape(cp: u32) -> bool {
    cp >= 0x100 || matches!(cp, 0xAD | 0x85)
}

fn refused_hit(start: usize, end: usize, cp: u32, out: &mut Vec<EscapeHit>) {
    if !refused_escape(cp) {
        return;
    }
    if let Some(c) = char::from_u32(cp) {
        if let Some(name) = refused_name(c) {
            out.push(EscapeHit {
                start,
                end,
                character: c,
                name,
            });
        }
    }
}

/// The escapes of one JSON string's content (`base` = where it starts in the text).
fn json_string_escapes(text: &str, base: usize, out: &mut Vec<EscapeHit>) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        if b.get(i + 1) != Some(&b'u') {
            i += if b.get(i + 1) == Some(&b'\\') { 2 } else { 1 };
            continue;
        }
        let Some(unit) = b.get(i + 2..i + 6).and_then(hex_value) else {
            i += 2;
            continue;
        };
        let low = (0xD800..0xDC00)
            .contains(&unit)
            .then(|| b.get(i + 6..i + 8).filter(|p| *p == b"\\u"))
            .flatten()
            .and_then(|_| b.get(i + 8..i + 12).and_then(hex_value))
            .filter(|l| (0xDC00..0xE000).contains(l));
        match low {
            Some(l) => {
                refused_hit(
                    base + i,
                    base + i + 12,
                    0x10000 + ((unit - 0xD800) << 10) + (l - 0xDC00),
                    out,
                );
                i += 12;
            }
            None => {
                refused_hit(base + i, base + i + 6, unit, out);
                i += 6;
            }
        }
    }
}

/// Where the reader of a JSON text is inside it: enough to tell a regular
/// expression (`pattern` value, `patternProperties` key) from a name or a value.
struct JsonFrame {
    object: bool,
    expect_key: bool,
    key: String,
    pattern_properties: bool,
}

fn json_escapes(text: &str, out: &mut Vec<EscapeHit>) {
    let b = text.as_bytes();
    let mut stack: Vec<JsonFrame> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' | b'[' => {
                let in_pattern_properties = stack
                    .last()
                    .is_some_and(|f| f.object && !f.expect_key && f.key == "patternProperties");
                stack.push(JsonFrame {
                    object: b[i] == b'{',
                    expect_key: true,
                    key: String::new(),
                    pattern_properties: b[i] == b'{' && in_pattern_properties,
                });
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                i += 1;
            }
            b',' => {
                if let Some(f) = stack.last_mut().filter(|f| f.object) {
                    f.expect_key = true;
                }
                i += 1;
            }
            b':' => {
                if let Some(f) = stack.last_mut().filter(|f| f.object) {
                    f.expect_key = false;
                }
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let mut end = start;
                while end < b.len() && b[end] != b'"' {
                    end += if b[end] == b'\\' { 2 } else { 1 };
                }
                let end = end.min(b.len());
                let content = text.get(start..end).unwrap_or("");
                let exempt = match stack.last_mut() {
                    Some(f) if f.object && f.expect_key => {
                        f.key = content.to_string();
                        f.pattern_properties
                    }
                    Some(f) if f.object => f.key == "pattern",
                    _ => false,
                };
                if !exempt {
                    json_string_escapes(content, start, out);
                }
                i = end + 1;
            }
            _ => i += 1,
        }
    }
}

/// A numeric character reference at `i`: where it ends and what it names. Every
/// digit counts (leading zeros do not hide a reference); a value too big for a
/// code point names nothing.
fn xml_reference(text: &str, i: usize, out: &mut Vec<EscapeHit>) -> Option<usize> {
    let b = text.as_bytes();
    if !text[i..].starts_with("&#") {
        return None;
    }
    let hex = matches!(b.get(i + 2), Some(b'x' | b'X'));
    let digits = i + if hex { 3 } else { 2 };
    let len = b[digits.min(b.len())..]
        .iter()
        .take_while(|d| char::from(**d).is_digit(if hex { 16 } else { 10 }))
        .count();
    if len == 0 || b.get(digits + len) != Some(&b';') {
        return None;
    }
    if let Ok(cp) = u32::from_str_radix(&text[digits..digits + len], if hex { 16 } else { 10 }) {
        refused_hit(i, digits + len + 1, cp, out);
    }
    Some(digits + len + 1)
}

/// Whether the tag that starts at `open` (a `<`) is an `xs:pattern`, whose
/// `value` is a regular expression and never shown decoded.
fn is_pattern_tag(text: &str, open: usize) -> bool {
    let name: String = text[open + 1..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '>' | '/'))
        .collect();
    name == "pattern" || name.ends_with(":pattern")
}

/// The attribute name written just before the `=` that precedes the quote at `quote`.
fn attribute_before(text: &str, quote: usize) -> &str {
    let before = text[..quote].trim_end();
    let before = before.strip_suffix('=').unwrap_or(before).trim_end();
    let start = before
        .rfind(|c: char| c.is_whitespace() || c == '<')
        .map_or(0, |p| p + 1);
    &before[start..]
}

fn xml_escapes(text: &str, out: &mut Vec<EscapeHit>) {
    let b = text.as_bytes();
    let mut i = 0;
    let mut tag: Option<usize> = None;
    let mut quote: Option<u8> = None;
    let mut pattern_value = false;
    while i < b.len() {
        let Some(rest) = text.get(i..) else {
            i += 1;
            continue;
        };
        if let Some(q) = quote {
            if b[i] == q {
                quote = None;
                i += 1;
            } else if !pattern_value {
                i = xml_reference(text, i, out).unwrap_or(i + 1);
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(open) = tag {
            match b[i] {
                b'>' => tag = None,
                q @ (b'"' | b'\'') => {
                    quote = Some(q);
                    pattern_value = is_pattern_tag(text, open) && attribute_before(text, i) == "value";
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        // Outside a tag a comment, CDATA section and processing instruction are
        // not decoded; inside one (an attribute value) `<!--` is plain text.
        let skip_to = [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")]
            .into_iter()
            .find(|(open, _)| rest.starts_with(open))
            .map(|(open, close)| {
                rest[open.len()..]
                    .find(close)
                    .map_or(b.len(), |p| i + open.len() + p + close.len())
            });
        if let Some(end) = skip_to {
            i = end;
        } else if b[i] == b'<' {
            tag = Some(i);
            i += 1;
        } else {
            i = xml_reference(text, i, out).unwrap_or(i + 1);
        }
    }
}

/// One decoded byte of a .proto string literal and where it was written.
struct SourceByte {
    value: u8,
    start: usize,
    end: usize,
    escaped: bool,
}

fn proto_escapes(text: &str, out: &mut Vec<EscapeHit>) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let Some(rest) = text.get(i..) else {
            i += 1;
            continue;
        };
        if rest.starts_with("//") {
            i += rest.find('\n').map_or(rest.len(), |p| p + 1);
        } else if rest.starts_with("/*") {
            i += rest[2..].find("*/").map_or(rest.len(), |p| p + 4);
        } else if b[i] == b'"' || b[i] == b'\'' {
            i = proto_run(text, i, out);
        } else {
            i += 1;
        }
    }
}

/// Where the next token starts after whitespace and comments.
fn skip_blank(text: &str, mut i: usize) -> usize {
    loop {
        let rest = &text[i..];
        let trimmed = rest.trim_start();
        i += rest.len() - trimmed.len();
        if trimmed.starts_with("//") {
            i += trimmed.find('\n').map_or(trimmed.len(), |p| p + 1);
        } else if trimmed.starts_with("/*") {
            i += trimmed[2..].find("*/").map_or(trimmed.len(), |p| p + 4);
        } else {
            return i;
        }
    }
}

/// Whether the statement that holds the literal at `at` declares a `bytes`
/// field (`optional bytes magic = 1 [default = "\xEF\xBB\xBF"]`): the type is the
/// token before the field name, left of the first `=`. The default of such a
/// field is bytes, not text, so it is not read as UTF-8.
fn is_bytes_default(text: &str, at: usize) -> bool {
    let before = &text[..at];
    let start = before.rfind([';', '{', '}']).map_or(0, |p| p + 1);
    let statement = &before[start..];
    let head = statement.split('=').next().unwrap_or("");
    let tokens: Vec<&str> = head
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .filter(|t| !t.is_empty())
        .collect();
    statement.contains('=')
        && tokens.len() >= 2
        && tokens[tokens.len() - 2] == "bytes"
}

/// Reads the run of adjacent string literals that starts at `open`: the lexer
/// joins them (blanks and comments between them are skipped) before it decodes
/// UTF-8, so `"\xE2" "\x80\xAE"` is one character. Reports the refused
/// characters their escapes decode to; returns where the run ends.
fn proto_run(text: &str, open: usize, out: &mut Vec<EscapeHit>) -> usize {
    let mut bytes: Vec<SourceByte> = Vec::new();
    let mut at = open;
    loop {
        at = proto_literal_bytes(text, at, &mut bytes);
        let next = skip_blank(text, at.min(text.len()));
        if matches!(text.as_bytes().get(next), Some(b'"' | b'\'')) {
            at = next;
        } else {
            break;
        }
    }
    if !is_bytes_default(text, open) {
        decode_proto_bytes(&bytes, out);
    }
    at
}

/// Appends the bytes one string literal decodes to; returns where it ends.
fn proto_literal_bytes(text: &str, open: usize, bytes: &mut Vec<SourceByte>) -> usize {
    let b = text.as_bytes();
    let quote = b[open];
    let mut i = open + 1;
    while i < b.len() && b[i] != quote && b[i] != b'\n' {
        if b[i] != b'\\' {
            let len = text[i..].chars().next().map_or(1, char::len_utf8);
            bytes.extend(
                text.as_bytes()[i..i + len]
                    .iter()
                    .enumerate()
                    .map(|(k, v)| SourceByte {
                        value: *v,
                        start: i + k,
                        end: i + k + 1,
                        escaped: false,
                    }),
            );
            i += len;
            continue;
        }
        let Some(next) = b.get(i + 1).copied() else {
            return b.len();
        };
        let (value, len) = match next {
            b'a' => (Some(7), 2),
            b'b' => (Some(8), 2),
            b'f' => (Some(0x0c), 2),
            b'n' => (Some(b'\n'), 2),
            b'r' => (Some(b'\r'), 2),
            b't' => (Some(b'\t'), 2),
            b'v' => (Some(0x0b), 2),
            b'x' => match b.get(i + 2..i + 4).and_then(hex_value) {
                Some(v) => (Some(v as u8), 4),
                None => (None, 2),
            },
            // The protobuf lexer takes any decimal digit after the first octal one
            // and lets the byte wrap, so `\428` is 0x18.
            b'0'..=b'7' => {
                let digits = 1 + b[i + 2..]
                    .iter()
                    .take(2)
                    .take_while(|d| d.is_ascii_digit())
                    .count();
                let v = b[i + 1..i + 1 + digits]
                    .iter()
                    .fold(0u8, |acc, d| (acc << 3).wrapping_add(d - b'0'));
                (Some(v), 1 + digits)
            }
            _ => (
                None,
                1 + text[i + 1..].chars().next().map_or(1, char::len_utf8),
            ),
        };
        match value {
            Some(value) => bytes.push(SourceByte {
                value,
                start: i,
                end: i + len,
                escaped: true,
            }),
            // A backslash before any other character leaves that character.
            None => {
                for k in i + 1..i + len {
                    bytes.push(SourceByte {
                        value: b[k],
                        start: k,
                        end: k + 1,
                        escaped: false,
                    });
                }
            }
        }
        i += len;
    }
    i + 1
}

/// Groups the bytes of a joined run as UTF-8 and reports the refused characters
/// some escape wrote. A group that is not valid UTF-8 is not text and passes.
fn decode_proto_bytes(bytes: &[SourceByte], out: &mut Vec<EscapeHit>) {
    let mut at = 0;
    while at < bytes.len() {
        let lead = bytes[at].value;
        let want = match lead {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        let chunk: Vec<u8> = bytes.iter().skip(at).take(want).map(|s| s.value).collect();
        match std::str::from_utf8(&chunk)
            .ok()
            .and_then(|s| s.chars().next())
        {
            Some(c) if chunk.len() == want => {
                let group = &bytes[at..at + want];
                if group.iter().any(|s| s.escaped) {
                    refused_hit(group[0].start, group[want - 1].end, c as u32, out);
                }
                at += want;
            }
            _ => at += 1,
        }
    }
}

/// Every refused character `text` writes as an escape, in order of appearance.
fn escape_hits(text: &str, syntax: Syntax) -> Vec<EscapeHit> {
    let mut out = Vec::new();
    match syntax {
        Syntax::Json => json_escapes(text, &mut out),
        Syntax::Xml => xml_escapes(text, &mut out),
        Syntax::Proto => proto_escapes(text, &mut out),
        Syntax::Thrift | Syntax::Unknown => {}
    }
    out
}

/// `text` with every escape that decodes to a refused character written out as
/// that character, so the raw rules (`find_hidden`, `strip_hidden`,
/// `hidden_spellings`) see what a parser would. Borrowed when there is none.
pub(super) fn reveal(text: &str, rules: TextRules) -> Cow<'_, str> {
    let hits = escape_hits(text, rules.syntax);
    if hits.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for hit in hits {
        out.push_str(&text[from..hit.start]);
        out.push(hit.character);
        from = hit.end;
    }
    out.push_str(&text[from..]);
    Cow::Owned(out)
}

/// The first refused character of a text and where it stands.
pub(super) struct Finding {
    pub(super) character: char,
    pub(super) name: &'static str,
    pub(super) line: usize,
    pub(super) column: usize,
    /// Written as an escape sequence, not as the character itself.
    pub(super) escape: bool,
}

impl Finding {
    pub(super) fn code_point(&self) -> String {
        format!("U+{:04X}", self.character as u32)
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}) at line {}, column {}",
            self.code_point(),
            self.name,
            self.line,
            self.column
        )?;
        if self.escape {
            f.write_str(WRITTEN_AS_ESCAPE)?;
        }
        Ok(())
    }
}

/// Follows the position of a finding whose character is written as an escape;
/// the dashboard recognises it (`schema-windows.js`).
pub(super) const WRITTEN_AS_ESCAPE: &str = " (written as an escape sequence)";

fn has_refused(text: &str) -> bool {
    text.chars().any(|c| refused_name(c).is_some())
}

/// Most places one refusal lists; the count of the rest follows them.
pub(super) const MAX_REFUSED_PLACES: usize = 20;

/// Every refused character of `text`, written as itself or as an escape, in
/// order of appearance. One pass over the text, however many there are.
pub(super) fn find_all_hidden(text: &str, rules: TextRules) -> Vec<Finding> {
    let mut events: Vec<(usize, char, &'static str, bool)> = Vec::new();
    for (byte, c) in text.char_indices() {
        if let Some(name) = refused_name(c) {
            if !(rules.leading_bom && byte == 0 && c == '\u{FEFF}') {
                events.push((byte, c, name, false));
            }
        }
    }
    events.extend(
        escape_hits(text, rules.syntax)
            .into_iter()
            .map(|h| (h.start, h.character, h.name, true)),
    );
    events.sort_by_key(|e| e.0);
    let (mut line, mut column, mut cursor) = (1usize, 1usize, 0usize);
    events
        .into_iter()
        .map(|(byte, character, name, escape)| {
            for c in text[cursor..byte].chars() {
                if c == '\n' {
                    line += 1;
                    column = 1;
                } else {
                    column += 1;
                }
            }
            cursor = byte;
            Finding {
                character,
                name,
                line,
                column,
                escape,
            }
        })
        .collect()
}

/// The first refused character of `text`, written as itself or as an escape,
/// whichever stands first.
pub(super) fn find_hidden(text: &str, rules: TextRules) -> Option<Finding> {
    find_all_hidden(text, rules).into_iter().next()
}

/// Schema text that passed the screen. Only [`screen`] makes one, and the
/// kind-specific methods of `SchemaKindOps` take nothing else, so no caller can
/// reach a parser without going through the screen.
#[derive(Clone, Copy)]
pub struct Screened<'a>(&'a str);

impl<'a> Screened<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

/// The places of a refusal: `U+202E (NAME) at line 3, column 14` each, joined by
/// `; `, the first [`MAX_REFUSED_PLACES`] and `; and N more`.
fn places_sentence(found: &[Finding]) -> String {
    let mut out = found
        .iter()
        .take(MAX_REFUSED_PLACES)
        .map(Finding::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    if found.len() > MAX_REFUSED_PLACES {
        out.push_str(&format!("; and {} more", found.len() - MAX_REFUSED_PLACES));
    }
    out
}

pub(super) fn screen(text: &str, rules: TextRules) -> Result<Screened<'_>, SchemaError> {
    let found = find_all_hidden(text, rules);
    if found.is_empty() {
        return Ok(Screened(text));
    }
    Err(SchemaError::Invalid(format!(
        "{HIDDEN_CHARACTER} {}",
        places_sentence(&found)
    )))
}

/// `text` without any character [`find_hidden`] would refuse, written as itself
/// or as an escape. A stored version that holds one is compared against a new
/// one in this form (see `SchemaKindOps::check_compatibility`).
pub(super) fn strip_hidden(text: &str, rules: TextRules) -> String {
    reveal(text, rules)
        .chars()
        .enumerate()
        .filter(|(index, c)| {
            refused_name(*c).is_none() || (rules.leading_bom && *index == 0 && *c == '\u{FEFF}')
        })
        .map(|(_, c)| c)
        .collect()
}

/// A JSON text a parser can read although it holds refused characters: outside
/// a string literal they are blanks between tokens and are dropped; inside one
/// they stay, written as `⟦U+XXXX⟧`, so two names that differ only by them stay
/// two names.
fn lenient_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        let refused = refused_name(c).is_some();
        if in_string {
            if refused {
                out.push_str(&format!("⟦U+{:04X}⟧", c as u32));
                continue;
            }
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if refused {
            continue;
        } else if c == '"' {
            in_string = true;
        }
        out.push(c);
    }
    out
}

/// `text` without the `⟦U+XXXX⟧` markers.
fn unmark(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("⟦U+") {
        out.push_str(&rest[..at]);
        match rest[at..].find('⟧') {
            Some(end) => rest = &rest[at + end + '⟧'.len_utf8()..],
            None => {
                out.push_str(&rest[at..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// A parsed JSON document that keeps every key of an object, repeated ones too
/// (serde_json's own map would keep the last without a word).
enum Node {
    Object(Vec<(String, Node)>),
    Array(Vec<Node>),
    Text(String),
    Other,
}

impl<'de> serde::Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Node, D::Error> {
        use serde::de::{MapAccess, SeqAccess, Visitor};
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Node;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E>(self, _: bool) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_str<E>(self, s: &str) -> Result<Node, E> {
                Ok(Node::Text(s.to_string()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Node::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, Node>()? {
                    pairs.push(pair);
                }
                Ok(Node::Object(pairs))
            }
        }
        d.deserialize_any(V)
    }
}

fn json_tree(revealed: &str) -> Option<Node> {
    serde_json::from_str(&lenient_json(revealed)).ok()
}

fn merged_key(node: &Node) -> Option<String> {
    match node {
        Node::Object(pairs) => {
            let mut spellings: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
            for (key, _) in pairs {
                spellings.entry(unmark(key)).or_default().insert(key);
            }
            // The same spelling twice is a duplicate the text already had; two
            // spellings of one name are what removing the characters merges.
            spellings
                .into_iter()
                .find(|(_, spelled)| spelled.len() > 1)
                .map(|(name, _)| name)
                .or_else(|| pairs.iter().find_map(|(_, value)| merged_key(value)))
        }
        Node::Array(items) => items.iter().find_map(merged_key),
        Node::Text(_) | Node::Other => None,
    }
}

/// The key that removing the refused characters of a stored old text would
/// make a duplicate (`pesel` and `pesel<ZWSP>` in one object becoming one),
/// which the comparison would read as the last of them without a word. A
/// duplicate the stored text already had on its own is not this kind and is
/// left alone, and so is a text that is not JSON.
pub(super) fn collapsed_key(revealed: &str, rules: TextRules) -> Option<String> {
    if !matches!(rules.syntax, Syntax::Json) {
        return None;
    }
    merged_key(&json_tree(revealed)?)
}

/// Keys whose string value is prose for a reader, or a pattern, identifier or
/// format label of the schema itself, and not a name or a value that data carries.
const NON_DATA_KEYS: [&str; 9] = [
    "title",
    "description",
    "doc",
    "$comment",
    "example",
    "pattern",
    "$id",
    "$schema",
    "format",
];

/// Longest name or value a list shows, in characters.
const SHOWN_CHARS: usize = 120;

/// Most names a list carries; the true count travels beside it.
pub const MAX_LISTED_SPELLINGS: usize = 20;

/// The names and values a text spells with a refused character.
#[derive(Debug, Default)]
pub struct Spellings {
    /// The first [`MAX_LISTED_SPELLINGS`], in order of appearance, without
    /// repeats, with every refused character written as `⟦U+XXXX⟧`.
    pub shown: Vec<String>,
    /// How many there are in all.
    pub total: usize,
}

fn json_spellings(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Object(pairs) => {
            for (key, value) in pairs {
                if key.contains("⟦U+") {
                    out.push(key.clone());
                }
                let prose = match value {
                    Node::Text(_) => NON_DATA_KEYS.contains(&unmark(key).as_str()),
                    Node::Array(_) => unmark(key) == "examples",
                    _ => false,
                };
                if !prose {
                    json_spellings(value, out);
                }
            }
        }
        Node::Array(items) => items.iter().for_each(|item| json_spellings(item, out)),
        Node::Text(s) if s.contains("⟦U+") => out.push(s.clone()),
        Node::Text(_) | Node::Other => {}
    }
}

/// The attribute values of an XML text: where a schema names things. Element
/// text is documentation.
fn xml_spellings(text: &str, out: &mut Vec<String>) {
    let b = text.as_bytes();
    let mut i = 0;
    let mut in_tag = false;
    while i < b.len() {
        let Some(rest) = text.get(i..) else {
            i += 1;
            continue;
        };
        // Inside a tag these are plain text of an attribute value, and searching
        // for their close from every one of them would make the scan quadratic.
        let skip = (!in_tag)
            .then(|| {
                [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")]
                    .into_iter()
                    .find(|(open, _)| rest.starts_with(open))
            })
            .flatten()
            .map(|(open, close)| {
                rest[open.len()..]
                    .find(close)
                    .map_or(b.len(), |p| i + open.len() + p + close.len())
            });
        if let Some(end) = skip {
            i = end;
            continue;
        }
        match b[i] {
            b'<' => in_tag = true,
            b'>' => in_tag = false,
            q @ (b'"' | b'\'') if in_tag => {
                let end = rest[1..].find(char::from(q)).map_or(b.len(), |p| i + 1 + p);
                let value = &text[i + 1..end];
                if has_refused(value) {
                    out.push(sanitize(value));
                }
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
}

/// The identifiers of a .proto or Thrift text: comments and string literals
/// hold prose and option values.
fn idl_spellings(text: &str, hash_comments: bool, out: &mut Vec<String>) {
    let b = text.as_bytes();
    let mut word = String::new();
    let mut flush = |word: &mut String| {
        if word.chars().any(char::is_alphanumeric) && has_refused(word) {
            out.push(sanitize(word));
        }
        word.clear();
    };
    let mut i = 0;
    while i < b.len() {
        let Some(rest) = text.get(i..) else {
            i += 1;
            continue;
        };
        if rest.starts_with("//") || (hash_comments && b[i] == b'#') {
            flush(&mut word);
            i += rest.find('\n').map_or(rest.len(), |p| p + 1);
        } else if rest.starts_with("/*") {
            flush(&mut word);
            i += rest[2..].find("*/").map_or(rest.len(), |p| p + 4);
        } else if b[i] == b'"' || b[i] == b'\'' {
            flush(&mut word);
            let mut j = i + 1;
            while j < b.len() && b[j] != b[i] && b[j] != b'\n' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            if c.is_alphanumeric() || c == '_' || c == '.' || refused_name(c).is_some() {
                word.push(c);
            } else {
                flush(&mut word);
            }
            i += c.len_utf8();
        }
    }
    flush(&mut word);
}

/// The names and values of a text, in the positions where a stored record
/// carries them (field and property names, enum symbols, constant values,
/// references), that are spelled with a refused character. Titles,
/// descriptions, comments, patterns, `$id`/`$schema`/`format` labels and every
/// string literal of a .proto or Thrift text (options and defaults alike) are
/// not names and are left out.
/// `revealed` is the text after [`reveal`].
pub(super) fn hidden_spellings(revealed: &str, rules: TextRules) -> Spellings {
    let mut all = Vec::new();
    match rules.syntax {
        Syntax::Json => {
            if let Some(tree) = json_tree(revealed) {
                json_spellings(&tree, &mut all);
            }
        }
        Syntax::Xml => xml_spellings(revealed, &mut all),
        Syntax::Proto => idl_spellings(revealed, false, &mut all),
        Syntax::Thrift => idl_spellings(revealed, true, &mut all),
        Syntax::Unknown => {}
    }
    let mut seen = BTreeSet::new();
    let unique: Vec<String> = all
        .into_iter()
        .map(|s| {
            if s.chars().count() > SHOWN_CHARS {
                format!("{}…", s.chars().take(SHOWN_CHARS).collect::<String>())
            } else {
                s
            }
        })
        .filter(|s| seen.insert(s.clone()))
        .collect();
    Spellings {
        total: unique.len(),
        shown: unique.into_iter().take(MAX_LISTED_SPELLINGS).collect(),
    }
}

/// `detail` with every refused code point written as `⟦U+XXXX⟧` (the marker the
/// dashboard uses for a stored text too), for a message
/// built from a text the screen has not seen (the old side of a comparison).
pub(super) fn sanitize(detail: &str) -> String {
    detail
        .chars()
        .fold(String::with_capacity(detail.len()), |mut out, c| {
            if refused_name(c).is_some() {
                out.push_str(&format!("⟦U+{:04X}⟧", c as u32));
            } else {
                out.push(c);
            }
            out
        })
}

pub(super) fn sanitize_error(error: SchemaError) -> SchemaError {
    match error {
        SchemaError::Invalid(m) => SchemaError::Invalid(sanitize(&m)),
        SchemaError::Violation(m) => SchemaError::Violation(sanitize(&m)),
        SchemaError::LimitExceeded(m) => SchemaError::LimitExceeded(sanitize(&m)),
        SchemaError::Incompatible(m) => SchemaError::Incompatible(sanitize(&m)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::{Compatibility, SchemaError, SchemaKindOps, SchemaType};
    use super::*;

    /// One character of every refused class, with the name the message gives.
    const REFUSED: [(char, &str); 24] = [
        ('\u{1}', "CONTROL CHARACTER"),
        ('\u{7f}', "DELETE"),
        ('\u{85}', "NEXT LINE"),
        ('\u{202e}', "RIGHT-TO-LEFT OVERRIDE"),
        ('\u{2066}', "LEFT-TO-RIGHT ISOLATE"),
        ('\u{200e}', "LEFT-TO-RIGHT MARK"),
        ('\u{200f}', "RIGHT-TO-LEFT MARK"),
        ('\u{61c}', "ARABIC LETTER MARK"),
        ('\u{200b}', "ZERO WIDTH SPACE"),
        ('\u{2060}', "WORD JOINER"),
        ('\u{feff}', "ZERO WIDTH NO-BREAK SPACE"),
        ('\u{ad}', "SOFT HYPHEN"),
        ('\u{180e}', "MONGOLIAN VOWEL SEPARATOR"),
        ('\u{2028}', "LINE SEPARATOR"),
        ('\u{206a}', "INVISIBLE FORMAT CHARACTER"),
        ('\u{600}', "INVISIBLE FORMAT CHARACTER"),
        ('\u{e0041}', "TAG CHARACTER"),
        ('\u{34f}', "COMBINING GRAPHEME JOINER"),
        ('\u{115f}', "HANGUL CHOSEONG FILLER"),
        ('\u{1160}', "HANGUL JUNGSEONG FILLER"),
        ('\u{3164}', "HANGUL FILLER"),
        ('\u{ffa0}', "HALFWIDTH HANGUL FILLER"),
        ('\u{180b}', "MONGOLIAN FREE VARIATION SELECTOR ONE"),
        ('\u{180f}', "MONGOLIAN FREE VARIATION SELECTOR FOUR"),
    ];

    #[derive(Clone, Copy, PartialEq)]
    enum Slot {
        Space,
        Comment,
        Text,
        Name,
    }

    struct Case {
        kind: SchemaType,
        slot: Slot,
        template: &'static str,
        /// What replaces the marker in the control text that must compile.
        plain: &'static str,
    }

    const MARK: char = '§';

    const fn case(
        kind: SchemaType,
        slot: Slot,
        template: &'static str,
        plain: &'static str,
    ) -> Case {
        Case {
            kind,
            slot,
            template,
            plain,
        }
    }

    const JSON: &str = "{\n  \"type\": \"object\",\n  \"properties\": {\n    \"a\": {\"type\": \"string\"}§\n  }\n}";
    const JSON_TEXT: &str = "{\n  \"type\": \"object\",\n  \"description\": \"opis §\"\n}";
    const JSON_NAME: &str = "{\n  \"type\": \"object\",\n  \"properties\": {\n    \"poz§\": {\"type\": \"string\"}\n  }\n}";
    const AVRO: &str = "{\n  \"type\": \"record\",\n  \"name\": \"R\",\n  \"fields\": [\n    {\"name\": \"a\", \"type\": \"string\"}§\n  ]\n}";
    const AVRO_TEXT: &str = "{\n  \"type\": \"record\",\n  \"name\": \"R\",\n  \"fields\": [\n    {\"name\": \"a\", \"type\": \"string\", \"doc\": \"opis §\"}\n  ]\n}";
    const AVRO_NAME: &str = "{\n  \"type\": \"record\",\n  \"name\": \"R\",\n  \"fields\": [\n    {\"name\": \"po§\", \"type\": \"string\"}\n  ]\n}";
    const XSD_HEAD: &str = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n";
    const HL7_SPACE: &str = "{\n  \"required_segments\": [\"MSH\"]§\n}";
    const HL7_TEXT: &str = "{\n  \"description\": \"opis §\"\n}";
    const HL7_NAME: &str = "{\n  \"required_segments\": [\"P§D\"]\n}";
    const PROTO_HEAD: &str = "syntax = \"proto3\";\n";
    const THRIFT_HEAD: &str = "namespace rs t\n";

    fn cases() -> Vec<Case> {
        // The XSD, Protobuf and Thrift templates are joined with their
        // heads at the call site below (constant concatenation is not
        // available for borrowed consts).
        let xsd_space = leak(format!(
            "{XSD_HEAD}  <xs:element name=\"a\" type=\"xs:string\"/>§\n</xs:schema>"
        ));
        let xsd_comment = leak(format!("{XSD_HEAD}  <!-- opis § -->\n  <xs:element name=\"a\" type=\"xs:string\"/>\n</xs:schema>"));
        let xsd_text = leak(format!("{XSD_HEAD}  <xs:element name=\"a\" type=\"xs:string\">\n    <xs:annotation><xs:documentation>opis §</xs:documentation></xs:annotation>\n  </xs:element>\n</xs:schema>"));
        let xsd_name = leak(format!(
            "{XSD_HEAD}  <xs:element name=\"po§\" type=\"xs:string\"/>\n</xs:schema>"
        ));
        let proto_space = leak(format!("{PROTO_HEAD}message M {{ int32 a = 1; }}§\n"));
        let proto_comment = leak(format!(
            "{PROTO_HEAD}// opis §\nmessage M {{ int32 a = 1; }}\n"
        ));
        let proto_block = leak(format!(
            "{PROTO_HEAD}/* opis\n   § */\nmessage M {{ int32 a = 1; }}\n"
        ));
        let proto_text = leak(format!(
            "{PROTO_HEAD}option java_package = \"opis §\";\nmessage M {{ int32 a = 1; }}\n"
        ));
        let proto_name = leak(format!("{PROTO_HEAD}message M§ {{ int32 a = 1; }}\n"));
        let thrift_space = leak(format!("{THRIFT_HEAD}struct S {{ 1: i32 a }}§\n"));
        let thrift_comment = leak(format!("{THRIFT_HEAD}// opis §\nstruct S {{ 1: i32 a }}\n"));
        let thrift_block = leak(format!(
            "{THRIFT_HEAD}/* opis\n   § */\nstruct S {{ 1: i32 a }}\n"
        ));
        let thrift_hash = leak(format!("{THRIFT_HEAD}# opis §\nstruct S {{ 1: i32 a }}\n"));
        let thrift_text = leak(format!(
            "{THRIFT_HEAD}struct S {{\n  1: string a = \"opis §\"\n}}\n"
        ));
        let thrift_name = leak(format!("{THRIFT_HEAD}struct S§ {{ 1: i32 a }}\n"));
        vec![
            case(SchemaType::JsonSchema, Slot::Space, JSON, ""),
            case(SchemaType::JsonSchema, Slot::Text, JSON_TEXT, "ą"),
            case(SchemaType::JsonSchema, Slot::Name, JSON_NAME, "x"),
            case(SchemaType::Avro, Slot::Space, AVRO, ""),
            case(SchemaType::Avro, Slot::Text, AVRO_TEXT, "ą"),
            case(SchemaType::Avro, Slot::Name, AVRO_NAME, "x"),
            case(SchemaType::Xsd, Slot::Space, xsd_space, ""),
            case(SchemaType::Xsd, Slot::Comment, xsd_comment, "ą"),
            case(SchemaType::Xsd, Slot::Text, xsd_text, "ą"),
            case(SchemaType::Xsd, Slot::Name, xsd_name, "x"),
            case(SchemaType::Hl7v2Profile, Slot::Space, HL7_SPACE, ""),
            case(SchemaType::Hl7v2Profile, Slot::Text, HL7_TEXT, "ą"),
            case(SchemaType::Hl7v2Profile, Slot::Name, HL7_NAME, "I"),
            case(SchemaType::Protobuf, Slot::Space, proto_space, ""),
            case(SchemaType::Protobuf, Slot::Comment, proto_comment, "ą"),
            case(SchemaType::Protobuf, Slot::Comment, proto_block, "ą"),
            case(SchemaType::Protobuf, Slot::Text, proto_text, "ą"),
            case(SchemaType::Protobuf, Slot::Name, proto_name, "x"),
            case(SchemaType::Thrift, Slot::Space, thrift_space, ""),
            case(SchemaType::Thrift, Slot::Comment, thrift_comment, "ą"),
            case(SchemaType::Thrift, Slot::Comment, thrift_block, "ą"),
            case(SchemaType::Thrift, Slot::Comment, thrift_hash, "ą"),
            case(SchemaType::Thrift, Slot::Text, thrift_text, "ą"),
            case(SchemaType::Thrift, Slot::Name, thrift_name, "x"),
        ]
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn reject(text: &str) -> Result<(), SchemaError> {
        screen(text, TextRules::JSON).map(|_| ())
    }

    fn compile(kind: SchemaType, text: &str) -> Result<(), SchemaError> {
        kind.ops().compile(text).map(|_| ())
    }

    /// Line and column of the marker, counted here and not by the code under test.
    fn marker_position(template: &str) -> (usize, usize) {
        let before = &template[..template.find(MARK).unwrap()];
        let line = before.matches('\n').count() + 1;
        let column = before.chars().rev().take_while(|c| *c != '\n').count() + 1;
        (line, column)
    }

    #[test]
    fn every_template_compiles_without_the_hidden_character() {
        for c in cases() {
            let text = c.template.replace(MARK, c.plain);
            assert!(
                compile(c.kind, &text).is_ok(),
                "{:?} control text must compile: {:?}\n{text}",
                c.kind.as_str(),
                compile(c.kind, &text)
            );
        }
    }

    #[test]
    fn every_kind_refuses_each_class_everywhere_with_its_line_and_column() {
        let mut refused = 0;
        for c in cases() {
            let (line, column) = marker_position(c.template);
            for (ch, name) in REFUSED {
                let text = c.template.replace(MARK, &ch.to_string());
                let err = c.kind.ops().compile(&text).unwrap_err();
                let expected = format!(
                    "invalid schema: {HIDDEN_CHARACTER} U+{:04X} ({name}) at line {line}, column {column}",
                    ch as u32
                );
                assert_eq!(
                    err.to_string(),
                    expected,
                    "{} U+{:04X}\n{text:?}",
                    c.kind.as_str(),
                    ch as u32
                );
                refused += 1;
            }
        }
        assert_eq!(refused, cases().len() * REFUSED.len());
    }

    #[test]
    fn every_kind_has_a_case_in_every_slot_it_can_hold() {
        for kind in SchemaType::ALL {
            for slot in [Slot::Space, Slot::Text, Slot::Name] {
                assert!(
                    cases().iter().any(|c| c.kind == kind && c.slot == slot),
                    "{} has no case for one of its slots",
                    kind.as_str()
                );
            }
        }
    }

    #[test]
    fn polish_letters_and_emoji_pass_wherever_text_is_free() {
        for c in cases() {
            let fragment = match c.slot {
                Slot::Comment | Slot::Text => "zażółć gęślą jaźń 😀 👍🏽",
                Slot::Space | Slot::Name => continue,
            };
            let text = c.template.replace(MARK, fragment);
            assert!(
                compile(c.kind, &text).is_ok(),
                "{} refused Polish text: {:?}",
                c.kind.as_str(),
                compile(c.kind, &text)
            );
        }
    }

    #[test]
    fn tab_lf_and_cr_pass_between_tokens() {
        for c in cases().iter().filter(|c| c.slot == Slot::Space) {
            let text = c.template.replace(MARK, "\t\r\n \t");
            assert!(
                compile(c.kind, &text).is_ok(),
                "{} refused whitespace: {:?}",
                c.kind.as_str(),
                compile(c.kind, &text)
            );
        }
    }

    #[test]
    fn the_whole_refused_ranges_are_covered() {
        let refused = |from: u32, to: u32| {
            (from..=to)
                .filter_map(char::from_u32)
                .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
                .for_each(|c| {
                    assert!(
                        reject(&format!("a{c}b")).is_err(),
                        "U+{:04X} must be refused",
                        c as u32
                    )
                });
        };
        refused(0x00, 0x1F);
        refused(0x7F, 0x9F);
        refused(0x202A, 0x202E);
        refused(0x2060, 0x206F);
        refused(0x2066, 0x2069);
        refused(0x200B, 0x200F);
        refused(0xFFF9, 0xFFFB);
        refused(0xE0000, 0xE007F);
        for c in ['\u{AD}', '\u{61C}', '\u{180E}', '\u{FEFF}', '\u{2029}'] {
            refused(c as u32, c as u32);
        }
    }

    #[test]
    fn ordinary_text_is_not_refused() {
        for text in [
            "",
            "plain ascii\t\r\n",
            "zażółć gęślą jaźń ŻÓŁĆ",
            "emoji 😀 🚀 ❤\u{FE0F} 日本語 العربية עברית",
            "\u{A0}non-breaking space and \u{2013} dash",
        ] {
            assert_eq!(reject(text), Ok(()), "{text:?}");
        }
    }

    #[test]
    fn the_message_counts_characters_and_lines_not_bytes() {
        let err = reject("ąę\n\nżółć\u{200B}").unwrap_err();
        assert_eq!(
            err,
            SchemaError::Invalid(format!(
                "{HIDDEN_CHARACTER} U+200B (ZERO WIDTH SPACE) at line 3, column 5"
            ))
        );
        let first = reject("\u{202E}x\u{200B}").unwrap_err();
        assert!(first.to_string().contains("U+202E"), "{first}");
        assert!(first.to_string().contains("line 1, column 1"), "{first}");
    }

    const XSD_DOC: &str = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"a\" type=\"xs:string\"/></xs:schema>";

    #[test]
    fn a_leading_byte_order_mark_is_accepted_for_an_xsd_only() {
        let bom = format!("\u{FEFF}{XSD_DOC}");
        assert!(compile(SchemaType::Xsd, &bom).is_ok());
        assert!(SchemaType::Xsd
            .ops()
            .check_compatibility(XSD_DOC, &bom, Compatibility::Full)
            .is_ok());
        for kind in SchemaType::ALL
            .into_iter()
            .filter(|k| *k != SchemaType::Xsd)
        {
            let err = kind.ops().compile("\u{FEFF}{}").unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains("U+FEFF") && m.contains("line 1, column 1")),
                "{}: {err:?}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn a_byte_order_mark_past_the_first_character_is_refused_in_an_xsd_too() {
        let mid = XSD_DOC.replacen("<xs:element", "\u{FEFF}<xs:element", 1);
        // The BOM sits where `<xs:element` was, one column further in than the XSD alone.
        let column = XSD_DOC.find("<xs:element").unwrap() + 1;
        let err = compile(SchemaType::Xsd, &mid).unwrap_err();
        assert!(
            matches!(&err, SchemaError::Invalid(m) if m.contains("U+FEFF") && m.contains(&format!("line 1, column {column}"))),
            "{err:?}"
        );
        let twice = format!("\u{FEFF}\u{FEFF}{XSD_DOC}");
        let err = compile(SchemaType::Xsd, &twice).unwrap_err();
        assert!(
            matches!(&err, SchemaError::Invalid(m) if m.contains("line 1, column 2")),
            "{err:?}"
        );
        // A different hidden character right after the BOM counts it as column 1.
        let after = format!("\u{FEFF}\u{202E}{XSD_DOC}");
        let err = compile(SchemaType::Xsd, &after).unwrap_err();
        assert!(
            matches!(&err, SchemaError::Invalid(m) if m.contains("U+202E") && m.contains("column 2")),
            "{err:?}"
        );
    }

    #[test]
    fn deriving_from_a_stored_text_with_a_hidden_character_is_refused() {
        let allowed: BTreeSet<String> = ["a".to_string()].into();
        for kind in SchemaType::ALL {
            let err = kind
                .ops()
                .derive_subschema("{\u{202E}}", &allowed)
                .unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.starts_with(HIDDEN_CHARACTER)),
                "{}: {err:?}",
                kind.as_str()
            );
        }
    }

    /// Both ends of every range and every singleton of the table, written out
    /// here and not read from it, so a table edit cannot make this pass by itself.
    #[test]
    fn both_ends_of_every_refused_range_are_refused_and_their_neighbours_are_not() {
        let ranges: [(u32, u32); 40] = [
            (0x00, 0x08),
            (0x0B, 0x0C),
            (0x0E, 0x1F),
            (0x7F, 0x9F),
            (0xAD, 0xAD),
            (0x34F, 0x34F),
            (0x600, 0x605),
            (0x61C, 0x61C),
            (0x6DD, 0x6DD),
            (0x70F, 0x70F),
            (0x890, 0x891),
            (0x8E2, 0x8E2),
            (0x115F, 0x1160),
            (0x180B, 0x180F),
            (0x200B, 0x200F),
            (0x2028, 0x202E),
            (0x2060, 0x206F),
            (0x3164, 0x3164),
            (0xFEFF, 0xFEFF),
            (0xFFA0, 0xFFA0),
            (0xFFF9, 0xFFFB),
            (0x110BD, 0x110BD),
            (0x110CD, 0x110CD),
            (0x13430, 0x1343F),
            (0x1BCA0, 0x1BCA3),
            (0x1D173, 0x1D17A),
            (0xE0000, 0xE007F),
            (0x202A, 0x202E),
            (0x2066, 0x2069),
            (0x2060, 0x2064),
            (0x80, 0x9F),
            (0x200E, 0x200F),
            (0x2028, 0x2029),
            (0x180B, 0x180D),
            (0x180E, 0x180E),
            (0x180F, 0x180F),
            (0x115F, 0x115F),
            (0x1160, 0x1160),
            (0xFFF9, 0xFFF9),
            (0xFFFB, 0xFFFB),
        ];
        for (from, to) in ranges {
            for cp in [from, to] {
                let c = char::from_u32(cp).unwrap();
                assert!(
                    reject(&format!("a{c}b")).is_err(),
                    "U+{cp:04X} must be refused"
                );
            }
        }
        let accepted_neighbours = [
            0x20, 0x7E, 0xA0, 0xAC, 0xAE, 0x34E, 0x350, 0x5FF, 0x606, 0x61B, 0x61D, 0x6DC, 0x6DE,
            0x70E, 0x710, 0x88F, 0x892, 0x8E1, 0x8E3, 0x115E, 0x1161, 0x180A, 0x1810, 0x200A,
            0x2010, 0x202F, 0x205F, 0x2070, 0x3163, 0x3165, 0xFEFE, 0xFF00, 0xFF9F, 0xFFA1, 0xFFF8,
            0xFFFC, 0x110BC, 0x110BE, 0x110CC, 0x110CE, 0x1342F, 0x13440, 0x1BC9F, 0x1BCA4,
            0x1D172, 0x1D17B, 0xDFFFF, 0xE0080,
            // Kept on purpose: emoji presentation and CJK glyph selectors.
            0xFE00, 0xFE0E, 0xFE0F, 0xE0100, 0xE01EF,
        ];
        for cp in accepted_neighbours {
            let c = char::from_u32(cp).unwrap();
            assert!(reject(&format!("a{c}b")).is_ok(), "U+{cp:04X} must pass");
        }
        // Every refused range is also refused all the way through.
        for (from, to) in ranges {
            for cp in from..=to {
                let c = char::from_u32(cp).unwrap();
                assert!(reject(&format!("a{c}")).is_err(), "U+{cp:04X}");
            }
        }
    }

    #[test]
    fn the_table_is_sorted_and_free_of_overlaps() {
        let mut previous_end = None;
        for span in spans() {
            assert!(span.from <= span.to, "{}", span.name);
            assert!(
                previous_end.is_none_or(|end| span.from > end),
                "{}",
                span.name
            );
            assert!(!span.name.is_empty() && span.name == span.name.to_uppercase());
            previous_end = Some(span.to);
        }
    }

    #[test]
    fn columns_count_scalar_values_and_a_crlf_line_ends_at_its_lf() {
        let at = |text: &str| {
            let found = find_hidden(text, TextRules::JSON).unwrap();
            (found.line, found.column)
        };
        assert_eq!(at("ab\r\nc\u{202E}"), (2, 2));
        assert_eq!(at("ab\r\n\r\n\u{200B}"), (3, 1));
        assert_eq!(at("ab\r\u{200B}"), (1, 4), "the CR is a column of its line");
        assert_eq!(at("a\rb\u{200B}"), (1, 4), "a lone CR starts no line");
        assert_eq!(
            at("😀ą\u{200B}"),
            (1, 3),
            "an emoji is one column, not two or four"
        );
    }

    #[test]
    fn a_refused_character_in_a_detail_is_written_as_its_code_point() {
        assert_eq!(
            sanitize("field 'a\u{202E}b' and \u{200B}zero, żółć 😀"),
            "field 'a⟦U+202E⟧b' and ⟦U+200B⟧zero, żółć 😀"
        );
        assert_eq!(sanitize("tab\tstays\r\n"), "tab\tstays\r\n");
        assert_eq!(
            sanitize_error(SchemaError::Incompatible("x\u{2066}".to_string())),
            SchemaError::Incompatible("x⟦U+2066⟧".to_string())
        );
    }

    /// A kind whose comparison echoes the old text, as the real ones do in
    /// their details (field names, paths).
    struct Echo;

    impl SchemaKindOps for Echo {
        fn compile_screened(
            &self,
            _: Screened<'_>,
        ) -> Result<super::super::CompiledSchema, SchemaError> {
            unreachable!("not compiled in this test")
        }
        fn validate_metered(
            &self,
            _: &super::super::CompiledSchema,
            _: &[u8],
            _: &mut super::super::ValidationBudget,
        ) -> Result<(), SchemaError> {
            unreachable!("not validated in this test")
        }
        fn derive_screened(
            &self,
            _: Screened<'_>,
            _: &BTreeSet<String>,
        ) -> Result<String, SchemaError> {
            unreachable!("not derived in this test")
        }
        fn compare_screened(
            &self,
            old: &str,
            _: Screened<'_>,
            _: Compatibility,
        ) -> Result<(), SchemaError> {
            Err(SchemaError::Incompatible(format!("old says {old}")))
        }
    }

    #[test]
    fn a_comparison_reads_the_stored_old_text_without_its_hidden_characters() {
        let err = Echo
            .check_compatibility("na\u{202E}me", "{}", Compatibility::Full)
            .unwrap_err();
        assert_eq!(err, SchemaError::Incompatible("old says name".to_string()));
        // A refused character in a detail a kind builds is still written out.
        assert_eq!(
            sanitize_error(SchemaError::Incompatible(
                "old says na\u{202E}me".to_string()
            )),
            SchemaError::Incompatible("old says na⟦U+202E⟧me".to_string())
        );
        // The new text is screened before the comparison runs.
        let err = Echo
            .check_compatibility("{}", "na\u{202E}me", Compatibility::Full)
            .unwrap_err();
        assert!(
            matches!(&err, SchemaError::Invalid(m) if m.starts_with(HIDDEN_CHARACTER)),
            "{err:?}"
        );
    }

    #[test]
    fn the_real_kinds_keep_a_stored_hidden_character_out_of_the_comparison_detail() {
        let old = "{\"type\":\"object\",\"properties\":{\"a\u{202E}b\":{\"type\":\"string\"}},\"required\":[\"a\u{202E}b\"]}";
        let new = "{\"type\":\"object\",\"properties\":{\"a\u{202E}b\":{\"type\":\"integer\"}}}";
        let err = SchemaType::JsonSchema
            .ops()
            .check_compatibility(old, &new.replace('\u{202E}', "x"), Compatibility::Full)
            .unwrap_err();
        let shown = format!("{err:?}{err}");
        assert!(shown.chars().all(|c| refused_name(c).is_none()), "{shown}");
        // The same text on the NEW side is refused outright.
        assert!(SchemaType::JsonSchema
            .ops()
            .check_compatibility(old, new, Compatibility::Full)
            .is_err());
    }

    #[test]
    fn strip_hidden_removes_what_find_hidden_finds_and_keeps_an_allowed_bom() {
        assert_eq!(
            strip_hidden("a\u{202E}b\u{200B}c\tż😀\r\n", TextRules::JSON),
            "abc\tż😀\r\n"
        );
        assert_eq!(
            strip_hidden("\u{FEFF}a\u{FEFF}", TextRules::XML),
            "\u{FEFF}a"
        );
        assert_eq!(strip_hidden("\u{FEFF}a", TextRules::JSON), "a");
        for (text, rules) in [
            ("a\u{2066}\u{E0041}b", TextRules::JSON),
            ("\u{FEFF}x\u{3164}", TextRules::XML),
        ] {
            assert!(
                find_hidden(&strip_hidden(text, rules), rules).is_none(),
                "{text:?}"
            );
        }
    }

    fn merged(text: &str) -> Option<String> {
        collapsed_key(&reveal(text, TextRules::JSON), TextRules::JSON)
    }

    #[test]
    fn collapsed_key_names_a_key_the_removal_would_duplicate_and_only_that() {
        let merge = "{\"a\":{\"pesel\":1,\"pesel\u{200B}\":2}}";
        assert_eq!(merged(merge), Some("pesel".to_string()));
        assert_eq!(
            merged("{\"pesel\":1,\"pe\u{200B}sel\":2,\"x\":3}"),
            Some("pesel".to_string())
        );
        // Distinct after removal, not JSON, or already duplicated as stored.
        assert_eq!(merged("{\"pe\u{200B}sel\":1,\"x\":2}"), None);
        assert_eq!(merged("message M { string a\u{200B}b = 1; }"), None);
        assert_eq!(merged("{\"a\":1,\"a\":2,\"b\u{200B}\":1}"), None);
    }

    #[test]
    fn an_old_duplicate_does_not_hide_a_new_merge_and_is_not_one() {
        // A duplicate the text already had, and a real merge next to it.
        assert_eq!(
            merged("{\"a\":1,\"a\":2,\"pesel\":1,\"pesel\u{200B}\":2}"),
            Some("pesel".to_string())
        );
        // Three spellings of one name: the repeat is old, the other spelling is the merge.
        assert_eq!(
            merged("{\"a\":1,\"a\":2,\"a\u{200B}\":3}"),
            Some("a".to_string())
        );
        // The merge sits deeper than the old duplicate.
        assert_eq!(
            merged("{\"x\":{\"k\":1,\"k\":2},\"y\":{\"n\":1,\"n\u{2060}\":2}}"),
            Some("n".to_string())
        );
        // A blank character between tokens is not part of any name and must not stop the read
        // before a duplicate that stands after it (the old reading called that one new).
        assert_eq!(
            merged("{\"a\":1\u{200B},\"b\":1,\"b\":2,\"c\u{200B}\":3}"),
            None
        );
        assert_eq!(
            merged("{\"a\":1 \u{200B}, \"b\":1, \"b\u{200B}\":2}"),
            Some("b".to_string())
        );
    }

    fn spelled(text: &str, rules: TextRules) -> Vec<String> {
        hidden_spellings(&reveal(text, rules), rules).shown
    }

    #[test]
    fn hidden_spellings_lists_each_name_or_value_with_a_refused_character_once() {
        assert_eq!(
            spelled(
                "{\"pac\u{202E}jent\":[\"a\u{200B}b\",\"pac\u{202E}jent\",\"ok\"]}",
                TextRules::JSON
            ),
            vec!["pac⟦U+202E⟧jent".to_string(), "a⟦U+200B⟧b".to_string()]
        );
        assert!(spelled("\u{FEFF}<xs:schema/>", TextRules::XML).is_empty());
    }

    #[test]
    fn hidden_spellings_leave_out_prose_and_keep_names_in_every_format() {
        // JSON: a title or description is read by people, a property or enum value by data.
        assert_eq!(
            spelled(
                "{\"title\":\"a\u{200B}b\",\"description\":\"c\u{200B}d\",\"properties\":{\"p\u{200B}q\":{\"description\":\"x\u{200B}\"}},\"enum\":[\"e\u{200B}f\"]}",
                TextRules::JSON
            ),
            vec!["p⟦U+200B⟧q".to_string(), "e⟦U+200B⟧f".to_string()]
        );
        // A property that happens to be called "description" is still a name.
        assert_eq!(
            spelled(
                "{\"properties\":{\"description\":{\"properties\":{\"n\u{200B}m\":{}}}}}",
                TextRules::JSON
            ),
            vec!["n⟦U+200B⟧m".to_string()]
        );
        // XSD: attribute values, not documentation or comments.
        assert_eq!(
            spelled(
                "<xs:schema><!-- c\u{200B} --><xs:element name=\"po\u{200B}x\"><xs:annotation><xs:documentation>d\u{200B}</xs:documentation></xs:annotation></xs:element></xs:schema>",
                TextRules::XML
            ),
            vec!["po⟦U+200B⟧x".to_string()]
        );
        // Protobuf and Thrift: identifiers, not comments or string options.
        assert_eq!(
            spelled(
                "// c\u{200B}\nmessage M\u{200B} { string a = 1; } option x = \"s\u{200B}\"; /* b\u{200B} */",
                TextRules::PROTO
            ),
            vec!["M⟦U+200B⟧".to_string()]
        );
        assert_eq!(
            spelled(
                "# c\u{200B}\nstruct S\u{200B} { 1: i32 a = 1 } const string s = \"v\u{200B}\"",
                TextRules::THRIFT
            ),
            vec!["S⟦U+200B⟧".to_string()]
        );
    }

    #[test]
    fn the_list_is_capped_but_the_total_is_true() {
        let keys: Vec<String> = (0..25).map(|i| format!("\"k{i}\u{200B}\":1")).collect();
        let text = format!("{{{}}}", keys.join(","));
        let found = hidden_spellings(&reveal(&text, TextRules::JSON), TextRules::JSON);
        assert_eq!(found.shown.len(), MAX_LISTED_SPELLINGS);
        assert_eq!(found.total, 25);
        assert_eq!(found.shown[0], "k0⟦U+200B⟧");
    }

    /// One text per format that names a refused character only through an escape,
    /// the same text without it, and the code point and place the refusal gives.
    struct Escaped {
        kind: SchemaType,
        bad: &'static str,
        ok: &'static str,
        code: &'static str,
        /// The escape itself; the refusal must point at its first character.
        needle: &'static str,
    }

    fn escaped_cases() -> Vec<Escaped> {
        vec![
            Escaped {
                kind: SchemaType::JsonSchema,
                bad: "{\n  \"type\": \"object\",\n  \"properties\": {\"pac\\u202Ejent\": {\"type\": \"string\"}}\n}",
                ok: "{\n  \"type\": \"object\",\n  \"properties\": {\"pacjent\": {\"type\": \"string\"}}\n}",
                code: "U+202E",
                needle: "\\u202E",
            },
            Escaped {
                kind: SchemaType::JsonSchema,
                bad: "{\"type\":\"object\",\"description\":\"x\\ud83d\\ude00\\udb40\\udc41\"}",
                ok: "{\"type\":\"object\",\"description\":\"x\\ud83d\\ude00\"}",
                code: "U+E0041",
                needle: "\\udb40\\udc41",
            },
            Escaped {
                kind: SchemaType::Avro,
                bad: "{\"type\":\"record\",\"name\":\"R\",\"fields\":[{\"name\":\"a\",\"type\":\"string\",\"default\":\"\\u200b\"}]}",
                ok: "{\"type\":\"record\",\"name\":\"R\",\"fields\":[{\"name\":\"a\",\"type\":\"string\",\"default\":\"x\"}]}",
                code: "U+200B",
                needle: "\\u200b",
            },
            Escaped {
                kind: SchemaType::Hl7v2Profile,
                bad: "{\n  \"required_segments\": [\"P\\u200bD\"]\n}",
                ok: "{\n  \"required_segments\": [\"PID\"]\n}",
                code: "U+200B",
                needle: "\\u200b",
            },
            Escaped {
                kind: SchemaType::Xsd,
                bad: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"po&#x202E;x\" type=\"xs:string\"/>\n</xs:schema>",
                ok: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"pox\" type=\"xs:string\"/>\n</xs:schema>",
                code: "U+202E",
                needle: "&#x202E;",
            },
            Escaped {
                kind: SchemaType::Xsd,
                bad: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"a\" type=\"xs:string\"><xs:annotation><xs:documentation>x&#8238;y</xs:documentation></xs:annotation></xs:element>\n</xs:schema>",
                ok: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"a\" type=\"xs:string\"><xs:annotation><xs:documentation>xy</xs:documentation></xs:annotation></xs:element>\n</xs:schema>",
                code: "U+202E",
                needle: "&#8238;",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto3\";\noption java_package = \"a\\xe2\\x80\\xaeb\";\nmessage M { int32 a = 1; }\n",
                ok: "syntax = \"proto3\";\noption java_package = \"ab\";\nmessage M { int32 a = 1; }\n",
                code: "U+202E",
                needle: "\\xe2\\x80\\xae",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto3\";\noption java_package = \"a\\342\\200\\256b\";\nmessage M { int32 a = 1; }\n",
                ok: "syntax = \"proto3\";\noption java_package = \"ab\";\nmessage M { int32 a = 1; }\n",
                code: "U+202E",
                needle: "\\342\\200\\256",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto3\";\noption java_package = \"a\\xE2\" '\\x80\\xAEb';\nmessage M { int32 a = 1; }\n",
                ok: "syntax = \"proto3\";\noption java_package = \"a\" 'b';\nmessage M { int32 a = 1; }\n",
                code: "U+202E",
                needle: "\\xE2\" '\\x80\\xAE",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto2\";\nmessage M { optional string a = 1 [default = \"\\xE2\" /*x*/ \"\\x80\\xAE\"]; }\n",
                ok: "syntax = \"proto2\";\nmessage M { optional string a = 1 [default = \"x\" /*x*/ \"y\"]; }\n",
                code: "U+202E",
                needle: "\\xE2\" /*x*/ \"\\x80\\xAE",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto2\";\nmessage M { optional string bytes = 1 [default = \"\\xE2\\x80\\xAE\"]; }\n",
                ok: "syntax = \"proto2\";\nmessage M { optional string bytes = 1 [default = \"x\"]; }\n",
                code: "U+202E",
                needle: "\\xE2\\x80\\xAE",
            },
            Escaped {
                kind: SchemaType::Xsd,
                bad: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"><xs:enumeration value=\"a<!--\"/><xs:enumeration value=\"&#x202E;x\"/><xs:enumeration value=\"-->\"/></xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"t\"/></xs:schema>",
                ok: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"><xs:enumeration value=\"a<!--\"/><xs:enumeration value=\"x\"/><xs:enumeration value=\"-->\"/></xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"t\"/></xs:schema>",
                code: "U+202E",
                needle: "&#x202E;",
            },
            Escaped {
                kind: SchemaType::Xsd,
                bad: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"><xs:enumeration value=\"a<?\"/><xs:enumeration value=\"&#x202E;x\"/><xs:enumeration value=\"?>\"/></xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"t\"/></xs:schema>",
                ok: "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"><xs:enumeration value=\"a<?\"/><xs:enumeration value=\"x\"/><xs:enumeration value=\"?>\"/></xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"t\"/></xs:schema>",
                code: "U+202E",
                needle: "&#x202E;",
            },
            Escaped {
                kind: SchemaType::Protobuf,
                bad: "syntax = \"proto3\";\noption java_package = \"a\\342\\200\\248b\";\nmessage M { int32 a = 1; }\n",
                ok: "syntax = \"proto3\";\noption java_package = \"ab\";\nmessage M { int32 a = 1; }\n",
                code: "U+2028",
                needle: "\\342\\200\\248",
            },
        ]
    }

    #[test]
    fn an_escape_that_decodes_to_a_refused_character_is_refused_in_every_format_that_has_one() {
        for c in escaped_cases() {
            assert!(
                compile(c.kind, c.ok).is_ok(),
                "{} control text must compile: {:?}",
                c.kind.as_str(),
                compile(c.kind, c.ok)
            );
            let err = c.kind.ops().compile(c.bad).unwrap_err();
            let message = err.to_string();
            let (line, column) = marker_position(&c.bad.replacen(c.needle, &MARK.to_string(), 1));
            assert!(
                message.starts_with(&format!("invalid schema: {HIDDEN_CHARACTER} {} (", c.code))
                    && message.ends_with(&format!(
                        ") at line {line}, column {column}{WRITTEN_AS_ESCAPE}"
                    )),
                "{} {:?}: {message}",
                c.kind.as_str(),
                c.bad
            );
        }
    }

    const XSD_OPEN: &str = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">";

    #[test]
    fn escapes_below_u0100_are_the_formats_own_controls_and_bytes_and_pass() {
        let avro = "{\"type\":\"record\",\"name\":\"R\",\"fields\":[{\"name\":\"b\",\"type\":\"bytes\",\"default\":\"\\u0000\\u00ff\\u001f\"}]}";
        let json = "{\"type\":\"string\",\"pattern\":\"^[^\\u0000-\\u001F]*$\"}";
        let xsd = |pattern: &str| {
            format!("{XSD_OPEN}<xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\"><xs:pattern value=\"{pattern}\"/></xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"xs:string\"/></xs:schema>")
        };
        let proto2 = "syntax = \"proto2\";\nmessage M { optional bytes a = 1 [default = \"\\x00\\x01\"]; }\n";
        for (kind, text) in [
            (SchemaType::Avro, avro.to_string()),
            (SchemaType::JsonSchema, json.to_string()),
            (SchemaType::Xsd, xsd("[^&#x7F;-&#x9F;]*")),
            (SchemaType::Xsd, xsd("[^&#x1;-&#x1F;]")),
            (SchemaType::Xsd, xsd("a&#10;b&#x9;c&#13;d")),
            (SchemaType::Protobuf, proto2.to_string()),
        ] {
            assert!(compile(kind, &text).is_ok(), "{}: {text}: {:?}", kind.as_str(), compile(kind, &text));
        }
    }

    #[test]
    fn a_stored_text_with_only_such_escapes_still_compares_and_is_not_listed() {
        let old = "{\"type\":\"string\",\"pattern\":\"^[^\\u0000-\\u001F]*$\"}";
        let kept = SchemaType::JsonSchema
            .ops()
            .check_compatibility_noting(old, old, Compatibility::Full)
            .unwrap();
        assert_eq!(kept.total, 0);
        assert!(find_hidden(old, TextRules::JSON).is_none());
    }

    #[test]
    fn u00ad_and_u0085_are_refused_as_escapes_in_every_format() {
        let xsd = |reference: &str| format!("{XSD_OPEN}<xs:element name=\"a{reference}b\" type=\"xs:string\"/></xs:schema>");
        for (kind, text, code) in [
            (SchemaType::JsonSchema, "{\"description\":\"\\u00ad\"}".to_string(), "U+00AD"),
            (SchemaType::JsonSchema, "{\"description\":\"\\u0085\"}".to_string(), "U+0085"),
            (SchemaType::Avro, "{\"type\":\"string\",\"doc\":\"\\u0085\"}".to_string(), "U+0085"),
            (SchemaType::Xsd, xsd("&#xAD;"), "U+00AD"),
            (SchemaType::Xsd, xsd("&#133;"), "U+0085"),
            (SchemaType::Protobuf, "syntax = \"proto3\";\noption java_package = \"a\\xc2\\xadb\";\nmessage M { int32 a = 1; }\n".to_string(), "U+00AD"),
            (SchemaType::Protobuf, "syntax = \"proto3\";\noption java_package = \"a\\302\\205b\";\nmessage M { int32 a = 1; }\n".to_string(), "U+0085"),
        ] {
            let message = compile(kind, &text).unwrap_err().to_string();
            assert!(
                message.contains(&format!("{code} (")) && message.ends_with(WRITTEN_AS_ESCAPE),
                "{} {code}: {message}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn an_avro_escape_above_the_byte_range_is_refused_like_any_other() {
        let wide = "{\"type\":\"record\",\"name\":\"R\",\"fields\":[{\"name\":\"b\",\"type\":\"bytes\",\"default\":\"\\u2066\"}]}";
        assert!(compile(SchemaType::Avro, wide).is_err());
        let raw = wide.replace("\\u2066", "\u{0}");
        assert!(compile(SchemaType::Avro, &raw).is_err(), "a raw control stays refused");
    }

    #[test]
    fn xml_references_are_read_to_their_last_digit() {
        let hit = |reference: &str| {
            find_hidden(&format!("<a>{reference}</a>"), TextRules::XML)
                .map(|f| (f.code_point(), f.escape))
        };
        for reference in ["&#x202E;", "&#x0000000202E;", "&#00000000008238;", "&#X000202e;"] {
            assert_eq!(hit(reference), Some(("U+202E".to_string(), true)), "{reference}");
        }
        // Too large for a code point: not a reference to anything refused, and no panic.
        for reference in ["&#x110000000000000000;", "&#99999999999999999999;", "&#x110000;"] {
            assert_eq!(hit(reference), None, "{reference}");
        }
    }

    #[test]
    fn a_protobuf_octal_escape_wraps_and_takes_decimal_digits_like_the_lexer() {
        // `\248` is 0xA8 for the lexer (2*64+4*8+8 = 168), not `\24` followed by an 8.
        let text = "option x = \"\\342\\200\\248\";";
        let found = find_hidden(text, TextRules::PROTO).unwrap();
        assert_eq!((found.code_point().as_str(), found.escape), ("U+2028", true));
        // `\428` is 0x18, a control below U+0100, and not an 8 after `\42`.
        assert!(find_hidden("option x = \"\\428\";", TextRules::PROTO).is_none());
        assert!(find_hidden("option x = \"\\342\\200\\24\" ;", TextRules::PROTO).is_none());
    }

    #[test]
    fn a_refusal_lists_every_place_raw_and_escaped_together() {
        let text = "{\n  \"a\u{200B}\": \"x\\u202Ey\",\n  \"b\": \"\u{2066}\\u0085\"\n}";
        let message = compile(SchemaType::JsonSchema, text).unwrap_err().to_string();
        assert_eq!(
            message,
            format!(
                "invalid schema: {HIDDEN_CHARACTER} U+200B (ZERO WIDTH SPACE) at line 2, column 5; \
                 U+202E (RIGHT-TO-LEFT OVERRIDE) at line 2, column 11{WRITTEN_AS_ESCAPE}; \
                 U+2066 (LEFT-TO-RIGHT ISOLATE) at line 3, column 9; \
                 U+0085 (NEXT LINE) at line 3, column 10{WRITTEN_AS_ESCAPE}"
            )
        );
        let many = format!("\"{}\"", "\u{200B}".repeat(MAX_REFUSED_PLACES + 7));
        let message = compile(SchemaType::JsonSchema, &many).unwrap_err().to_string();
        assert_eq!(message.matches("U+200B").count(), MAX_REFUSED_PLACES, "{message}");
        assert!(message.ends_with("; and 7 more"), "{message}");
    }

    #[test]
    fn the_places_of_a_large_text_are_found_in_one_pass() {
        let text = "\u{200B}\n".repeat(200_000);
        let started = std::time::Instant::now();
        assert_eq!(find_all_hidden(&text, TextRules::JSON).len(), 200_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_backslash_that_is_not_an_escape_of_the_format_is_left_alone() {
        // An escaped backslash before `u202E` is the letters, not the character.
        assert!(compile(
            SchemaType::JsonSchema,
            "{\"type\":\"string\",\"pattern\":\"\\\\u202E\"}"
        )
        .is_ok());
        // A comment is not decoded by any of these formats.
        assert!(compile(
            SchemaType::Xsd,
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><!-- &#x202E; --><xs:element name=\"a\" type=\"xs:string\"/></xs:schema>"
        )
        .is_ok());
        assert!(compile(
            SchemaType::Protobuf,
            "syntax = \"proto3\";\n// \"\\x07\"\nmessage M { int32 a = 1; }\n"
        )
        .is_ok());
        // Protobuf's lexer reads `\u` as the letter u, so this is not the character.
        assert!(compile(
            SchemaType::Protobuf,
            "syntax = \"proto3\";\noption java_package = \"a\\u202eb\";\nmessage M { int32 a = 1; }\n"
        )
        .is_ok());
        // Thrift has no escape that can decode to one: `\u` is refused as an escape it does not define.
        let err = compile(
            SchemaType::Thrift,
            "namespace rs t\nstruct S { 1: string a = \"\\u202e\" }\n",
        )
        .unwrap_err();
        assert!(!err.to_string().contains(HIDDEN_CHARACTER), "{err}");
        // Tab, newline and carriage return stay usable as escapes.
        assert!(compile(
            SchemaType::Protobuf,
            "syntax = \"proto3\";\noption java_package = \"a\\tb\\n\\r\";\nmessage M { int32 a = 1; }\n"
        )
        .is_ok());
    }

    #[test]
    fn a_corrected_stored_escape_is_not_a_rename_and_is_listed() {
        let old = "{\"type\":\"object\",\"properties\":{\"pac\\u202Ejent\":{\"type\":\"string\"}},\"required\":[\"pac\\u202Ejent\"]}";
        let new = "{\"type\":\"object\",\"properties\":{\"pacjent\":{\"type\":\"string\"}},\"required\":[\"pacjent\"]}";
        let kept = SchemaType::JsonSchema
            .ops()
            .check_compatibility_noting(old, new, Compatibility::Full)
            .unwrap();
        assert_eq!(kept.shown, vec!["pac⟦U+202E⟧jent".to_string()]);
    }

    #[test]
    fn hidden_spellings_leave_out_patterns_labels_and_string_defaults() {
        assert_eq!(
            spelled(
                "{\"$schema\":\"s\u{200B}\",\"$id\":\"i\u{200B}\",\"properties\":{\"p\":{\"pattern\":\"a\u{200B}\",\"format\":\"f\u{200B}\"}}}",
                TextRules::JSON
            ),
            Vec::<String>::new()
        );
        assert!(spelled(
            "message M { optional string a = 1 [default = \"d\u{200B}\"]; }",
            TextRules::PROTO
        )
        .is_empty());
        assert!(spelled(
            "struct S { 1: string a = \"d\u{200B}\" }",
            TextRules::THRIFT
        )
        .is_empty());
    }

    #[test]
    fn xml_spellings_do_not_search_for_a_close_from_every_comment_opener_inside_a_tag() {
        let text = format!("<a {}", "<!--".repeat(60_000));
        let started = std::time::Instant::now();
        assert!(hidden_spellings(&text, TextRules::XML).shown.is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_bytes_default_is_not_text_and_a_regular_expression_is_never_shown_decoded() {
        let xsd = |element: &str| format!("{XSD_OPEN}<xs:simpleType name=\"t\"><xs:restriction base=\"xs:string\">{element}</xs:restriction></xs:simpleType><xs:element name=\"a\" type=\"t\"/></xs:schema>");
        let ok = [
            (SchemaType::Protobuf, "syntax = \"proto2\";\nmessage M { optional bytes magic = 1 [default = \"\\xEF\\xBB\\xBF\"]; }\n".to_string()),
            (SchemaType::JsonSchema, "{\"type\":\"string\",\"pattern\":\"^[^\\u200B\\uFEFF\\u202E]*$\"}".to_string()),
            (SchemaType::Xsd, xsd("<xs:pattern value=\"[^&#x200B;&#xFEFF;]*\"/>")),
        ];
        for (kind, text) in &ok {
            assert!(compile(*kind, text).is_ok(), "{}: {text}: {:?}", kind.as_str(), compile(*kind, text));
            // A stored version like this is compared and listed as clean.
            let kept = kind.ops().check_compatibility_noting(text, text, Compatibility::Full).unwrap();
            assert_eq!(kept.total, 0, "{text}");
        }
        // The compiler has no `patternProperties`, the screen still reads its keys as expressions.
        let keys = "{\"patternProperties\":{\"^[^\\u200B]+$\":{}},\"x\":{\"a\\u200B\":1}}";
        let hits = find_all_hidden(keys, TextRules::JSON);
        assert_eq!(hits.len(), 1, "only the key outside patternProperties is refused");
        // Raw characters stay refused there, and an escape anywhere else is refused.
        let bad = [
            (SchemaType::JsonSchema, "{\"type\":\"string\",\"pattern\":\"^[^\u{200B}]*$\"}".to_string()),
            (SchemaType::JsonSchema, "{\"type\":\"string\",\"description\":\"\\u200B\"}".to_string()),
            (SchemaType::JsonSchema, "{\"type\":\"object\",\"properties\":{\"a\\u200B\":{\"type\":\"string\"}}}".to_string()),
            (SchemaType::Xsd, xsd("<xs:pattern value=\"[^\u{200B}]*\"/>")),
            (SchemaType::Xsd, xsd("<xs:enumeration value=\"&#x200B;\"/>")),
            (SchemaType::Xsd, xsd("<xs:pattern id=\"&#x200B;\" value=\"x\"/>")),
        ];
        for (kind, text) in &bad {
            assert!(compile(*kind, text).is_err(), "{}: {text}", kind.as_str());
        }
    }

    #[test]
    fn an_unknown_kind_reads_no_escape() {
        let unknown = TextRules { leading_bom: true, syntax: Syntax::Unknown };
        assert!(find_hidden("option x = \"\\u202e\";", unknown).is_none());
        assert!(find_hidden("a\u{202E}", unknown).is_some());
    }
}
