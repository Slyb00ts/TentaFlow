// =============================================================================
// File: bus/payload_format/json.rs — JSON payload field format (reference impl)
// =============================================================================
// Reference implementation — the format `5a7cd456c` shipped, now sitting
// behind the `PayloadFieldFormat` trait with unchanged behavior.
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;

use super::{FormatError, PayloadFieldFormat};

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

pub(super) static JSON_FORMAT: JsonFormat = JsonFormat;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_codec_round_trips_allowed_fields() {
        let codec = &JSON_FORMAT;
        let allowed: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let out = codec
            .project(br#"{"a":1,"b":2}"#, &allowed)
            .expect("project");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    #[test]
    fn json_codec_empty_projection_is_empty_object() {
        assert_eq!(JSON_FORMAT.empty_projection().as_ref(), b"{}");
    }
}
