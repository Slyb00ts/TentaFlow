// =============================================================================
// File: bus/schema_registry/hl7v2_profile.rs — HL7 v2 message profile (F4 B5)
// =============================================================================
// SUM/tentabus/PLAN-F4-REST.md §B.5. HL7 v2 has no machine-readable schema
// language in common use, so a profile here is a small JSON document:
//
//   { "description": "ADT admit", "required_segments": ["MSH", "PID"],
//     "required_fields": ["PID-3", "MSH-9"] }
//
// Semantics (the single source of truth for `validate`, `derive_subschema`
// and `check_compatibility`):
//   - A message is parsed with the same ER7 parser the field policies use
//     (`payload_format::hl7v2::for_each_segment`); a message that does not
//     parse is a violation.
//   - A required segment must occur at least once.
//   - A required field `SEG-N` must be NON-EMPTY (contain a character other
//     than whitespace, and not be the HL7 explicit null `""`, which means
//     "delete the value") in EVERY occurrence of segment `SEG`. Naming a field
//     therefore also requires its segment: the profile is normalized at
//     compile time so `required_segments` always contains the segment of
//     every required field. Component-level structure (`^`, `&`) is not
//     inspected: `^^` counts as non-empty.
//   - Field addresses go through `validate_field_name`, so `MSH-1`/`MSH-2`
//     (separator and encoding characters) are refused exactly as they are in
//     field policies. Unknown keys, duplicates and oversized lists are
//     rejected, never ignored.
//
// Violation messages carry segment/field addresses and the occurrence
// number only, never field values (they reach audit rows and DLQ headers).
// =============================================================================

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::bus::payload_format::{hl7v2, FormatError, PayloadFormat};

use super::{Compatibility, CompiledSchema, SchemaError, SchemaKindOps, MAX_SCHEMA_TEXT_BYTES};

const MAX_LIST_ENTRIES: usize = 512;
const MAX_DESCRIPTION_CHARS: usize = 1000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    description: Option<String>,
    #[serde(default)]
    required_segments: Vec<String>,
    #[serde(default)]
    required_fields: Vec<String>,
}

/// A required field address already split into its segment and number, so
/// nothing after `parse_profile` has to re-parse untrusted text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldRef {
    segment: String,
    number: usize,
}

/// Normalized profile: `segments` already includes the segment of every
/// required field, so two profiles are compared and derived on sets alone.
/// `fields` maps the canonical address to its parsed parts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    description: Option<String>,
    segments: BTreeSet<String>,
    fields: BTreeMap<String, FieldRef>,
}

#[derive(Debug)]
pub struct Compiled {
    segments: Vec<String>,
    /// Segment id -> required field numbers in that segment.
    fields_by_segment: BTreeMap<String, Vec<usize>>,
}

fn invalid(msg: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(msg.into())
}

fn check_unique_and_sized(list: &[String], what: &str) -> Result<(), SchemaError> {
    if list.len() > MAX_LIST_ENTRIES {
        return Err(invalid(format!(
            "{what} lists {} entries, exceeding the {MAX_LIST_ENTRIES}-entry limit",
            list.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for item in list {
        if !seen.insert(item.as_str()) {
            return Err(invalid(format!("{what} lists '{item}' more than once")));
        }
    }
    Ok(())
}

fn parse_profile(schema_text: &str) -> Result<Profile, SchemaError> {
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(invalid(format!(
            "profile text is {} bytes, exceeding the {MAX_SCHEMA_TEXT_BYTES}-byte limit",
            schema_text.len()
        )));
    }
    let raw: RawProfile = serde_json::from_str(schema_text)
        .map_err(|e| invalid(format!("not a valid HL7 v2 profile: {e}")))?;
    if let Some(d) = &raw.description {
        if d.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid(format!(
                "description exceeds {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
    }
    check_unique_and_sized(&raw.required_segments, "required_segments")?;
    check_unique_and_sized(&raw.required_fields, "required_fields")?;

    let codec = PayloadFormat::Hl7V2.codec();
    let mut segments = BTreeSet::new();
    for seg in &raw.required_segments {
        // A segment id is valid exactly when it forms a valid field address
        // (field 3: MSH-1 and MSH-2 are refused, but MSH itself is a segment).
        codec
            .validate_field_name(&format!("{seg}-3"))
            .map_err(|_| invalid(format!("'{seg}' is not a valid 3-character segment id")))?;
        segments.insert(seg.clone());
    }
    let mut fields = BTreeMap::new();
    for field in &raw.required_fields {
        codec
            .validate_field_name(field)
            .map_err(|e| invalid(e.to_string()))?;
        let field_ref = field
            .split_once('-')
            .and_then(|(seg, n)| {
                Some(FieldRef {
                    segment: seg.to_string(),
                    number: n.parse().ok()?,
                })
            })
            .ok_or_else(|| invalid(format!("'{field}' is not a valid field address")))?;
        segments.insert(field_ref.segment.clone());
        fields.insert(field.clone(), field_ref);
    }
    Ok(Profile {
        description: raw.description,
        segments,
        fields,
    })
}

fn render(profile: &Profile) -> String {
    let mut map = Map::new();
    if let Some(d) = &profile.description {
        map.insert("description".to_string(), Value::String(d.clone()));
    }
    map.insert(
        "required_segments".to_string(),
        Value::Array(
            profile
                .segments
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    map.insert(
        "required_fields".to_string(),
        Value::Array(profile.fields.keys().cloned().map(Value::String).collect()),
    );
    Value::Object(map).to_string()
}

/// Documents valid under `data` must stay valid under `reader`: the reader
/// may not require anything the data side does not already guarantee.
fn check_reader_requires_no_more(
    reader: &Profile,
    data: &Profile,
    direction: &str,
) -> Result<(), SchemaError> {
    let join = |items: Vec<&String>| {
        items
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let extra_segments = join(reader.segments.difference(&data.segments).collect());
    let extra_fields = join(
        reader
            .fields
            .keys()
            .filter(|f| !data.fields.contains_key(*f))
            .collect(),
    );
    if extra_segments.is_empty() && extra_fields.is_empty() {
        return Ok(());
    }
    let mut parts = Vec::new();
    if !extra_segments.is_empty() {
        parts.push(format!("segments [{extra_segments}] are not guaranteed"));
    }
    if !extra_fields.is_empty() {
        parts.push(format!("fields [{extra_fields}] are not guaranteed"));
    }
    Err(SchemaError::Incompatible(format!(
        "{direction}: {}",
        parts.join("; ")
    )))
}

/// HL7 v2 sends `""` to mean "delete the value", so it carries no data.
fn is_populated(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "\"\""
}

pub struct Hl7v2ProfileOps;

impl SchemaKindOps for Hl7v2ProfileOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        let profile = parse_profile(schema_text)?;
        let mut fields_by_segment: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for field in profile.fields.values() {
            fields_by_segment
                .entry(field.segment.clone())
                .or_default()
                .push(field.number);
        }
        Ok(CompiledSchema::Hl7v2Profile(Compiled {
            segments: profile.segments.into_iter().collect(),
            fields_by_segment,
        }))
    }

    fn validate(&self, compiled: &CompiledSchema, payload: &[u8]) -> Result<(), SchemaError> {
        let CompiledSchema::Hl7v2Profile(c) = compiled else {
            return Err(invalid(
                "validate called with a non-hl7v2_profile compiled schema",
            ));
        };
        let mut seen = vec![false; c.segments.len()];
        let mut occurrences: BTreeMap<&str, usize> = BTreeMap::new();
        hl7v2::for_each_segment(payload, |id, fields, first| {
            if let Ok(idx) = c.segments.binary_search_by(|s| s.as_str().cmp(id)) {
                seen[idx] = true;
            }
            let Some(required) = c.fields_by_segment.get(id) else {
                return Ok(());
            };
            let occurrence = occurrences.entry(id).or_insert(0);
            *occurrence += 1;
            for &number in required {
                let present = number
                    .checked_sub(first)
                    .and_then(|i| fields.get(i))
                    .is_some_and(|v| is_populated(v));
                if !present {
                    return Err(FormatError(format!(
                        "{id}-{number} is required but empty or missing (segment occurrence {})",
                        *occurrence
                    )));
                }
            }
            Ok(())
        })
        .map_err(|e| SchemaError::Violation(e.to_string()))?;

        if let Some(idx) = seen.iter().position(|s| !s) {
            return Err(SchemaError::Violation(format!(
                "required segment '{}' is missing",
                c.segments[idx]
            )));
        }
        Ok(())
    }

    fn derive_subschema(
        &self,
        schema_text: &str,
        allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError> {
        let mut profile = parse_profile(schema_text)?;
        // A projected message keeps every segment and blanks the fields a
        // policy hides, so only field requirements can stop holding.
        profile.fields.retain(|f, _| allowed.contains(f));
        Ok(render(&profile))
    }

    fn check_compatibility(
        &self,
        old_schema_text: &str,
        new_schema_text: &str,
        mode: Compatibility,
    ) -> Result<(), SchemaError> {
        if mode == Compatibility::None {
            return Ok(());
        }
        let old = parse_profile(old_schema_text)?;
        let new = parse_profile(new_schema_text)?;
        // Backward: documents valid under the old profile must satisfy the
        // new one, so the new profile may require nothing extra. Forward is
        // the mirror image; full is both, i.e. equal requirement sets.
        if matches!(mode, Compatibility::Backward | Compatibility::Full) {
            check_reader_requires_no_more(
                &new,
                &old,
                "backward (the new profile requires more than the old one guarantees)",
            )?;
        }
        if matches!(mode, Compatibility::Forward | Compatibility::Full) {
            check_reader_requires_no_more(
                &old,
                &new,
                "forward (the old profile requires more than the new one guarantees)",
            )?;
        }
        Ok(())
    }
}

pub(super) static HL7V2_PROFILE_OPS: Hl7v2ProfileOps = Hl7v2ProfileOps;

#[cfg(test)]
mod tests {
    use super::*;

    const ADT: &str = "MSH|^~\\&|APP|FAC|RAPP|RFAC|20260902||ADT^A01|MSG1|P|2.5\rPID|1||MRN123||Doe^Jan||19800101|M\r";
    const PROFILE: &str = r#"{"description":"ADT admit","required_segments":["PID"],"required_fields":["PID-3","MSH-9"]}"#;

    fn compile(text: &str) -> CompiledSchema {
        HL7V2_PROFILE_OPS.compile(text).unwrap()
    }

    #[test]
    fn golden_fixtures_validate_as_declared() {
        let profile =
            crate::bus::schema_registry::fixtures::read("hl7v2_profile", "adt-a01.profile.json");
        let compiled = compile(&profile);
        for case in crate::bus::schema_registry::fixtures::cases("hl7v2_profile", "hl7") {
            let got = HL7V2_PROFILE_OPS.validate(&compiled, &case.payload);
            match &case.expect {
                None => assert!(got.is_ok(), "{}: {got:?}", case.name),
                Some(expect) => assert!(
                    matches!(&got, Err(SchemaError::Violation(m)) if m.contains(expect.as_str())),
                    "{}: expected a violation containing {expect:?}, got {got:?}",
                    case.name
                ),
            }
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn compile_rejects_malformed_profiles() {
        for (text, needle) in [
            ("not json", "not a valid HL7 v2 profile"),
            (
                r#"{"required_segments":["PID"],"extra":1}"#,
                "unknown field",
            ),
            (r#"{"required_segments":["pid"]}"#, "segment id"),
            (r#"{"required_segments":["PIDX"]}"#, "segment id"),
            (r#"{"required_fields":["MSH-1"]}"#, "MSH-1"),
            (r#"{"required_fields":["MSH-2"]}"#, "MSH-2"),
            (r#"{"required_fields":["PID"]}"#, "SEGMENT-N"),
            (r#"{"required_fields":["PID-0"]}"#, "field number"),
            (r#"{"required_fields":["PID-3","PID-3"]}"#, "more than once"),
            (r#"{"required_segments":["PID","PID"]}"#, "more than once"),
        ] {
            let err = HL7V2_PROFILE_OPS.compile(text).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains(needle)),
                "{text}: {err:?}"
            );
        }
    }

    #[test]
    fn compile_bounds_the_field_number_instead_of_panicking() {
        for address in [
            "PID-99999999999999999999",
            "PID-1000",
            "PID-18446744073709551616",
        ] {
            let text = format!(r#"{{"required_fields":["{address}"]}}"#);
            let err = HL7V2_PROFILE_OPS.compile(&text).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains("maximum")),
                "{address}: {err:?}"
            );
        }
        assert!(HL7V2_PROFILE_OPS
            .compile(r#"{"required_fields":["PID-999"]}"#)
            .is_ok());
    }

    #[test]
    fn explicit_null_does_not_satisfy_a_required_field() {
        let c = compile(PROFILE);
        let nulled = ADT.replace("MRN123", "\"\"");
        let err = HL7V2_PROFILE_OPS
            .validate(&c, nulled.as_bytes())
            .unwrap_err();
        assert!(matches!(&err, SchemaError::Violation(m) if m.contains("PID-3 is required")));
        // A value that merely contains quotes is data.
        let quoted = ADT.replace("MRN123", "\"x\"");
        assert!(HL7V2_PROFILE_OPS.validate(&c, quoted.as_bytes()).is_ok());
    }

    #[test]
    fn every_msh_occurrence_is_numbered_from_msh_3() {
        let c = compile(r#"{"required_fields":["MSH-9"]}"#);
        let fine =
            "MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\rMSH|^~\\&|A|B|C|D|2026||ADT^A01|2|P|2.5\r";
        // The second MSH has MSH-9 empty but MSH-10 set: a shifted numbering
        // would read MSH-10 as MSH-9 and let it through.
        let shifted = "MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\rMSH|^~\\&|A|B|C|D|2026|||2|P|2.5\r";
        assert!(HL7V2_PROFILE_OPS.validate(&c, fine.as_bytes()).is_ok());
        let err = HL7V2_PROFILE_OPS
            .validate(&c, shifted.as_bytes())
            .unwrap_err();
        assert!(
            matches!(&err, SchemaError::Violation(m) if m.contains("MSH-9") && m.contains("occurrence 2")),
            "{err:?}"
        );
    }

    #[test]
    fn a_malformed_segment_id_is_reported_without_echoing_it() {
        let c = compile(PROFILE);
        // One id with a character a segment id cannot have, one without a
        // field separator after a well-formed id.
        for segment in ["S-3CRET|x", "S3CRET9|x"] {
            let msg = format!("MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\r{segment}\r");
            let err = HL7V2_PROFILE_OPS.validate(&c, msg.as_bytes()).unwrap_err();
            assert!(matches!(&err, SchemaError::Violation(_)));
            assert!(
                !err.to_string().contains("S-3") && !err.to_string().contains("S3C"),
                "{err}"
            );
        }
    }

    #[test]
    fn compile_rejects_oversized_input() {
        let many: Vec<String> = (0..=MAX_LIST_ENTRIES)
            .map(|i| format!("\"PID-{}\"", i + 1))
            .collect();
        let text = format!("{{\"required_fields\":[{}]}}", many.join(","));
        assert!(matches!(
            HL7V2_PROFILE_OPS.compile(&text),
            Err(SchemaError::Invalid(m)) if m.contains("entry limit")
        ));
        let long = format!(
            "{{\"description\":\"{}\"}}",
            "x".repeat(MAX_DESCRIPTION_CHARS + 1)
        );
        assert!(HL7V2_PROFILE_OPS.compile(&long).is_err());
        let huge = " ".repeat(MAX_SCHEMA_TEXT_BYTES + 1);
        assert!(HL7V2_PROFILE_OPS.compile(&huge).is_err());
    }

    #[test]
    fn empty_profile_is_valid_and_only_requires_a_parsable_message() {
        let c = compile("{}");
        assert!(HL7V2_PROFILE_OPS.validate(&c, ADT.as_bytes()).is_ok());
        assert!(matches!(
            HL7V2_PROFILE_OPS.validate(&c, b"not hl7"),
            Err(SchemaError::Violation(_))
        ));
    }

    #[test]
    fn validate_accepts_a_conforming_message() {
        let c = compile(PROFILE);
        assert!(HL7V2_PROFILE_OPS.validate(&c, ADT.as_bytes()).is_ok());
    }

    #[test]
    fn validate_rejects_missing_segment_empty_field_and_missing_field() {
        let c = compile(PROFILE);
        let no_pid = "MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\r";
        let empty_pid3 = ADT.replace("MRN123", "");
        let short_pid = "MSH|^~\\&|A|B|C|D|2026||ADT^A01|1|P|2.5\rPID|1\r";
        let no_msh9 = "MSH|^~\\&|A|B|C|D|2026\rPID|1||MRN1\r";
        for (msg, needle) in [
            (no_pid.to_string(), "segment 'PID' is missing"),
            (empty_pid3, "PID-3 is required"),
            (short_pid.to_string(), "PID-3 is required"),
            (no_msh9.to_string(), "MSH-9 is required"),
        ] {
            let err = HL7V2_PROFILE_OPS.validate(&c, msg.as_bytes()).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Violation(m) if m.contains(needle)),
                "{needle}: {err:?}"
            );
        }
    }

    #[test]
    fn required_field_must_hold_in_every_segment_occurrence() {
        let c = compile(r#"{"required_fields":["OBX-5"]}"#);
        let ok = "MSH|^~\\&|A\rOBX|1|ST|A||x\rOBX|2|ST|B||y\r";
        let bad = "MSH|^~\\&|A\rOBX|1|ST|A||x\rOBX|2|ST|B||\r";
        assert!(HL7V2_PROFILE_OPS.validate(&c, ok.as_bytes()).is_ok());
        let err = HL7V2_PROFILE_OPS.validate(&c, bad.as_bytes()).unwrap_err();
        assert!(matches!(&err, SchemaError::Violation(m) if m.contains("occurrence 2")));
    }

    #[test]
    fn violation_messages_never_echo_field_values() {
        let c = compile(PROFILE);
        let msg = ADT.replace("MRN123", " ").replace("Doe^Jan", "SECRETNAME");
        let err = HL7V2_PROFILE_OPS.validate(&c, msg.as_bytes()).unwrap_err();
        assert!(!err.to_string().contains("SECRETNAME"));
    }

    #[test]
    fn derive_drops_required_fields_the_policy_hides_and_is_stable() {
        let derived = HL7V2_PROFILE_OPS
            .derive_subschema(PROFILE, &set(&["MSH-9", "PID-5"]))
            .unwrap();
        let v: Value = serde_json::from_str(&derived).unwrap();
        assert_eq!(v["required_fields"], serde_json::json!(["MSH-9"]));
        // The segment stays: a projection blanks fields, it never removes segments.
        assert_eq!(v["required_segments"], serde_json::json!(["MSH", "PID"]));
        assert_eq!(v["description"], "ADT admit");
        // Compiles again, and deriving the derived profile changes nothing.
        assert!(HL7V2_PROFILE_OPS.compile(&derived).is_ok());
        let twice = HL7V2_PROFILE_OPS
            .derive_subschema(&derived, &set(&["MSH-9", "PID-5"]))
            .unwrap();
        assert_eq!(derived, twice);
        // Order of the source lists must not matter.
        let reordered = r#"{"required_fields":["MSH-9","PID-3"],"description":"ADT admit","required_segments":["PID"]}"#;
        assert_eq!(
            derived,
            HL7V2_PROFILE_OPS
                .derive_subschema(reordered, &set(&["MSH-9", "PID-5"]))
                .unwrap()
        );
    }

    #[test]
    fn projected_message_satisfies_the_derived_profile() {
        let allowed = set(&["MSH-9", "PID-5"]);
        let derived = HL7V2_PROFILE_OPS
            .derive_subschema(PROFILE, &allowed)
            .unwrap();
        let projected = PayloadFormat::Hl7V2
            .codec()
            .project(ADT.as_bytes(), &allowed)
            .unwrap();
        // The original profile rejects the projection (PID-3 is blanked) ...
        assert!(HL7V2_PROFILE_OPS
            .validate(&compile(PROFILE), &projected)
            .is_err());
        // ... the derived one describes it exactly.
        assert!(HL7V2_PROFILE_OPS
            .validate(&compile(&derived), &projected)
            .is_ok());
    }

    #[test]
    fn compatibility_matrix() {
        let base = r#"{"required_segments":["PID"],"required_fields":["PID-3"]}"#;
        let looser = r#"{"required_segments":["PID"]}"#;
        let stricter = r#"{"required_segments":["PID","OBR"],"required_fields":["PID-3"]}"#;
        let reordered = r#"{"required_fields":["PID-3"],"required_segments":["PID"]}"#;
        let unrelated = r#"{"required_segments":["OBR"]}"#;
        use Compatibility::{Backward, Forward, Full};
        // (old, new, backward, forward, full)
        let cases = [
            (base, base, true, true, true),
            (base, reordered, true, true, true),
            // Dropping a requirement only widens what is valid.
            (base, looser, true, false, false),
            // Adding one narrows it.
            (base, stricter, false, true, false),
            (looser, base, false, true, false),
            (base, unrelated, false, false, false),
        ];
        for (old, new, backward, forward, full) in cases {
            for (mode, expected) in [(Backward, backward), (Forward, forward), (Full, full)] {
                let got = HL7V2_PROFILE_OPS.check_compatibility(old, new, mode);
                assert_eq!(
                    got.is_ok(),
                    expected,
                    "{old} -> {new} under {mode:?}: {got:?}"
                );
                if let Err(e) = got {
                    assert!(matches!(e, SchemaError::Incompatible(_)));
                }
            }
            assert!(HL7V2_PROFILE_OPS
                .check_compatibility(old, new, Compatibility::None)
                .is_ok());
        }
    }

    #[test]
    fn a_required_field_implies_its_segment_in_compatibility() {
        // Both profiles demand PID and PID-3 once the implication is applied.
        let a = r#"{"required_fields":["PID-3"]}"#;
        let b = r#"{"required_segments":["PID"],"required_fields":["PID-3"]}"#;
        assert!(HL7V2_PROFILE_OPS
            .check_compatibility(a, b, Compatibility::Full)
            .is_ok());
    }
}
