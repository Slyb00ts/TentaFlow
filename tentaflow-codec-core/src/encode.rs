// ===== File: tentaflow-codec-core/src/encode.rs — JSON request → CBOR body bytes, driven by the wire schema =====
//
// The browser builds a request as JSON (snake_case wire names) and needs the CBOR a
// server decodes into its typed `MessageBody`. Going through a typed `Deserialize`
// of the protocol would link every variant's visitor into the wasm, so the schema
// does the typing instead: scalars are range-checked, integers may feed float
// fields, required fields must be present, byte vectors become CBOR byte strings.
// The tests prove that, for every variant, whatever the typed path accepts encodes
// to the very same typed value here.

use ciborium::value::{Integer, Value};
use serde_json::Value as Json;

use crate::schema::{Def, Field, Shape, Ty, DEFS, NULL_WHEN_ABSENT, KEEP_NULL, REQUIRED, ROOT};

#[derive(Debug, PartialEq, Eq)]
pub struct EncodeError {
    /// Innermost segment first; read back to front.
    trail: Vec<String>,
    message: String,
}

impl EncodeError {
    fn new(message: impl Into<String>) -> Self {
        Self { trail: Vec::new(), message: message.into() }
    }

    fn at(mut self, segment: impl Into<String>) -> Self {
        self.trail.push(segment.into());
        self
    }

}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, segment) in self.trail.iter().rev().enumerate() {
            f.write_str(if index == 0 { "" } else { "." })?;
            f.write_str(segment)?;
        }
        if !self.trail.is_empty() {
            f.write_str(": ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for EncodeError {}

fn mismatch(expected: &str, found: &Json) -> EncodeError {
    EncodeError::new(format!("expected {expected}, found {}", json_kind(found)))
}

fn json_kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

/// Encodes `variant` of the payload family reached through `path` (the tags of the
/// routing wrappers, outermost first; empty for a variant of `MessageBody` itself)
/// with `content` as its fields, and returns the CBOR of the whole body.
pub fn encode_body(path: &[&str], variant: &str, content: &Json) -> Result<Vec<u8>, EncodeError> {
    encode_body_with(path, variant, content, &[])
}

/// Like `encode_body`, with binary arguments the content refers to as `{"$bytes": n}`.
pub fn encode_body_with(path: &[&str], variant: &str, content: &Json, blobs: &[&[u8]]) -> Result<Vec<u8>, EncodeError> {
    let mut enum_id = ROOT as usize;
    for tag in path {
        enum_id = match variant_shape(enum_id, tag)? {
            Shape::Newtype(Ty::Named(next)) if matches!(DEFS[*next as usize], Def::Enum { routing: true, .. }) => *next as usize,
            _ => return Err(EncodeError::new(format!("`{tag}` does not wrap a payload family"))),
        };
    }
    let Def::Enum { tag: None, variants, deny_unknown, .. } = &DEFS[enum_id] else {
        return Err(EncodeError::new("the target is not an externally tagged enum"));
    };
    let shape = find_variant(variants, variant)?;
    let mut value = external_variant(variant, shape, *deny_unknown, content, blobs).map_err(|e| e.at(variant))?;
    for tag in path.iter().rev() {
        value = Value::Map(vec![(Value::Text((*tag).to_string()), value)]);
    }
    let mut out = Vec::new();
    ciborium::ser::into_writer(&value, &mut out).map_err(|e| EncodeError::new(format!("CBOR write failed: {e}")))?;
    Ok(out)
}

fn variant_shape(enum_id: usize, tag: &str) -> Result<&'static Shape, EncodeError> {
    match &DEFS[enum_id] {
        Def::Enum { variants, .. } => find_variant(variants, tag),
        _ => Err(EncodeError::new(format!("`{tag}` is not inside an enum"))),
    }
}

fn find_variant(variants: &'static [(&'static str, Shape)], name: &str) -> Result<&'static Shape, EncodeError> {
    variants
        .iter()
        .find(|(variant, _)| *variant == name)
        .map(|(_, shape)| shape)
        .ok_or_else(|| EncodeError::new(format!("unknown variant `{name}`")))
}

/// An externally tagged variant: a bare tag, or a one-entry map `{tag: content}`.
fn external_variant(name: &str, shape: &Shape, deny_unknown: bool, content: &Json, blobs: &[&[u8]]) -> Result<Value, EncodeError> {
    let inner = match shape {
        Shape::Unit => {
            return match content {
                Json::Null => Ok(Value::Text(name.to_string())),
                Json::Object(entries) if entries.is_empty() => Ok(Value::Text(name.to_string())),
                other => Err(mismatch("no content for a unit variant", other)),
            };
        }
        Shape::Newtype(ty) => to_cbor(ty, content, blobs)?,
        Shape::Struct(fields) => struct_value(fields, deny_unknown, content, None, blobs)?,
    };
    Ok(Value::Map(vec![(Value::Text(name.to_string()), inner)]))
}

fn to_cbor(ty: &Ty, json: &Json, blobs: &[&[u8]]) -> Result<Value, EncodeError> {
    match ty {
        Ty::Str => json.as_str().map(|text| Value::Text(text.to_string())).ok_or_else(|| mismatch("a string", json)),
        Ty::Bool => json.as_bool().map(Value::Bool).ok_or_else(|| mismatch("a boolean", json)),
        Ty::Unit => match json {
            Json::Null => Ok(Value::Null),
            Json::Object(entries) if entries.is_empty() => Ok(Value::Null),
            other => Err(mismatch("null", other)),
        },
        Ty::Uint(bits) => {
            let number = json.as_u64().ok_or_else(|| mismatch("an unsigned integer", json))?;
            if *bits < 64 && number >> bits != 0 {
                return Err(EncodeError::new(format!("{number} does not fit u{bits}")));
            }
            Ok(Value::Integer(number.into()))
        }
        Ty::Sint(bits) => {
            let number = json.as_i64().ok_or_else(|| mismatch("an integer", json))?;
            if *bits < 64 && !(-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(&number) {
                return Err(EncodeError::new(format!("{number} does not fit i{bits}")));
            }
            Ok(Value::Integer(Integer::from(number)))
        }
        Ty::Float(bits) => {
            let number = json.as_f64().ok_or_else(|| mismatch("a number", json))?;
            Ok(Value::Float(if *bits == 32 { f64::from(number as f32) } else { number }))
        }
        Ty::Json => Ok(json_value(json)),
        Ty::Bytes => match json {
            _ if blob_ref(json).is_some() => blob(json, blobs).map(|bytes| Value::Bytes(bytes.to_vec())),
            Json::Array(items) => items
                .iter()
                .map(|item| item.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or_else(|| mismatch("a byte", item)))
                .collect::<Result<Vec<u8>, _>>()
                .map(Value::Bytes),
            other => Err(mismatch("an array of bytes", other)),
        },
        Ty::Opt(inner) => match json {
            Json::Null => Ok(Value::Null),
            other => to_cbor(inner, other, blobs),
        },
        Ty::Seq(inner) => match json {
            _ if matches!(**inner, Ty::Uint(8)) && blob_ref(json).is_some() => {
                blob(json, blobs).map(|bytes| Value::Array(bytes.iter().map(|b| Value::Integer((*b).into())).collect()))
            }
            Json::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| to_cbor(inner, item, blobs).map_err(|e| e.at(index.to_string())))
                .collect::<Result<_, _>>()
                .map(Value::Array),
            other => Err(mismatch("an array", other)),
        },
        Ty::Tuple(items) => match json {
            Json::Array(values) if values.len() == items.len() => items
                .iter()
                .zip(values)
                .enumerate()
                .map(|(index, (ty, value))| to_cbor(ty, value, blobs).map_err(|e| e.at(index.to_string())))
                .collect::<Result<_, _>>()
                .map(Value::Array),
            Json::Array(values) => Err(EncodeError::new(format!("expected {} elements, found {}", items.len(), values.len()))),
            other => Err(mismatch("an array", other)),
        },
        Ty::Map(inner) => match json {
            Json::Object(entries) => entries
                .iter()
                .map(|(key, value)| Ok((Value::Text(key.clone()), to_cbor(inner, value, blobs).map_err(|e| e.at(key.as_str()))?)))
                .collect::<Result<_, _>>()
                .map(Value::Map),
            other => Err(mismatch("an object", other)),
        },
        Ty::Named(id) => named_value(&DEFS[*id as usize], json, blobs),
    }
}

fn named_value(def: &Def, json: &Json, blobs: &[&[u8]]) -> Result<Value, EncodeError> {
    match def {
        Def::Struct { fields, deny_unknown } => struct_value(fields, *deny_unknown, json, None, blobs),
        Def::Enum { tag: None, variants, deny_unknown, .. } => match json {
            Json::String(name) => external_variant(name, find_variant(variants, name)?, *deny_unknown, &Json::Null, blobs),
            Json::Object(entries) if entries.len() == 1 => {
                let (name, content) = entries.iter().next().expect("one entry");
                external_variant(name, find_variant(variants, name)?, *deny_unknown, content, blobs).map_err(|e| e.at(name.as_str()))
            }
            other => Err(mismatch("a variant name or a one-entry object", other)),
        },
        Def::Enum { tag: Some(tag), content: None, variants, deny_unknown, .. } => {
            let (name, entries) = tagged(tag, json)?;
            let shape = find_variant(variants, name)?;
            let head = Some((*tag, name));
            match shape {
                Shape::Unit => Ok(Value::Map(vec![(Value::Text((*tag).to_string()), Value::Text(name.to_string()))])),
                Shape::Struct(fields) => struct_value(fields, *deny_unknown, &Json::Object(entries.clone()), head, blobs),
                Shape::Newtype(Ty::Named(id)) => match &DEFS[*id as usize] {
                    Def::Struct { fields, deny_unknown } => struct_value(fields, *deny_unknown, &Json::Object(entries.clone()), head, blobs),
                    _ => Err(EncodeError::new(format!("variant `{name}` wraps an enum inside an internal tag"))),
                },
                Shape::Newtype(_) => Err(EncodeError::new(format!("variant `{name}` wraps a non-struct inside an internal tag"))),
            }
        }
        Def::Enum { tag: Some(tag), content: Some(content_key), variants, deny_unknown, .. } => {
            let (name, entries) = tagged(tag, json)?;
            let shape = find_variant(variants, name)?;
            let mut out = vec![(Value::Text((*tag).to_string()), Value::Text(name.to_string()))];
            let content = entries.get(*content_key).unwrap_or(&Json::Null);
            match shape {
                Shape::Unit => {}
                Shape::Newtype(ty) => out.push((Value::Text((*content_key).to_string()), to_cbor(ty, content, blobs).map_err(|e| e.at(*content_key))?)),
                Shape::Struct(fields) => {
                    out.push((Value::Text((*content_key).to_string()), struct_value(fields, *deny_unknown, content, None, blobs).map_err(|e| e.at(*content_key))?))
                }
            }
            Ok(Value::Map(out))
        }
    }
}

/// The variant name an internally or adjacently tagged object carries, with its entries.
fn tagged<'a>(tag: &str, json: &'a Json) -> Result<(&'a str, &'a serde_json::Map<String, Json>), EncodeError> {
    let Json::Object(entries) = json else { return Err(mismatch("an object", json)) };
    match entries.get(tag) {
        Some(Json::String(name)) => Ok((name.as_str(), entries)),
        Some(other) => Err(mismatch("a string", other).at(tag)),
        None => Err(EncodeError::new(format!("missing tag `{tag}`"))),
    }
}

/// A struct as a CBOR map in schema order. `head` is the tag entry of an
/// internally tagged enum, written first and never counted as an unknown key.
fn struct_value(fields: &[Field], deny_unknown: bool, json: &Json, head: Option<(&str, &str)>, blobs: &[&[u8]]) -> Result<Value, EncodeError> {
    let Json::Object(entries) = json else { return Err(mismatch("an object", json)) };
    let mut out = Vec::with_capacity(fields.len() + 1);
    if let Some((tag, name)) = head {
        out.push((Value::Text(tag.to_string()), Value::Text(name.to_string())));
    }
    for field in fields {
        match entries.get(field.name) {
            // `skip_serializing_if = "Option::is_none"` drops a null on the typed path too.
            Some(Json::Null) if field.flags & (NULL_WHEN_ABSENT | KEEP_NULL) == 0 && matches!(field.ty, Ty::Opt(_)) => {}
            Some(value) => out.push((Value::Text(field.name.to_string()), to_cbor(&field.ty, value, blobs).map_err(|e| e.at(field.name))?)),
            None if field.flags & REQUIRED != 0 => return Err(EncodeError::new(format!("missing field `{}`", field.name))),
            None if field.flags & NULL_WHEN_ABSENT != 0 => out.push((Value::Text(field.name.to_string()), Value::Null)),
            None => {}
        }
    }
    if deny_unknown {
        if let Some(extra) = entries.keys().find(|key| !fields.iter().any(|f| f.name == key.as_str()) && head.map(|(tag, _)| tag) != Some(key.as_str())) {
            return Err(EncodeError::new(format!("unknown field `{extra}`")));
        }
    }
    Ok(Value::Map(out))
}

/// `{"$bytes": n}` stands for the n-th binary argument, so a file crosses as a Uint8Array
/// instead of a JSON array of numbers, wherever in the request the byte field sits.
fn blob_ref(json: &Json) -> Option<&Json> {
    match json {
        Json::Object(entries) if entries.len() == 1 => entries.get("$bytes"),
        _ => None,
    }
}

fn blob<'a>(json: &Json, blobs: &[&'a [u8]]) -> Result<&'a [u8], EncodeError> {
    let index = blob_ref(json).and_then(Json::as_u64).ok_or_else(|| mismatch("a binary argument index", json))?;
    usize::try_from(index)
        .ok()
        .and_then(|index| blobs.get(index).copied())
        .ok_or_else(|| EncodeError::new(format!("binary argument {index} was not supplied")))
}

/// User data: shape and keys exactly as sent.
fn json_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => match (number.as_u64(), number.as_i64()) {
            (Some(unsigned), _) => Value::Integer(unsigned.into()),
            (None, Some(signed)) => Value::Integer(signed.into()),
            _ => Value::Float(number.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(text) => Value::Text(text.clone()),
        Json::Array(items) => Value::Array(items.iter().map(json_value).collect()),
        Json::Object(entries) => Value::Map(entries.iter().map(|(key, value)| (Value::Text(key.clone()), json_value(value))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tentaflow_protocol::MessageBody;

    /// Rotates through the variants of every enum so that all of them get visited.
    struct Sampler {
        seed: usize,
        counter: usize,
        minimal: bool,
    }

    const DEPTH_LIMIT: usize = 7;

    impl Sampler {
        fn ty(&mut self, ty: &Ty, depth: usize) -> Option<Json> {
            Some(match ty {
                Ty::Str => json!("text"),
                Ty::Bool => json!(true),
                Ty::Unit => Json::Null,
                Ty::Uint(_) => json!(7),
                Ty::Sint(_) => json!(-3),
                Ty::Float(_) if self.seed % 2 == 0 => json!(0.5),
                Ty::Float(_) => json!(2),
                Ty::Json => json!({"user_key": [1, "x", null, 2.5, true]}),
                Ty::Bytes => json!([1, 2, 255]),
                Ty::Opt(inner) => {
                    if depth > DEPTH_LIMIT || self.seed % 3 == 0 {
                        Json::Null
                    } else {
                        self.ty(inner, depth + 1)?
                    }
                }
                Ty::Seq(inner) => {
                    if depth > DEPTH_LIMIT {
                        json!([])
                    } else {
                        Json::Array(vec![self.ty(inner, depth + 1)?])
                    }
                }
                Ty::Tuple(items) => Json::Array(items.iter().map(|item| self.ty(item, depth + 1)).collect::<Option<_>>()?),
                Ty::Map(inner) => {
                    if depth > DEPTH_LIMIT {
                        json!({})
                    } else {
                        json!({ "entry_key": self.ty(inner, depth + 1)? })
                    }
                }
                Ty::Named(id) => self.named(&DEFS[*id as usize], depth + 1)?,
            })
        }

        fn fields(&mut self, fields: &[Field], depth: usize) -> Option<serde_json::Map<String, Json>> {
            let mut out = serde_json::Map::new();
            for field in fields {
                if self.minimal && field.flags & REQUIRED == 0 {
                    continue;
                }
                match self.ty(&field.ty, depth) {
                    Some(value) => {
                        out.insert(field.name.to_string(), value);
                    }
                    None if field.flags & REQUIRED == 0 => {}
                    None => return None,
                }
            }
            Some(out)
        }

        fn shape(&mut self, name: &str, shape: &Shape, tag: Option<&str>, depth: usize) -> Option<Json> {
            self.counter += 1;
            Some(match (shape, tag) {
                (Shape::Unit, None) => Json::String(name.to_string()),
                (Shape::Unit, Some(tag)) => json!({ tag: name }),
                (Shape::Newtype(ty), None) => json!({ name: self.ty(ty, depth)? }),
                (Shape::Struct(fields), None) => json!({ name: Json::Object(self.fields(fields, depth)?) }),
                (Shape::Newtype(ty), Some(tag)) => {
                    let mut entries = match self.ty(ty, depth)? {
                        Json::Object(entries) => entries,
                        _ => return None,
                    };
                    entries.insert(tag.to_string(), json!(name));
                    Json::Object(entries)
                }
                (Shape::Struct(fields), Some(tag)) => {
                    let mut entries = self.fields(fields, depth)?;
                    entries.insert(tag.to_string(), json!(name));
                    Json::Object(entries)
                }
            })
        }

        fn named(&mut self, def: &Def, depth: usize) -> Option<Json> {
            match def {
                Def::Struct { fields, .. } => self.fields(fields, depth).map(Json::Object),
                Def::Enum { tag, content, variants, .. } => {
                    let start = (self.seed + self.counter) % variants.len();
                    for offset in 0..variants.len() {
                        let (name, shape) = &variants[(start + offset) % variants.len()];
                        match (tag, content) {
                            (Some(tag), Some(content_key)) => {
                                let body = match shape {
                                    Shape::Unit => None,
                                    Shape::Newtype(ty) => self.ty(ty, depth),
                                    Shape::Struct(fields) => self.fields(fields, depth).map(Json::Object),
                                };
                                if matches!(shape, Shape::Unit) {
                                    return Some(json!({ *tag: name }));
                                }
                                if let Some(body) = body {
                                    return Some(json!({ *tag: name, *content_key: body }));
                                }
                            }
                            _ => {
                                if let Some(value) = self.shape(name, shape, *tag, depth) {
                                    return Some(value);
                                }
                            }
                        }
                    }
                    None
                }
            }
        }
    }

    /// The content of one body variant as the browser would send it.
    fn content(shape: &Shape, sampler: &mut Sampler) -> Option<Json> {
        match shape {
            Shape::Unit => Some(Json::Null),
            Shape::Newtype(ty) => sampler.ty(ty, 0),
            Shape::Struct(fields) => sampler.fields(fields, 0).map(Json::Object),
        }
    }

    fn wrap(path: &[&str], variant: &str, shape: &Shape, content: &Json) -> Json {
        let mut value = match shape {
            Shape::Unit => json!(variant),
            _ => json!({ variant: content }),
        };
        for tag in path.iter().rev() {
            value = json!({ *tag: value });
        }
        value
    }

    fn leaves(enum_id: usize, path: &mut Vec<&'static str>, out: &mut Vec<(Vec<&'static str>, &'static str, &'static Shape)>) {
        let Def::Enum { variants, .. } = &DEFS[enum_id] else { return };
        for (name, shape) in variants.iter() {
            match shape {
                Shape::Newtype(Ty::Named(next)) if matches!(DEFS[*next as usize], Def::Enum { routing: true, .. }) => {
                    path.push(name);
                    leaves(*next as usize, path, out);
                    path.pop();
                }
                _ => out.push((path.clone(), name, shape)),
            }
        }
    }

    /// For every body variant and several fillings of it: whatever the typed
    /// `Deserialize` accepts must encode to the same typed value.
    #[test]
    fn schema_encoding_equals_the_typed_path_for_every_variant() {
        let mut all = Vec::new();
        leaves(ROOT as usize, &mut Vec::new(), &mut all);
        assert!(all.len() > 300, "only {} variants found", all.len());
        let mut failures = Vec::new();
        let (mut compared, mut typed_rejected) = (0usize, 0usize);
        for (path, variant, shape) in &all {
            for minimal in [false, true] {
                for seed in 0..40 {
                    let mut sampler = Sampler { seed, counter: 0, minimal };
                    let Some(body) = content(shape, &mut sampler) else { continue };
                    let Ok(typed) = serde_json::from_value::<MessageBody>(wrap(path, variant, shape, &body)) else {
                        typed_rejected += 1;
                        continue;
                    };
                    compared += 1;
                    let label = format!("{}.{variant} (seed {seed}, minimal {minimal})", path.join("."));
                    match encode_body(path, variant, &body) {
                        Err(error) => failures.push(format!("{label}: encoder refused: {error}\n    {body}")),
                        Ok(bytes) => match tentaflow_protocol::cbor::decode::<MessageBody>(&bytes) {
                            Err(error) => failures.push(format!("{label}: typed decode of our bytes failed: {error}\n    {body}")),
                            Ok(ours) if ours != typed => failures.push(format!("{label}: values differ\n    {body}")),
                            Ok(_) => {}
                        },
                    }
                }
            }
        }
        assert!(compared > 10_000, "only {compared} comparisons ran ({typed_rejected} rejected by the typed path)");
        assert!(failures.is_empty(), "{} of {compared} comparisons failed, first ones:\n{}", failures.len(), failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
    }

    #[test]
    fn a_missing_required_field_names_itself() {
        let error = encode_body(&["ServiceBody"], "ReqOauthStart", &json!({})).unwrap_err();
        assert_eq!(error.to_string(), "ReqOauthStart: missing field `provider`");
    }

    #[test]
    fn scalars_are_checked_against_their_rust_type() {
        let error = encode_body(&[], "MetaHeartbeat", &json!({"sent_at_epoch": "soon"})).unwrap_err();
        assert_eq!(error.to_string(), "MetaHeartbeat.sent_at_epoch: expected an unsigned integer, found a string");
        let error = encode_body(&[], "MetaHeartbeat", &json!({"sent_at_epoch": -1})).unwrap_err();
        assert!(error.to_string().contains("expected an unsigned integer"), "{error}");
    }

    #[test]
    fn unknown_variants_and_paths_are_refused() {
        assert!(encode_body(&[], "NoSuchBody", &Json::Null).is_err());
        assert!(encode_body(&["NoSuchBody"], "X", &Json::Null).is_err());
        assert!(encode_body(&["MetaHeartbeat"], "X", &Json::Null).is_err());
    }

    #[test]
    fn a_binary_argument_stands_in_for_a_byte_array() {
        let inline = encode_body(&["VncTunnelBody"], "ReqSend", &json!({"tunnel_id": "t", "bytes": [1, 2, 3]})).unwrap();
        let referenced = encode_body_with(&["VncTunnelBody"], "ReqSend", &json!({"tunnel_id": "t", "bytes": {"$bytes": 0}}), &[&[1, 2, 3]]).unwrap();
        assert_eq!(inline, referenced);
        let error = encode_body_with(&["VncTunnelBody"], "ReqSend", &json!({"tunnel_id": "t", "bytes": {"$bytes": 1}}), &[&[1]]).unwrap_err();
        assert!(error.to_string().contains("binary argument 1 was not supplied"), "{error}");
    }

    #[test]
    fn a_unit_variant_is_a_bare_tag() {
        let bytes = encode_body(&[], "ModelListRequest", &Json::Null).unwrap();
        assert_eq!(tentaflow_protocol::cbor::decode::<MessageBody>(&bytes).unwrap(), MessageBody::ModelListRequest);
    }
}
