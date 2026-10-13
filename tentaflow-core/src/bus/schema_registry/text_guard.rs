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
// A text stored before this screen existed (or applied from a peer, which
// stores the row as it came) is not rewritten. It fails on its next compile
// with this same refusal, which the bus reports as the "stored schema cannot be
// compiled" sentence (`bus/mod.rs` `STORED_SCHEMA_UNREADABLE`) followed by it;
// `registry::warn_about_stored_hidden_characters` lists such versions in the log
// at startup. A corrected version registers as usual: the compatibility check
// reads the old text without this screen, and a detail it builds from that text
// is passed through `sanitize`, so a refused character never reaches the UI or
// the audit trail as itself.
// =============================================================================

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

/// The first refused character of a text and where it stands.
pub(super) struct Finding {
    pub(super) character: char,
    pub(super) name: &'static str,
    pub(super) line: usize,
    pub(super) column: usize,
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
        )
    }
}

/// The first refused character of `text`; `allow_leading_bom` lets a U+FEFF at
/// the very start pass (XSD only, see the module header).
pub(super) fn find_hidden(text: &str, allow_leading_bom: bool) -> Option<Finding> {
    let mut line = 1usize;
    let mut column = 1usize;
    for (index, c) in text.chars().enumerate() {
        if let Some(name) = refused_name(c) {
            if !(allow_leading_bom && index == 0 && c == '\u{FEFF}') {
                return Some(Finding {
                    character: c,
                    name,
                    line,
                    column,
                });
            }
        }
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    None
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

pub(super) fn screen(text: &str, allow_leading_bom: bool) -> Result<Screened<'_>, SchemaError> {
    match find_hidden(text, allow_leading_bom) {
        Some(found) => Err(SchemaError::Invalid(format!("{HIDDEN_CHARACTER} {found}"))),
        None => Ok(Screened(text)),
    }
}

/// `detail` with every refused code point written as `U+XXXX`, for a message
/// built from a text the screen has not seen (the old side of a comparison).
pub(super) fn sanitize(detail: &str) -> String {
    detail
        .chars()
        .fold(String::with_capacity(detail.len()), |mut out, c| {
            if refused_name(c).is_some() {
                out.push_str(&format!("U+{:04X}", c as u32));
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
        screen(text, false).map(|_| ())
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
            let found = find_hidden(text, false).unwrap();
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
            "field 'aU+202Eb' and U+200Bzero, żółć 😀"
        );
        assert_eq!(sanitize("tab\tstays\r\n"), "tab\tstays\r\n");
        assert_eq!(
            sanitize_error(SchemaError::Incompatible("x\u{2066}".to_string())),
            SchemaError::Incompatible("xU+2066".to_string())
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
    fn a_comparison_never_echoes_a_hidden_character_of_the_stored_old_text() {
        let err = Echo
            .check_compatibility("na\u{202E}me", "{}", Compatibility::Full)
            .unwrap_err();
        assert_eq!(
            err,
            SchemaError::Incompatible("old says naU+202Eme".to_string())
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
}
