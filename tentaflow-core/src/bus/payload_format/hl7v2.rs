// =============================================================================
// File: bus/payload_format/hl7v2.rs — HL7 v2 (ER7) payload field format (F2)
// =============================================================================
// SUM/tentabus/POLITYKI-POL-FORMATY.md, phase F2. Decided with the owner via
// AskUserQuestion on 02.09.2026:
//   - granularity: field ADDRESS only ("PID-5"), not whole-segment — matches
//     JSON's/XML's top-level-only granularity, generalized to HL7's
//     positional model. There is no component/subcomponent addressing
//     ("PID-5.1") in v1, and no per-OCCURRENCE addressing: a message with
//     three OBX segments has exactly one field-address space (`OBX-1`,
//     `OBX-2`, ...) shared by all three occurrences — write validation
//     unions every occurrence's fields, and a `project` blanks a
//     disallowed address at EVERY occurrence uniformly.
//   - fail-closed empty projection: always keeps a minimal MSH header
//     (never the fully-empty byte string) — an HL7 message without MSH
//     isn't a message at all, so "hide everything" still needs to remain
//     a syntactically valid (if content-free) HL7 document, the same way
//     JSON's fail-closed fallback is `{}` (a valid, empty JSON value) and
//     not literally zero bytes.
//
// MSH-1 (the field separator character itself) and MSH-2 (the encoding
// characters `^~\&`) are NOT addressable/filterable fields — they define
// how the rest of the message is split at all, so blanking or hiding them
// would corrupt every other field's addressing. `validate_field_name`
// rejects "MSH-1"/"MSH-2" outright; `project` always copies them through
// untouched regardless of the policy's allow-list.
//
// Segment terminator: the spec mandates bare CR (0x0D), but real-world
// producers routinely use LF or CRLF — this parser treats any of CR, LF,
// or CRLF as a segment boundary (`split_segments`) and always normalizes
// `project`'s OUTPUT to bare CR between segments; it does not attempt to
// preserve the original terminator byte-for-byte.
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;

use super::{FormatError, PayloadFieldFormat};

const SEGMENT_TERMINATOR: char = '\r';

fn split_segments(text: &str) -> Vec<&str> {
    text.split(['\r', '\n']).filter(|s| !s.is_empty()).collect()
}

/// Extracts a segment's 3-character id and its ordinary (non-MSH) fields.
/// `fields[0]` is `SEG-1`, `fields[1]` is `SEG-2`, etc.
fn segment_fields<'a>(seg: &'a str, fsep: char) -> Result<(String, Vec<&'a str>), FormatError> {
    let id: String = seg.chars().take(3).collect();
    if id.chars().count() != 3 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(FormatError(format!("hl7: '{id}' is not a valid segment id")));
    }
    let rest = &seg[id.len()..]; // safe: id is 3 single-byte ASCII chars
    let fields: Vec<&str> = if rest.is_empty() {
        Vec::new()
    } else if let Some(stripped) = rest.strip_prefix(fsep) {
        stripped.split(fsep).collect()
    } else {
        return Err(FormatError(format!(
            "hl7: segment '{id}' is missing the field separator after its id"
        )));
    };
    Ok((id, fields))
}

/// Extracts MSH's field separator (MSH-1) and its own fields starting at
/// MSH-2 (`msh_fields[0]` is `MSH-2`, `msh_fields[1]` is `MSH-3`, ...) — see
/// this module's header for why MSH's layout is offset by one relative to
/// every other segment.
fn parse_msh<'a>(msh_segment: &'a str) -> Result<(char, Vec<&'a str>), FormatError> {
    if !msh_segment.starts_with("MSH") {
        return Err(FormatError(
            "hl7: message does not start with an MSH segment".to_string(),
        ));
    }
    let rest = &msh_segment[3..];
    let fsep = rest
        .chars()
        .next()
        .ok_or_else(|| FormatError("hl7: MSH segment has no field separator".to_string()))?;
    let after_fsep = &rest[fsep.len_utf8()..];
    let msh_fields: Vec<&str> = after_fsep.split(fsep).collect();
    Ok((fsep, msh_fields))
}

pub struct Hl7V2Format;

impl PayloadFieldFormat for Hl7V2Format {
    fn list_fields(&self, payload: &[u8]) -> Result<BTreeSet<String>, FormatError> {
        let text =
            std::str::from_utf8(payload).map_err(|e| FormatError(format!("hl7: not valid utf-8: {e}")))?;
        let segments = split_segments(text);
        let Some(first) = segments.first() else {
            return Err(FormatError("hl7: empty message".to_string()));
        };
        let (fsep, msh_fields) = parse_msh(first)?;
        let mut out = BTreeSet::new();
        // `msh_fields[0]` is MSH-2 (encoding characters) — structural,
        // never an addressable field (see module header), so skipped here
        // exactly as `project` always copies it through.
        for (idx, _) in msh_fields.iter().enumerate().skip(1) {
            out.insert(format!("MSH-{}", idx + 2));
        }
        for seg in &segments[1..] {
            let (id, fields) = segment_fields(seg, fsep)?;
            for (idx, _) in fields.iter().enumerate() {
                out.insert(format!("{id}-{}", idx + 1));
            }
        }
        Ok(out)
    }

    fn project(&self, payload: &[u8], allowed: &BTreeSet<String>) -> Result<Bytes, FormatError> {
        let text =
            std::str::from_utf8(payload).map_err(|e| FormatError(format!("hl7: not valid utf-8: {e}")))?;
        let segments = split_segments(text);
        let Some(first) = segments.first() else {
            return Err(FormatError("hl7: empty message".to_string()));
        };
        let (fsep, msh_fields) = parse_msh(first)?;

        let mut out_segments: Vec<String> = Vec::with_capacity(segments.len());

        // MSH-1/MSH-2 are structural, never filtered (see module header).
        let msh_rendered: Vec<String> = msh_fields
            .iter()
            .enumerate()
            .map(|(idx, v)| {
                let addr = format!("MSH-{}", idx + 2);
                if idx == 0 || allowed.contains(&addr) {
                    v.to_string()
                } else {
                    String::new()
                }
            })
            .collect();
        out_segments.push(format!("MSH{fsep}{}", msh_rendered.join(&fsep.to_string())));

        for seg in &segments[1..] {
            let (id, fields) = segment_fields(seg, fsep)?;
            let rendered: Vec<String> = fields
                .iter()
                .enumerate()
                .map(|(idx, v)| {
                    let addr = format!("{id}-{}", idx + 1);
                    if allowed.contains(&addr) {
                        v.to_string()
                    } else {
                        String::new()
                    }
                })
                .collect();
            if rendered.is_empty() {
                out_segments.push(id);
            } else {
                out_segments.push(format!("{id}{fsep}{}", rendered.join(&fsep.to_string())));
            }
        }

        let mut joined = out_segments.join(&SEGMENT_TERMINATOR.to_string());
        joined.push(SEGMENT_TERMINATOR);
        Ok(Bytes::from(joined.into_bytes()))
    }

    fn empty_projection(&self) -> Bytes {
        // Minimal, syntactically valid MSH-only message with the
        // conventional default separators (`|`, `^~\&`) — the specific
        // separators of the ORIGINAL payload are unknowable here (this
        // method takes no payload), same limitation as XML's
        // `empty_projection` not knowing the original root tag name.
        Bytes::from_static(b"MSH|^~\\&\r")
    }

    fn validate_field_name(&self, name: &str) -> Result<(), FormatError> {
        if name == "MSH-1" || name == "MSH-2" {
            return Err(FormatError(format!(
                "hl7: '{name}' is the message's own field-separator/encoding-characters \
                 definition and can never be filtered"
            )));
        }
        let Some((seg_id, field_no)) = name.split_once('-') else {
            return Err(FormatError(format!(
                "hl7: '{name}' is not SEGMENT-N shaped (e.g. 'PID-5')"
            )));
        };
        if seg_id.chars().count() != 3
            || !seg_id
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Err(FormatError(format!(
                "hl7: '{seg_id}' is not a valid 3-character segment id"
            )));
        }
        if field_no.is_empty()
            || !field_no.chars().all(|c| c.is_ascii_digit())
            || field_no.starts_with('0')
        {
            return Err(FormatError(format!(
                "hl7: '{field_no}' is not a valid positive field number"
            )));
        }
        Ok(())
    }
}

pub(super) static HL7V2_FORMAT: Hl7V2Format = Hl7V2Format;

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "MSH|^~\\&|APP|FAC|RAPP|RFAC|20260902||ADT^A01|MSG1|P|2.5\rPID|1||MRN123||Doe^Jan||19800101|M\r";

    fn fields(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_fields_addresses_msh_from_msh_2_and_pid_from_pid_1() {
        let got = HL7V2_FORMAT.list_fields(SAMPLE.as_bytes()).unwrap();
        assert!(got.contains("MSH-9")); // ADT^A01
        assert!(!got.contains("MSH-1"));
        assert!(!got.contains("MSH-2"));
        assert!(got.contains("PID-1")); // 1
        assert!(got.contains("PID-3")); // MRN123
        assert!(got.contains("PID-5")); // Doe^Jan
        assert!(got.contains("PID-7")); // 19800101
    }

    #[test]
    fn list_fields_unions_repeated_segment_occurrences() {
        let msg = "MSH|^~\\&|A|B|C|D|20260902||ORU^R01|1|P|2.5\rOBX|1|ST|A||val1\rOBX|2|ST|B||val2||extra\r";
        let got = HL7V2_FORMAT.list_fields(msg.as_bytes()).unwrap();
        assert!(got.contains("OBX-1"));
        assert!(got.contains("OBX-5"));
        // Only present in the second OBX occurrence, still unioned in.
        assert!(got.contains("OBX-6"));
    }

    #[test]
    fn list_fields_rejects_message_not_starting_with_msh() {
        assert!(HL7V2_FORMAT.list_fields(b"PID|1||MRN123\r").is_err());
    }

    #[test]
    fn project_blanks_disallowed_fields_in_place_without_shifting() {
        let allowed = fields(&["PID-1", "PID-3"]);
        let out = HL7V2_FORMAT.project(SAMPLE.as_bytes(), &allowed).unwrap();
        let text = String::from_utf8(out.to_vec()).unwrap();
        // Output uses bare CR as the segment terminator, which
        // `str::lines` does not split on.
        let pid_line = split_segments(&text)
            .into_iter()
            .find(|l| l.starts_with("PID"))
            .unwrap();
        let parts: Vec<&str> = pid_line.split('|').collect();
        assert_eq!(parts[1], "1"); // PID-1 kept
        assert_eq!(parts[3], "MRN123"); // PID-3 kept
        assert_eq!(parts[5], ""); // PID-5 (name) blanked, not removed
        assert_eq!(parts[7], ""); // PID-7 (dob) blanked
        // Blanking never shifts positions: field count is unchanged.
        let orig_pid = split_segments(SAMPLE)
            .into_iter()
            .find(|l| l.starts_with("PID"))
            .unwrap();
        assert_eq!(parts.len(), orig_pid.split('|').count());
    }

    #[test]
    fn project_always_preserves_msh_1_and_msh_2_regardless_of_policy() {
        let allowed: BTreeSet<String> = BTreeSet::new(); // nothing allowed
        let out = HL7V2_FORMAT.project(SAMPLE.as_bytes(), &allowed).unwrap();
        let text = String::from_utf8(out.to_vec()).unwrap();
        let msh_line = split_segments(&text)[0];
        assert!(msh_line.starts_with("MSH|^~\\&|"));
        // Everything past MSH-2 is blanked, nothing is dropped.
        assert_eq!(msh_line.split('|').count(), split_segments(SAMPLE)[0].split('|').count());
    }

    #[test]
    fn empty_projection_is_a_minimal_valid_msh_message() {
        let out = HL7V2_FORMAT.empty_projection();
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.starts_with("MSH|^~\\&"));
        // Round-trips through list_fields without erroring — it IS a
        // parseable, if content-free, HL7 message.
        assert!(HL7V2_FORMAT.list_fields(out.as_ref()).is_ok());
    }

    #[test]
    fn validate_field_name_rejects_msh_1_and_msh_2() {
        assert!(HL7V2_FORMAT.validate_field_name("MSH-1").is_err());
        assert!(HL7V2_FORMAT.validate_field_name("MSH-2").is_err());
    }

    #[test]
    fn validate_field_name_accepts_well_formed_addresses_rejects_malformed() {
        assert!(HL7V2_FORMAT.validate_field_name("PID-5").is_ok());
        assert!(HL7V2_FORMAT.validate_field_name("OBX-11").is_ok());
        assert!(HL7V2_FORMAT.validate_field_name("PID").is_err());
        assert!(HL7V2_FORMAT.validate_field_name("PID-0").is_err());
        assert!(HL7V2_FORMAT.validate_field_name("PID-05").is_err());
        assert!(HL7V2_FORMAT.validate_field_name("pid-5").is_err());
        assert!(HL7V2_FORMAT.validate_field_name("PIDX-5").is_err());
    }
}
