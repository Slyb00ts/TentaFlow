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

pub(crate) mod hl7v2;
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

/// How a read rule alters a visible field's value instead of hiding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceAction {
    Mask,
    Hash,
}

/// Whether a value's text length carries meaning. A JSON boolean or a nested
/// object/array is `Opaque`: a length-preserving mask would tell `true` from
/// `false` (or size a hidden structure), so a mask of it is constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueShape {
    Text,
    Opaque,
}

/// Decides which visible fields a projection replaces and with what.
/// Implemented by `field_policies` (mask rule, keyed hash); a codec only asks.
pub trait FieldReplacer {
    /// The action for the field at `field`, `None` when it stays as it is.
    fn action(&self, field: &str) -> Option<ReplaceAction>;

    /// The replacement for one value, given as its text. An empty value is
    /// never passed: it carries nothing to hide and stays empty. The result
    /// is plain text; the codec makes it safe for its own syntax.
    fn replace(&self, action: ReplaceAction, text: &str, shape: ValueShape) -> String;
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
    /// fields in `visible` (read-path projection). A visible field the
    /// `replacer` names keeps its place and address but carries the
    /// replacement value instead of its own; the codec never learns how a
    /// replacement is computed (a hash key stays out of this layer).
    fn project(
        &self,
        payload: &[u8],
        visible: &BTreeSet<String>,
        replacer: Option<&dyn FieldReplacer>,
    ) -> Result<Bytes, FormatError>;

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
    /// Every format this build parses, in the order the UI lists them.
    pub const ALL: [PayloadFormat; 3] = [
        PayloadFormat::Json,
        PayloadFormat::Xml,
        PayloadFormat::Hl7V2,
    ];

    /// The `content_type` a topic of this format is created with — the
    /// first of the spellings `from_content_type` recognizes for it.
    pub fn canonical_content_type(self) -> &'static str {
        match self {
            PayloadFormat::Json => "application/json",
            PayloadFormat::Xml => "application/xml",
            PayloadFormat::Hl7V2 => "application/hl7-v2",
        }
    }

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

/// A replacer for the codec tests: names its masked and hashed fields and
/// turns a value into text with `output`, so a codec is tested without a key.
#[cfg(test)]
pub(crate) struct TestReplacer<'a> {
    pub masked: &'a [&'a str],
    pub hashed: &'a [&'a str],
    pub output: &'a dyn Fn(ReplaceAction, &str, ValueShape) -> String,
}

#[cfg(test)]
impl TestReplacer<'_> {
    /// `M[value]` for a mask, `H[value]` for a hash, `M[*]` for an opaque mask.
    pub(crate) fn tagging(
        masked: &'static [&'static str],
        hashed: &'static [&'static str],
    ) -> TestReplacer<'static> {
        TestReplacer {
            masked,
            hashed,
            output: &|action, text, shape| match (action, shape) {
                (ReplaceAction::Mask, ValueShape::Opaque) => "M[*]".to_string(),
                (ReplaceAction::Mask, ValueShape::Text) => format!("M[{text}]"),
                (ReplaceAction::Hash, _) => format!("H[{text}]"),
            },
        }
    }
}

#[cfg(test)]
impl FieldReplacer for TestReplacer<'_> {
    fn action(&self, field: &str) -> Option<ReplaceAction> {
        if self.hashed.contains(&field) {
            Some(ReplaceAction::Hash)
        } else if self.masked.contains(&field) {
            Some(ReplaceAction::Mask)
        } else {
            None
        }
    }

    fn replace(&self, action: ReplaceAction, text: &str, shape: ValueShape) -> String {
        (self.output)(action, text, shape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_content_type_resolves_back_to_its_format() {
        for format in PayloadFormat::ALL {
            assert_eq!(
                PayloadFormat::from_content_type(format.canonical_content_type()),
                format
            );
        }
    }

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
