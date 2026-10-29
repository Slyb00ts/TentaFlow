// =============================================================================
// File: bus/payload_format/json.rs — JSON payload field format (reference impl)
// =============================================================================
// Reference implementation — the format `5a7cd456c` shipped, now sitting
// behind the `PayloadFieldFormat` trait with unchanged behavior.
//
// A replaced value (mask/hash, F4 B7) keeps its key and becomes a JSON string,
// whatever it was: a number is replaced from its text (`12345` -> "**345"),
// `null` stays `null` and an empty string stays empty (nothing to hide). A
// boolean or a nested object/array has no text length worth keeping, so a mask
// of it is constant and a hash is taken over its compact JSON text.
//
// Known limit: a number is replaced from the text `serde_json` gives back, not
// from the bytes of the document (the workspace builds it without
// `arbitrary_precision`). An integer is exact up to u64/i64; `1.50` reads as
// `1.5`, `1e2` as `100.0`, and an integer beyond u64 as a float. The hash of
// such a number therefore differs from the hash of the same digits sent as a
// string. Identifiers that must hash equal in every format belong in a string
// (or are 16–19 digit integers, which are exact).
// =============================================================================

use std::collections::BTreeSet;

use bytes::Bytes;

use super::{FieldReplacer, FormatError, PayloadFieldFormat, ReplaceAction, ValueShape};

fn replaced_value(
    replacer: &dyn FieldReplacer,
    action: ReplaceAction,
    value: serde_json::Value,
) -> Result<serde_json::Value, FormatError> {
    use serde_json::Value;
    let (text, shape) = match &value {
        Value::Null => return Ok(Value::Null),
        Value::String(s) if s.is_empty() => return Ok(value),
        Value::String(s) => (s.clone(), ValueShape::Text),
        Value::Number(n) => (n.to_string(), ValueShape::Text),
        Value::Bool(b) => (b.to_string(), ValueShape::Opaque),
        Value::Array(_) | Value::Object(_) => (
            serde_json::to_string(&value).map_err(|e| FormatError(e.to_string()))?,
            ValueShape::Opaque,
        ),
    };
    Ok(Value::String(replacer.replace(action, &text, shape)))
}

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

    fn project(
        &self,
        payload: &[u8],
        visible: &BTreeSet<String>,
        replacer: Option<&dyn FieldReplacer>,
    ) -> Result<Bytes, FormatError> {
        let value: serde_json::Value =
            serde_json::from_slice(payload).map_err(|e| FormatError(e.to_string()))?;
        let serde_json::Value::Object(obj) = value else {
            return Err(FormatError("payload is not a JSON object".to_string()));
        };
        let mut filtered = serde_json::Map::new();
        for (key, value) in obj {
            if !visible.contains(&key) {
                continue;
            }
            let value = match replacer.and_then(|r| r.action(&key).map(|a| (r, a))) {
                Some((replacer, action)) => replaced_value(replacer, action, value)?,
                None => value,
            };
            filtered.insert(key, value);
        }
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
    use super::super::TestReplacer;
    use super::*;

    #[test]
    fn json_codec_round_trips_allowed_fields() {
        let codec = &JSON_FORMAT;
        let allowed: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let out = codec
            .project(br#"{"a":1,"b":2}"#, &allowed, None)
            .expect("project");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    fn visible(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn project_with(
        payload: &str,
        shown: &[&str],
        masked: &'static [&'static str],
        hashed: &'static [&'static str],
    ) -> serde_json::Value {
        let replacer = TestReplacer::tagging(masked, hashed);
        let out = JSON_FORMAT
            .project(payload.as_bytes(), &visible(shown), Some(&replacer))
            .expect("project");
        serde_json::from_slice(&out).unwrap()
    }

    /// F4 B7 review R1 P3-8: documents what a number becomes. An integer keeps
    /// its digits; a float or a huge integer is replaced from its re-rendered
    /// text, so it does not match the same digits sent as a string.
    #[test]
    fn json_numbers_are_replaced_from_their_rendered_text() {
        let got = project_with(
            r#"{"a":1234567890123456789,"b":1.50,"c":1e2,"d":"1.50"}"#,
            &["a", "b", "c", "d"],
            &[],
            &["a", "b", "c", "d"],
        );
        assert_eq!(got["a"], "H[1234567890123456789]", "an integer keeps its digits");
        assert_eq!(got["b"], "H[1.5]", "1.50 is rendered 1.5");
        assert_eq!(got["c"], "H[100.0]", "1e2 is rendered 100.0");
        assert_eq!(got["d"], "H[1.50]", "a string is taken as it is");
    }

    #[test]
    fn json_four_kinds_of_field_in_one_payload() {
        let got = project_with(
            r#"{"id":"7","ssn":"123456789","card":"4111","name":"Jan"}"#,
            &["id", "card", "name"],
            &["card"],
            &["name"],
        );
        assert_eq!(
            got,
            serde_json::json!({"id": "7", "card": "M[4111]", "name": "H[Jan]"})
        );
    }

    #[test]
    fn json_replaced_values_become_strings_and_null_and_empty_stay() {
        let got = project_with(
            r#"{"n":12345,"f":1.5,"b":true,"nul":null,"e":"","o":{"a":1},"a":[1,2]}"#,
            &["n", "f", "b", "nul", "e", "o", "a"],
            &["n", "f", "b", "nul", "e", "o", "a"],
            &[],
        );
        assert_eq!(
            got,
            serde_json::json!({
                "n": "M[12345]", "f": "M[1.5]", "b": "M[*]", "nul": null, "e": "",
                "o": "M[*]", "a": "M[*]"
            })
        );
        let hashed = project_with(
            r#"{"n":12345,"b":false,"nul":null,"e":"","o":{"a":1}}"#,
            &["n", "b", "nul", "e", "o"],
            &[],
            &["n", "b", "nul", "e", "o"],
        );
        assert_eq!(
            hashed,
            serde_json::json!({
                "n": "H[12345]", "b": "H[false]", "nul": null, "e": "", "o": "H[{\"a\":1}]"
            })
        );
    }

    #[test]
    fn json_a_replacer_never_shows_a_field_the_rule_hides() {
        let got = project_with(r#"{"a":"x","b":"y"}"#, &["a"], &["b"], &["b"]);
        assert_eq!(got, serde_json::json!({"a": "x"}));
    }

    #[test]
    fn json_codec_empty_projection_is_empty_object() {
        assert_eq!(JSON_FORMAT.empty_projection().as_ref(), b"{}");
    }
}
