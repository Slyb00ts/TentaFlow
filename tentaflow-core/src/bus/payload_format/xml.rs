// =============================================================================
// File: bus/payload_format/xml.rs — XML payload field format (F1)
// =============================================================================
// SUM/tentabus/POLITYKI-POL-FORMATY.md, phase F1: field addresses are the
// LOCAL NAMES of the root element's DIRECT CHILDREN only — no nested dotted
// paths (matches JSON's top-level-only granularity), no attribute
// modeling, no namespace resolution (a prefixed name like `ns:tag` is
// treated as the literal opaque string `"ns:tag"`, not resolved against a
// declared namespace URI) — all explicitly out of scope for v1 per the
// phasing table. A repeated child element name (e.g. two `<item>` siblings)
// collapses to ONE field address, same as JSON's single-object-key model —
// there is no per-occurrence addressing in v1.
//
// `list_fields`/`project` both walk the document with a single depth
// counter: depth 0 is "before the root", depth 1 is "inside the root,
// looking at its direct children", depth >= 2 is "inside some child's own
// subtree". Only Start/Empty events seen AT depth 1 are field addresses;
// everything at depth >= 2 belongs to whichever depth-1 child contains it
// and is forwarded or dropped as a whole subtree in `project`.
//
// A replaced field (mask/hash, F4 B7) keeps its element name at depth 1 and
// its subtree becomes ONE text node: the character data of the whole subtree
// (text, CDATA and resolved entity/character references, line ends
// normalized) goes through the replacer. Attributes and child markup of a
// replaced element are dropped, because they are part of its value and a
// mask that left them would not hide it. Character data that is only
// whitespace counts as empty and is not replaced.
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

use super::{FieldReplacer, FormatError, PayloadFieldFormat, ReplaceAction, ValueShape};

fn element_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.name().as_ref()).into_owned()
}

fn xml_err(e: impl std::fmt::Display) -> FormatError {
    FormatError(format!("xml: {e}"))
}

/// A depth-1 element whose value is being replaced: its subtree is read for
/// character data and written back as a single text node.
struct Replacing {
    action: ReplaceAction,
    name: String,
    text: String,
}

fn push_character_data(ev: &Event, out: &mut String) -> Result<(), FormatError> {
    match ev {
        Event::Text(t) => out.push_str(&t.xml10_content().map_err(xml_err)?),
        Event::CData(c) => out.push_str(&c.xml10_content().map_err(xml_err)?),
        Event::GeneralRef(r) => match r.resolve_char_ref().map_err(xml_err)? {
            Some(ch) => out.push(ch),
            None => match r.decode().map_err(xml_err)?.as_ref() {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "apos" => out.push('\''),
                "quot" => out.push('"'),
                // An entity the document defines itself cannot be resolved here;
                // its reference text is the stable value.
                other => {
                    out.push('&');
                    out.push_str(other);
                    out.push(';');
                }
            },
        },
        _ => {}
    }
    Ok(())
}

fn write_replaced<W: std::io::Write>(
    writer: &mut Writer<W>,
    replacer: &dyn FieldReplacer,
    done: Replacing,
) -> Result<(), FormatError> {
    writer
        .write_event(Event::Start(BytesStart::new(done.name.as_str())))
        .map_err(xml_err)?;
    // White space at the edges is the document's layout (an indented child
    // element, a line break before the closing tag), not part of the value:
    // trimmed, so a mask does not reveal a tail of it and a hash of the same
    // value does not depend on the indentation. A value that really begins
    // or ends with a space is therefore hashed without it — the one place
    // XML differs from JSON, where the string is taken as it is.
    let value = done.text.trim_matches(is_xml_space);
    if !value.is_empty() {
        let replacement = replacer.replace(done.action, value, ValueShape::Text);
        writer
            .write_event(Event::Text(BytesText::new(&replacement)))
            .map_err(xml_err)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new(done.name.as_str())))
        .map_err(xml_err)
}

fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

pub struct XmlFormat;

impl PayloadFieldFormat for XmlFormat {
    fn list_fields(&self, payload: &[u8]) -> Result<BTreeSet<String>, FormatError> {
        let mut reader = Reader::from_reader(payload);
        let mut buf = Vec::new();
        let mut depth: u32 = 0;
        let mut saw_root = false;
        let mut fields = BTreeSet::new();
        loop {
            let ev = reader.read_event_into(&mut buf).map_err(xml_err)?;
            match &ev {
                Event::Eof => break,
                Event::Start(e) => {
                    if depth == 0 {
                        if saw_root {
                            return Err(FormatError("xml: more than one root element".to_string()));
                        }
                        saw_root = true;
                    } else if depth == 1 {
                        fields.insert(element_name(e));
                    }
                    depth += 1;
                }
                Event::Empty(e) => {
                    if depth == 0 {
                        if saw_root {
                            return Err(FormatError("xml: more than one root element".to_string()));
                        }
                        saw_root = true;
                    } else if depth == 1 {
                        fields.insert(element_name(e));
                    }
                }
                Event::End(_) => {
                    if depth == 0 {
                        return Err(FormatError("xml: unmatched end tag".to_string()));
                    }
                    depth -= 1;
                }
                _ => {}
            }
            buf.clear();
        }
        if !saw_root {
            return Err(FormatError("xml: no root element".to_string()));
        }
        if depth != 0 {
            return Err(FormatError(
                "xml: unexpected end of document inside an open element".to_string(),
            ));
        }
        Ok(fields)
    }

    fn project(
        &self,
        payload: &[u8],
        visible: &BTreeSet<String>,
        replacer: Option<&dyn FieldReplacer>,
    ) -> Result<Bytes, FormatError> {
        let mut reader = Reader::from_reader(payload);
        let mut writer = Writer::new(Vec::new());
        let mut buf = Vec::new();
        let mut depth: u32 = 0;
        let mut saw_root = false;
        // `Some(d)`: we are inside a disallowed depth-1 child's subtree,
        // opened while `depth == d` (always 1) — skip every event until an
        // `End` brings `depth` back down to `d`.
        let mut skip_from: Option<u32> = None;
        // Set while inside a depth-1 child whose value the replacer takes.
        let mut replacing: Option<Replacing> = None;

        loop {
            let ev = reader.read_event_into(&mut buf).map_err(xml_err)?;
            match &ev {
                Event::Eof => break,
                Event::Start(e) => {
                    if depth == 0 {
                        if saw_root {
                            return Err(FormatError("xml: more than one root element".to_string()));
                        }
                        saw_root = true;
                        writer
                            .write_event(Event::Start(e.borrow()))
                            .map_err(xml_err)?;
                    } else if depth == 1 {
                        let name = element_name(e);
                        if !visible.contains(&name) {
                            skip_from = Some(depth);
                        } else if let Some(action) = replacer.and_then(|r| r.action(&name)) {
                            replacing = Some(Replacing {
                                action,
                                name,
                                text: String::new(),
                            });
                        } else {
                            writer
                                .write_event(Event::Start(e.borrow()))
                                .map_err(xml_err)?;
                        }
                    } else if skip_from.is_none() && replacing.is_none() {
                        writer
                            .write_event(Event::Start(e.borrow()))
                            .map_err(xml_err)?;
                    }
                    depth += 1;
                }
                Event::Empty(e) => {
                    if depth == 0 {
                        if saw_root {
                            return Err(FormatError("xml: more than one root element".to_string()));
                        }
                        saw_root = true;
                        writer.write_event(ev.borrow()).map_err(xml_err)?;
                    } else if depth == 1 {
                        let name = element_name(e);
                        if visible.contains(&name) {
                            if replacer.and_then(|r| r.action(&name)).is_some() {
                                // Nothing to replace, and its attributes are part of its value.
                                writer
                                    .write_event(Event::Empty(BytesStart::new(name.as_str())))
                                    .map_err(xml_err)?;
                            } else {
                                writer.write_event(ev.borrow()).map_err(xml_err)?;
                            }
                        }
                    } else if skip_from.is_none() && replacing.is_none() {
                        writer.write_event(ev.borrow()).map_err(xml_err)?;
                    }
                }
                Event::End(_) => {
                    if depth == 0 {
                        return Err(FormatError("xml: unmatched end tag".to_string()));
                    }
                    if depth == 1 {
                        // Closing the root.
                        writer.write_event(ev.borrow()).map_err(xml_err)?;
                        depth = 0;
                    } else {
                        depth -= 1;
                        if replacing.is_some() {
                            if depth == 1 {
                                if let (Some(done), Some(replacer)) = (replacing.take(), replacer) {
                                    write_replaced(&mut writer, replacer, done)?;
                                }
                            }
                        } else {
                            match skip_from {
                                Some(d) if d == depth => skip_from = None,
                                Some(_) => {}
                                None => {
                                    writer.write_event(ev.borrow()).map_err(xml_err)?;
                                }
                            }
                        }
                    }
                }
                // Text/CDATA/Comment/PI/Decl/DocType: only meaningful inside
                // an already-forwarded child's subtree (depth >= 2) — prolog
                // content (depth 0) and insignificant text BETWEEN direct
                // children (depth 1) are dropped, same as JSON's projection
                // never preserving whitespace between object keys. Inside a
                // replaced child only character data is kept, as its value.
                _ => {
                    if depth >= 2 && skip_from.is_none() {
                        match replacing.as_mut() {
                            Some(r) => push_character_data(&ev, &mut r.text)?,
                            None => {
                                writer.write_event(ev.borrow()).map_err(xml_err)?;
                            }
                        }
                    }
                }
            }
            buf.clear();
        }
        if !saw_root {
            return Err(FormatError("xml: no root element".to_string()));
        }
        if depth != 0 {
            return Err(FormatError(
                "xml: unexpected end of document inside an open element".to_string(),
            ));
        }
        Ok(Bytes::from(writer.into_inner()))
    }

    fn empty_projection(&self) -> Bytes {
        // Generic, schema-agnostic empty document — the original root's
        // name is unknowable here (this method takes no payload), so this
        // mirrors JSON's `{}`: valid, empty, and independent of source
        // shape.
        Bytes::from_static(b"<empty/>")
    }

    fn validate_field_name(&self, name: &str) -> Result<(), FormatError> {
        // Simplified XML `Name` check: first char a letter/underscore,
        // remaining chars letters/digits/`_`/`-`/`.`/`:` (`:` allowed since
        // a prefixed name like `ns:tag` is a valid literal field address in
        // this format — see this module's header on namespaces).
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return Err(FormatError("xml: field name must not be empty".to_string()));
        };
        if !(first.is_alphabetic() || first == '_') {
            return Err(FormatError(format!(
                "xml: field name '{name}' must start with a letter or '_'"
            )));
        }
        if !chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')) {
            return Err(FormatError(format!(
                "xml: field name '{name}' contains characters not valid in an XML element name"
            )));
        }
        Ok(())
    }
}

pub(super) static XML_FORMAT: XmlFormat = XmlFormat;

#[cfg(test)]
mod tests {
    use super::super::TestReplacer;
    use super::*;

    fn fields(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_fields_returns_direct_children_of_root() {
        let xml = b"<Patient><id>1</id><name>Doe</name></Patient>";
        let got = XML_FORMAT.list_fields(xml).unwrap();
        assert_eq!(got, fields(&["id", "name"]));
    }

    #[test]
    fn list_fields_ignores_nested_grandchildren() {
        let xml = b"<Patient><name><first>Jan</first><last>Doe</last></name></Patient>";
        let got = XML_FORMAT.list_fields(xml).unwrap();
        assert_eq!(got, fields(&["name"]));
    }

    #[test]
    fn list_fields_dedups_repeated_sibling_names() {
        let xml = b"<Order><item>a</item><item>b</item></Order>";
        let got = XML_FORMAT.list_fields(xml).unwrap();
        assert_eq!(got, fields(&["item"]));
    }

    #[test]
    fn list_fields_handles_self_closing_children() {
        let xml = b"<Patient><id>1</id><flag/></Patient>";
        let got = XML_FORMAT.list_fields(xml).unwrap();
        assert_eq!(got, fields(&["id", "flag"]));
    }

    #[test]
    fn list_fields_rejects_malformed_xml() {
        assert!(XML_FORMAT.list_fields(b"<Patient><id>1</id>").is_err());
        assert!(XML_FORMAT.list_fields(b"not xml at all").is_err());
    }

    #[test]
    fn project_keeps_only_allowed_children_with_full_subtrees() {
        let xml =
            b"<Patient><id>1</id><ssn>999-99-9999</ssn><name><first>Jan</first></name></Patient>";
        let out = XML_FORMAT
            .project(xml, &fields(&["id", "name"]), None)
            .unwrap();
        let got = XML_FORMAT.list_fields(&out).unwrap();
        assert_eq!(got, fields(&["id", "name"]));
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.contains("<first>Jan</first>"));
        assert!(!text.contains("ssn"));
    }

    #[test]
    fn project_preserves_root_attributes_and_repeated_allowed_siblings() {
        let xml = b"<Order id=\"o1\"><item>a</item><ssn>hide</ssn><item>b</item></Order>";
        let out = XML_FORMAT.project(xml, &fields(&["item"]), None).unwrap();
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.starts_with("<Order id=\"o1\">"));
        assert!(text.contains("<item>a</item>"));
        assert!(text.contains("<item>b</item>"));
        assert!(!text.contains("ssn"));
    }

    #[test]
    fn project_with_no_allowed_fields_keeps_only_root_tags() {
        let xml = b"<Patient><id>1</id></Patient>";
        let out = XML_FORMAT.project(xml, &fields(&[]), None).unwrap();
        assert_eq!(out.as_ref(), b"<Patient></Patient>");
    }

    fn project_xml(
        xml: &str,
        shown: &[&str],
        masked: &'static [&'static str],
        hashed: &'static [&'static str],
    ) -> String {
        let replacer = TestReplacer::tagging(masked, hashed);
        let out = XML_FORMAT
            .project(xml.as_bytes(), &fields(shown), Some(&replacer))
            .expect("project");
        String::from_utf8(out.to_vec()).unwrap()
    }

    #[test]
    fn four_kinds_of_field_in_one_xml_document() {
        let got = project_xml(
            "<P><id>7</id><ssn>123</ssn><card>4111</card><name>Jan</name></P>",
            &["id", "card", "name"],
            &["card"],
            &["name"],
        );
        assert_eq!(
            got,
            "<P><id>7</id><card>M[4111]</card><name>H[Jan]</name></P>"
        );
    }

    #[test]
    fn a_replaced_element_keeps_its_name_and_loses_attributes_and_markup() {
        let got = project_xml(
            r#"<P id="p1"><name lang="pl">Jan <b>Kow</b>alski</name><flag a="1"/><note/></P>"#,
            &["name", "flag", "note"],
            &["name", "flag", "note"],
            &[],
        );
        assert_eq!(
            got,
            r#"<P id="p1"><name>M[Jan Kowalski]</name><flag/><note/></P>"#
        );
    }

    #[test]
    fn replaced_text_is_unescaped_for_the_replacer_and_escaped_on_the_way_out() {
        let got = project_xml(
            "<P><a>x &amp; y &lt;z&gt; &#65;<![CDATA[ <raw> ]]></a></P>",
            &["a"],
            &[],
            &["a"],
        );
        // The replacer saw `x & y <z> A <raw>` (the layout space at the end is
        // trimmed); its output is escaped again.
        assert_eq!(got, "<P><a>H[x &amp; y &lt;z&gt; A &lt;raw&gt;]</a></P>");
    }

    #[test]
    fn every_repeated_sibling_is_replaced_and_whitespace_only_text_is_empty() {
        let got = project_xml(
            "<O>\n  <item>a</item>\n  <item>b</item>\n  <w>  \n </w>\n</O>",
            &["item", "w"],
            &["item", "w"],
            &[],
        );
        assert_eq!(got, "<O><item>M[a]</item><item>M[b]</item><w></w></O>");
    }

    /// F4 B7 review R1 P3-7: layout around nested markup is not part of the
    /// value, so the replacer gets the same text whatever the indentation.
    #[test]
    fn white_space_around_nested_markup_is_layout_not_value() {
        let compact = project_xml("<P><name><first>Jan</first></name></P>", &["name"], &["name"], &[]);
        let indented = project_xml(
            "<P><name>\n    <first>Jan</first>\n  </name></P>",
            &["name"],
            &["name"],
            &[],
        );
        assert_eq!(compact, "<P><name>M[Jan]</name></P>");
        assert_eq!(indented, compact);
        // And the mask of a value with a line break before its end tag keeps
        // its characters, not the break.
        let broken = project_xml("<P><n>Kowalski\n</n></P>", &["n"], &["n"], &[]);
        assert_eq!(broken, "<P><n>M[Kowalski]</n></P>");
    }

    #[test]
    fn a_hidden_element_is_dropped_even_when_it_is_named_for_replacing() {
        let got = project_xml("<P><a>1</a><b>2</b></P>", &["a"], &["b"], &["b"]);
        assert_eq!(got, "<P><a>1</a></P>");
    }

    #[test]
    fn replacing_leaves_unreplaced_visible_subtrees_untouched() {
        let got = project_xml(
            r#"<P><addr><street>S</street></addr><pin>1234</pin></P>"#,
            &["addr", "pin"],
            &["pin"],
            &[],
        );
        assert_eq!(
            got,
            "<P><addr><street>S</street></addr><pin>M[1234]</pin></P>"
        );
    }

    #[test]
    fn empty_projection_is_a_valid_generic_document() {
        assert_eq!(XML_FORMAT.empty_projection().as_ref(), b"<empty/>");
    }

    #[test]
    fn validate_field_name_accepts_names_and_prefixed_names() {
        assert!(XML_FORMAT.validate_field_name("patient_id").is_ok());
        assert!(XML_FORMAT.validate_field_name("ns:tag").is_ok());
        assert!(XML_FORMAT.validate_field_name("_x").is_ok());
    }

    #[test]
    fn validate_field_name_rejects_empty_and_leading_digit() {
        assert!(XML_FORMAT.validate_field_name("").is_err());
        assert!(XML_FORMAT.validate_field_name("1abc").is_err());
        assert!(XML_FORMAT.validate_field_name("has space").is_err());
    }
}
