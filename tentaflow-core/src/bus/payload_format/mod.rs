// =============================================================================
// File: bus/payload_format/mod.rs — pluggable wire-format field extraction
// =============================================================================
// SUM/tentabus/POLITYKI-POL-FORMATY.md: field-level access policies
// (`bus::field_policies`) validate/project payloads against a topic's
// declared wire format, not just JSON. One trait, one implementation per
// format (`json`/`xml`/`hl7v2` submodules) — deliberately NO shared
// intermediate representation: a read projection must re-emit the payload
// in its ORIGINAL wire format, losslessly for every allowed field, and an
// IR would lose whatever it doesn't model (XML attributes/namespaces, HL7
// repetition, an Avro writer's schema). Field addresses stay plain strings
// in the policy row regardless of format (a JSON key, HL7's "PID-5", a
// schema-resolved field name) — that is what lets one trait cover formats
// with completely different addressing models.
//
// `PayloadFormat::from_content_type` is where a topic's `content_type`
// resolves to a concrete codec, with a backward-compatibility rule: an
// unrecognized or default (`application/octet-stream`) content_type on a
// policy-bearing topic still resolves to `Json` — the exact behavior
// `5a7cd456c` shipped, before this format-routing layer existed. Only an
// EXPLICIT, recognized non-JSON content_type routes elsewhere.
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;

mod hl7v2;
mod json;
mod xml;

/// One format-specific parse/encode failure, carrying enough context for
/// `field_policies` to turn it into a `BusServiceError`. Deliberately not
/// `BusServiceError` itself — this trait lives below `bus::mod` in the
/// dependency graph and has no reason to know about the service's full
/// error surface.
#[derive(Debug, Clone)]
pub struct FormatError(pub String);

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Implemented once per wire format `bus_field_policies` can enforce
/// against. See this module's header for why there is no shared
/// intermediate representation between implementations.
pub trait PayloadFieldFormat: Send + Sync {
    /// Top-level field addresses present in `payload` (write-path
    /// validation: every address returned here that is not in the
    /// policy's allow-list fails the whole batch).
    fn list_fields(&self, payload: &[u8]) -> Result<BTreeSet<String>, FormatError>;

    /// Re-emits `payload` in the SAME wire format, containing only the
    /// fields in `allowed` (read-path projection).
    fn project(&self, payload: &[u8], allowed: &BTreeSet<String>) -> Result<Bytes, FormatError>;

    /// Fail-closed replacement payload for a read that cannot be projected
    /// at all (malformed payload, or a shape this format's `list_fields`/
    /// `project` cannot parse) — "hide only" (POLITYKI-POL.md) applied to
    /// the worst case: hide everything rather than guess.
    fn empty_projection(&self) -> Bytes;

    /// Syntax check for a field address as it would appear in a policy's
    /// `fields_json`/`required_fields_json` (administrative time, in
    /// `field_policies::set_policy`) — NOT whether the field exists in any
    /// particular payload.
    fn validate_field_name(&self, name: &str) -> Result<(), FormatError>;
}

/// Wire format a topic's `content_type` resolves to — the extension point
/// each phase of SUM/tentabus/POLITYKI-POL-FORMATY.md adds a variant to,
/// each backed by its own `PayloadFieldFormat` impl in this module's
/// submodules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    Json,
    /// F1. `application/xml` or `text/xml`.
    Xml,
    /// F2. `application/hl7-v2` or `x-application/hl7-v2+er7` (there is no
    /// single universal registered MIME type for ER7 HL7 v2 — these are
    /// the two conventions seen in practice; extend here if a real
    /// integration needs another string).
    Hl7V2,
}

impl PayloadFormat {
    /// Resolves a topic's `content_type` to a format. Backward-compat rule
    /// (see this module's header): unrecognized or
    /// `application/octet-stream` resolves to `Json`, matching pre-F0
    /// behavior exactly — only an explicit, recognized non-JSON
    /// content_type routes elsewhere.
    pub fn from_content_type(content_type: &str) -> PayloadFormat {
        match content_type {
            "application/xml" | "text/xml" => PayloadFormat::Xml,
            "application/hl7-v2" | "x-application/hl7-v2+er7" => PayloadFormat::Hl7V2,
            _ => PayloadFormat::Json,
        }
    }

    /// Human-readable label for error messages
    /// (`BusServiceError::FieldPolicyPayloadMalformed`'s `format` field).
    pub fn as_str(self) -> &'static str {
        match self {
            PayloadFormat::Json => "json",
            PayloadFormat::Xml => "xml",
            PayloadFormat::Hl7V2 => "hl7v2",
        }
    }

    pub fn codec(self) -> &'static dyn PayloadFieldFormat {
        match self {
            PayloadFormat::Json => &json::JSON_FORMAT,
            PayloadFormat::Xml => &xml::XML_FORMAT,
            PayloadFormat::Hl7V2 => &hl7v2::HL7V2_FORMAT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_content_type_recognizes_json() {
        assert_eq!(
            PayloadFormat::from_content_type("application/json"),
            PayloadFormat::Json
        );
    }

    #[test]
    fn from_content_type_recognizes_xml() {
        assert_eq!(
            PayloadFormat::from_content_type("application/xml"),
            PayloadFormat::Xml
        );
        assert_eq!(
            PayloadFormat::from_content_type("text/xml"),
            PayloadFormat::Xml
        );
    }

    #[test]
    fn from_content_type_recognizes_hl7v2() {
        assert_eq!(
            PayloadFormat::from_content_type("application/hl7-v2"),
            PayloadFormat::Hl7V2
        );
        assert_eq!(
            PayloadFormat::from_content_type("x-application/hl7-v2+er7"),
            PayloadFormat::Hl7V2
        );
    }

    #[test]
    fn from_content_type_defaults_unrecognized_to_json() {
        assert_eq!(
            PayloadFormat::from_content_type("application/octet-stream"),
            PayloadFormat::Json
        );
        assert_eq!(PayloadFormat::from_content_type(""), PayloadFormat::Json);
        assert_eq!(
            PayloadFormat::from_content_type("bogus/nonsense"),
            PayloadFormat::Json
        );
    }

    #[test]
    fn every_format_round_trips_its_own_empty_projection_through_list_fields() {
        // A codec's fail-closed `empty_projection()` must itself always be
        // parseable by that SAME codec's `list_fields` — otherwise a
        // second policy-bearing read of an already-degraded record would
        // degrade again instead of being stable.
        for fmt in [
            PayloadFormat::Json,
            PayloadFormat::Xml,
            PayloadFormat::Hl7V2,
        ] {
            let codec = fmt.codec();
            let empty = codec.empty_projection();
            assert!(
                codec.list_fields(&empty).is_ok(),
                "{:?}'s empty_projection() must round-trip through its own list_fields",
                fmt
            );
        }
    }
}
