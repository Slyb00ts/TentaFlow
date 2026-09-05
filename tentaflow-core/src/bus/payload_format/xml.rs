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
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

use super::{FormatError, PayloadFieldFormat};

fn element_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.name().as_ref()).into_owned()
}

fn xml_err(e: quick_xml::Error) -> FormatError {
    FormatError(format!("xml: {e}"))
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

    fn project(&self, payload: &[u8], allowed: &BTreeSet<String>) -> Result<Bytes, FormatError> {
        let mut reader = Reader::from_reader(payload);
        let mut writer = Writer::new(Vec::new());
        let mut buf = Vec::new();
        let mut depth: u32 = 0;
        let mut saw_root = false;
        // `Some(d)`: we are inside a disallowed depth-1 child's subtree,
        // opened while `depth == d` (always 1) — skip every event until an
        // `End` brings `depth` back down to `d`.
        let mut skip_from: Option<u32> = None;

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
                        if allowed.contains(&element_name(e)) {
                            writer
                                .write_event(Event::Start(e.borrow()))
                                .map_err(xml_err)?;
                        } else {
                            skip_from = Some(depth);
                        }
                    } else if skip_from.is_none() {
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
                        if allowed.contains(&element_name(e)) {
                            writer.write_event(ev.borrow()).map_err(xml_err)?;
                        }
                    } else if skip_from.is_none() {
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
                        match skip_from {
                            Some(d) if d == depth => skip_from = None,
                            Some(_) => {}
                            None => {
                                writer.write_event(ev.borrow()).map_err(xml_err)?;
                            }
                        }
                    }
                }
                // Text/CDATA/Comment/PI/Decl/DocType: only meaningful inside
                // an already-forwarded child's subtree (depth >= 2) — prolog
                // content (depth 0) and insignificant text BETWEEN direct
                // children (depth 1) are dropped, same as JSON's projection
                // never preserving whitespace between object keys.
                _ => {
                    if depth >= 2 && skip_from.is_none() {
                        writer.write_event(ev.borrow()).map_err(xml_err)?;
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
        let out = XML_FORMAT.project(xml, &fields(&["id", "name"])).unwrap();
        let got = XML_FORMAT.list_fields(&out).unwrap();
        assert_eq!(got, fields(&["id", "name"]));
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.contains("<first>Jan</first>"));
        assert!(!text.contains("ssn"));
    }

    #[test]
    fn project_preserves_root_attributes_and_repeated_allowed_siblings() {
        let xml = b"<Order id=\"o1\"><item>a</item><ssn>hide</ssn><item>b</item></Order>";
        let out = XML_FORMAT.project(xml, &fields(&["item"])).unwrap();
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.starts_with("<Order id=\"o1\">"));
        assert!(text.contains("<item>a</item>"));
        assert!(text.contains("<item>b</item>"));
        assert!(!text.contains("ssn"));
    }

    #[test]
    fn project_with_no_allowed_fields_keeps_only_root_tags() {
        let xml = b"<Patient><id>1</id></Patient>";
        let out = XML_FORMAT.project(xml, &fields(&[])).unwrap();
        assert_eq!(out.as_ref(), b"<Patient></Patient>");
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
