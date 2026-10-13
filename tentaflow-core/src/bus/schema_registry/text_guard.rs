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
// Refused:
//   - C0 controls except TAB, LF and CR (so NUL, ESC, VT, FF, BEL, ...);
//   - DEL and the C1 controls U+0080-U+009F (U+0085 NEXT LINE included);
//   - bidirectional controls: U+061C, U+200E, U+200F, U+202A-U+202E,
//     U+2066-U+2069;
//   - zero-width and invisible format characters: U+00AD, U+180E,
//     U+200B-U+200D, U+2060-U+2064;
//   - U+FEFF anywhere, including the first position: no format parser here
//     takes a byte-order mark (it is a syntax error in JSON and a refused
//     character in the others), so an editor-added BOM is refused with a
//     message naming it instead of being stripped, which would make the stored
//     bytes differ from the content hash of what the author submitted;
//   - the rest of Unicode category Cf as of Unicode 16, by block: U+0600-U+0605,
//     U+06DD, U+070F, U+0890-U+0891, U+08E2, U+2065-U+206F, U+FFF9-U+FFFB,
//     U+110BD, U+110CD, U+13430-U+1343F, U+1BCA0-U+1BCA3, U+1D173-U+1D17A and
//     the tag block U+E0000-U+E007F;
//   - U+2028 and U+2029 (line and paragraph separator): they break a line on
//     screen and in some tools but are not a line end for the parsers.
// Accepted: every other character, so Polish and other non-ASCII letters and
// plain emoji pass. Emoji joined by U+200D are refused because that character
// is listed above; variation selectors (U+FE0F) are not refused.
//
// A text stored before this screen existed (or applied from a peer, which
// stores the row as it came) is not rewritten. It fails on its next compile
// with this same refusal, which the bus reports as the "stored schema cannot be
// compiled" sentence (`bus/mod.rs` `STORED_SCHEMA_UNREADABLE`) followed by it;
// a corrected version registers as usual because the compatibility check reads
// the old text without this screen.
// =============================================================================

use super::SchemaError;

/// Stable opening of the refusal; the dashboard recognises it
/// (`schema-windows.js`) and `schema_error_phrases_are_stable` pins it.
pub(super) const HIDDEN_CHARACTER: &str =
    "the schema contains an invisible or direction-controlling character";

/// The name of `c` when a schema may not contain it.
fn refused_name(c: char) -> Option<&'static str> {
    let name = match c {
        '\t' | '\n' | '\r' => return None,
        '\u{0}'..='\u{1F}' => "CONTROL CHARACTER",
        '\u{7F}' => "DELETE",
        '\u{85}' => "NEXT LINE",
        '\u{80}'..='\u{9F}' => "C1 CONTROL CHARACTER",
        '\u{AD}' => "SOFT HYPHEN",
        '\u{61C}' => "ARABIC LETTER MARK",
        '\u{180E}' => "MONGOLIAN VOWEL SEPARATOR",
        '\u{200B}' => "ZERO WIDTH SPACE",
        '\u{200C}' => "ZERO WIDTH NON-JOINER",
        '\u{200D}' => "ZERO WIDTH JOINER",
        '\u{200E}' => "LEFT-TO-RIGHT MARK",
        '\u{200F}' => "RIGHT-TO-LEFT MARK",
        '\u{2028}' => "LINE SEPARATOR",
        '\u{2029}' => "PARAGRAPH SEPARATOR",
        '\u{202A}' => "LEFT-TO-RIGHT EMBEDDING",
        '\u{202B}' => "RIGHT-TO-LEFT EMBEDDING",
        '\u{202C}' => "POP DIRECTIONAL FORMATTING",
        '\u{202D}' => "LEFT-TO-RIGHT OVERRIDE",
        '\u{202E}' => "RIGHT-TO-LEFT OVERRIDE",
        '\u{2060}' => "WORD JOINER",
        '\u{2061}' => "FUNCTION APPLICATION",
        '\u{2062}' => "INVISIBLE TIMES",
        '\u{2063}' => "INVISIBLE SEPARATOR",
        '\u{2064}' => "INVISIBLE PLUS",
        '\u{2066}' => "LEFT-TO-RIGHT ISOLATE",
        '\u{2067}' => "RIGHT-TO-LEFT ISOLATE",
        '\u{2068}' => "FIRST STRONG ISOLATE",
        '\u{2069}' => "POP DIRECTIONAL ISOLATE",
        '\u{FEFF}' => "ZERO WIDTH NO-BREAK SPACE",
        '\u{E0000}'..='\u{E007F}' => "TAG CHARACTER",
        '\u{0600}'..='\u{0605}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{2065}'..='\u{206F}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}' => "INVISIBLE FORMAT CHARACTER",
        _ => return None,
    };
    Some(name)
}

/// Refuses the first character of `text` the module header lists, naming it
/// with its code point, its name and its 1-based line and column (columns count
/// characters, lines end at LF).
pub(super) fn reject_hidden_characters(text: &str) -> Result<(), SchemaError> {
    let mut line = 1usize;
    let mut column = 1usize;
    for c in text.chars() {
        if let Some(name) = refused_name(c) {
            return Err(SchemaError::Invalid(format!(
                "{HIDDEN_CHARACTER} U+{:04X} ({name}) at line {line}, column {column}",
                c as u32
            )));
        }
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::{SchemaError, SchemaType};
    use super::*;

    /// One character of every refused class, with the name the message gives.
    const REFUSED: [(char, &str); 17] = [
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
                        reject_hidden_characters(&format!("a{c}b")).is_err(),
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
            assert_eq!(reject_hidden_characters(text), Ok(()), "{text:?}");
        }
    }

    #[test]
    fn the_message_counts_characters_and_lines_not_bytes() {
        let err = reject_hidden_characters("ąę\n\nżółć\u{200B}").unwrap_err();
        assert_eq!(
            err,
            SchemaError::Invalid(format!(
                "{HIDDEN_CHARACTER} U+200B (ZERO WIDTH SPACE) at line 3, column 5"
            ))
        );
        let first = reject_hidden_characters("\u{202E}x\u{200B}").unwrap_err();
        assert!(first.to_string().contains("U+202E"), "{first}");
        assert!(first.to_string().contains("line 1, column 1"), "{first}");
    }

    #[test]
    fn a_byte_order_mark_is_refused_even_first() {
        for kind in SchemaType::ALL {
            let err = kind.ops().compile("\u{FEFF}{}").unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains("U+FEFF") && m.contains("line 1, column 1")),
                "{}: {err:?}",
                kind.as_str()
            );
        }
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
}
