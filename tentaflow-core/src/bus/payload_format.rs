// =============================================================================
// File: bus/payload_format.rs — pluggable wire-format field extraction
// =============================================================================
// SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): field-level access policies
// (`bus::field_policies`) validate/project payloads against a topic's
// declared wire format, not just JSON. One trait, one implementation per
// format — deliberately NO shared intermediate representation: a read
// projection must re-emit the payload in its ORIGINAL wire format,
// losslessly for every allowed field, and an IR would lose whatever it
// doesn't model (XML attributes/namespaces, HL7 repetition, an Avro
// writer's schema). Field addresses stay plain strings in the policy row
// regardless of format (a JSON key, HL7's "PID-5", a schema-resolved field
// name) — that is what lets one trait cover formats with completely
// different addressing models.
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

/// Reference implementation — the format `5a7cd456c` shipped, now sitting
/// behind the trait with unchanged behavior.
pub struct JsonFormat;

impl PayloadFieldFormat for JsonFormat {
    fn list_fields(&self, payload: &[u8]) -> Result<BTreeSet<String>, FormatError> {
        let value: serde_json::Value =
            serde_json::from_slice(payload).map_err(|e| FormatError(e.to_string()))?;
        match value {
            serde_json::Value::Object(obj) => Ok(obj.keys().cloned().collect()),
            _ => Err(FormatError("payload is not a JSON object".to_string())),
        }
    }

    fn project(&self, payload: &[u8], allowed: &BTreeSet<String>) -> Result<Bytes, FormatError> {
        let value: serde_json::Value =
            serde_json::from_slice(payload).map_err(|e| FormatError(e.to_string()))?;
        let serde_json::Value::Object(obj) = value else {
            return Err(FormatError("payload is not a JSON object".to_string()));
        };
        let filtered: serde_json::Map<String, serde_json::Value> = obj
            .into_iter()
            .filter(|(k, _)| allowed.contains(k))
            .collect();
        serde_json::to_vec(&serde_json::Value::Object(filtered))
            .map(Bytes::from)
            .map_err(|e| FormatError(e.to_string()))
    }

    fn empty_projection(&self) -> Bytes {
        Bytes::from_static(b"{}")
    }

    fn validate_field_name(&self, _name: &str) -> Result<(), FormatError> {
        // Any string is a syntactically valid JSON object key.
        Ok(())
    }
}

static JSON_FORMAT: JsonFormat = JsonFormat;

/// Wire format a topic's `content_type` resolves to. Only `Json` is
/// implemented today (SUM/tentabus/POLITYKI-POL-FORMATY.md, phase F0) —
/// this enum is the extension point later phases (F1 XML, F2 HL7 v2, F4
/// Avro/Protobuf/Thrift) add a variant to, each backed by its own
/// `PayloadFieldFormat` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    Json,
}

impl PayloadFormat {
    /// Resolves a topic's `content_type` to a format. Backward-compat rule
    /// (see this module's header): unrecognized or
    /// `application/octet-stream` resolves to `Json`, matching pre-F0
    /// behavior exactly — only an explicit, recognized non-JSON
    /// content_type is meant to route elsewhere once later phases add
    /// more variants.
    pub fn from_content_type(content_type: &str) -> PayloadFormat {
        match content_type {
            "application/json" => PayloadFormat::Json,
            _ => PayloadFormat::Json,
        }
    }

    /// Human-readable label for error messages
    /// (`BusServiceError::FieldPolicyPayloadMalformed`'s `format` field).
    pub fn as_str(self) -> &'static str {
        match self {
            PayloadFormat::Json => "json",
        }
    }

    pub fn codec(self) -> &'static dyn PayloadFieldFormat {
        match self {
            PayloadFormat::Json => &JSON_FORMAT,
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
    fn json_codec_round_trips_allowed_fields() {
        let codec = PayloadFormat::Json.codec();
        let allowed: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let out = codec
            .project(br#"{"a":1,"b":2}"#, &allowed)
            .expect("project");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    #[test]
    fn json_codec_empty_projection_is_empty_object() {
        let codec = PayloadFormat::Json.codec();
        assert_eq!(codec.empty_projection().as_ref(), b"{}");
    }
}
