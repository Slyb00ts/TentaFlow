// ===== File: tentaflow-protocol-wasm/src/body.rs — generic CBOR body → JS object, no typed Deserialize =====
//
// The shape contract (tag path in `_variant`, camelCase struct fields, untouched
// user-data keys, Number/BigInt policy) lives in `tentaflow-codec-core`; this file
// only builds the JS values.

use std::borrow::Cow;

use ciborium::value::Value;
use tentaflow_codec_core::{decode_body, IntRepr, Sink};
use wasm_bindgen::prelude::*;

struct JsSink;

impl Sink for JsSink {
    type Out = JsValue;

    fn null(&mut self) -> JsValue {
        JsValue::NULL
    }

    fn bool(&mut self, value: bool) -> JsValue {
        JsValue::from_bool(value)
    }

    fn int(&mut self, value: IntRepr) -> JsValue {
        match value {
            IntRepr::Number(number) => JsValue::from_f64(number),
            IntRepr::BigInt(big) => JsValue::bigint_from_str(&big.to_string()),
        }
    }

    fn float(&mut self, value: f64) -> JsValue {
        JsValue::from_f64(value)
    }

    fn text(&mut self, value: &str) -> JsValue {
        JsValue::from_str(value)
    }

    fn bytes(&mut self, value: &[u8]) -> JsValue {
        js_sys::Uint8Array::from(value).into()
    }

    fn array(&mut self, items: Vec<JsValue>) -> JsValue {
        items.into_iter().collect::<js_sys::Array>().into()
    }

    /// `Object.fromEntries` defines own data properties, so a wire key such as
    /// `__proto__` cannot reach the prototype setter the way `Reflect.set` would.
    fn object(&mut self, entries: Vec<(Cow<'_, str>, JsValue)>) -> JsValue {
        let pairs: js_sys::Array = entries
            .into_iter()
            .map(|(key, value)| JsValue::from(js_sys::Array::of2(&JsValue::from_str(&key), &value)))
            .collect();
        js_sys::Object::from_entries(&pairs).map_or(JsValue::UNDEFINED, JsValue::from)
    }
}

/// Decodes a CBOR-encoded `MessageBody` into a plain JS object.
#[wasm_bindgen(js_name = decodeMessageBody)]
pub fn decode_message_body(bytes: &[u8]) -> Result<JsValue, JsError> {
    let value: Value = ciborium::de::from_reader(bytes)
        .map_err(|e| JsError::new(&format!("body decode failed: {e}")))?;
    decode_body(&value, &mut JsSink).map_err(|e| JsError::new(&format!("body shape: {e}")))
}
