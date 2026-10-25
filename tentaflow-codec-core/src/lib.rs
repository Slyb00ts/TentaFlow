// ===== File: tentaflow-codec-core/src/lib.rs — wire-shape rules of the CBOR protocol, free of any JS/wasm type =====
//
// The browser codec, the native clients and a future Rust UI all see the same
// serde wire: externally tagged enums keyed by variant NAME, structs as maps keyed
// by field name. `decode_body` walks a decoded `ciborium::Value` against a static
// schema (generated from the protocol sources, see build.rs) and hands the result
// to a `Sink`, so no typed `Deserialize` of the protocol is linked — which is what
// kept the browser wasm at 32 MB.
//
// Shape of a decoded body:
//   - `_variant`: tag path from the wire, e.g. `ServiceBody.ReqOauthStart`;
//   - struct fields sit on the object, snake_case wire names turned into camelCase;
//   - keys of maps (user data) and of opaque JSON are never touched;
//   - non-struct content (array, scalar) sits under `value`;
//   - integers are Numbers up to 2^53-1, BigInt beyond; byte strings stay bytes.

use std::borrow::Cow;

use ciborium::value::{Integer, Value};

mod encode;
mod schema;

pub use encode::{encode_body, encode_body_with, EncodeError};
use schema::{Def, Field, Shape, Ty, DEFS, ROOT};

/// Key carrying the tag path. Underscore-prefixed because wire field names are
/// plain snake_case and some of them are literally `variant`.
pub const VARIANT_KEY: &str = "_variant";

/// Largest integer a JS Number represents exactly.
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShapeError {
    #[error("body is not a tagged variant (found {found})")]
    NotTagged { found: &'static str },
    #[error("variant tag is not text")]
    TagNotText,
    #[error("map key is neither text nor an integer")]
    BadMapKey,
}

/// A CBOR integer as JS wants it.
#[derive(Debug, PartialEq)]
pub enum IntRepr {
    Number(f64),
    BigInt(i128),
}

pub fn int_repr(integer: Integer) -> IntRepr {
    let value = i128::from(integer);
    if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
        IntRepr::Number(value as f64)
    } else {
        IntRepr::BigInt(value)
    }
}

/// Receives the converted tree. The browser implements it with JS values, tests
/// with a plain tree, a native UI could build its own model.
pub trait Sink {
    type Out;
    fn null(&mut self) -> Self::Out;
    fn bool(&mut self, value: bool) -> Self::Out;
    fn int(&mut self, value: IntRepr) -> Self::Out;
    fn float(&mut self, value: f64) -> Self::Out;
    fn text(&mut self, value: &str) -> Self::Out;
    fn bytes(&mut self, value: &[u8]) -> Self::Out;
    fn array(&mut self, items: Vec<Self::Out>) -> Self::Out;
    fn object(&mut self, entries: Vec<(Cow<'_, str>, Self::Out)>) -> Self::Out;
}

type Entries<'a, S> = Vec<(Cow<'a, str>, <S as Sink>::Out)>;

/// Converts a decoded `MessageBody` into the shape described in the file header.
pub fn decode_body<S: Sink>(body: &Value, sink: &mut S) -> Result<S::Out, ShapeError> {
    let mut path = String::new();
    let mut enum_id = ROOT as usize;
    let mut current = body;
    loop {
        let (tag, content) = match current {
            Value::Text(tag) => (tag.as_str(), None),
            Value::Map(entries) if entries.len() == 1 => match &entries[0].0 {
                Value::Text(tag) => (tag.as_str(), Some(&entries[0].1)),
                _ => return Err(ShapeError::TagNotText),
            },
            other => return Err(ShapeError::NotTagged { found: kind_name(other) }),
        };
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(tag);

        let shape = variant_shape(enum_id, tag);
        let Some(content) = content else { return Ok(tagged_object(&path, Vec::new(), sink)) };
        if let (Some(Shape::Newtype(Ty::Named(next))), true) = (shape, descends(content)) {
            if matches!(DEFS[*next as usize], Def::Enum { routing: true, .. }) {
                enum_id = *next as usize;
                current = content;
                continue;
            }
        }
        let mut entries: Entries<'_, S> = Vec::new();
        match shape {
            Some(Shape::Struct(fields)) => spread(content, &Ty::Json, Some(fields), sink, &mut entries)?,
            Some(Shape::Newtype(ty)) => spread(content, ty, None, sink, &mut entries)?,
            Some(Shape::Unit) | None => spread(content, &Ty::Json, None, sink, &mut entries)?,
        }
        return Ok(tagged_object(&path, entries, sink));
    }
}

fn tagged_object<S: Sink>(path: &str, mut entries: Entries<'_, S>, sink: &mut S) -> S::Out {
    let tag = sink.text(path);
    entries.insert(0, (Cow::Borrowed(VARIANT_KEY), tag));
    sink.object(entries)
}

fn variant_shape(enum_id: usize, tag: &str) -> Option<&'static Shape> {
    match &DEFS[enum_id] {
        Def::Enum { variants, .. } => variants.iter().find(|(name, _)| *name == tag).map(|(_, shape)| shape),
        _ => None,
    }
}

/// A wrapper's content is a tagged variant again: a bare tag or a one-entry text-keyed map.
fn descends(content: &Value) -> bool {
    match content {
        Value::Text(_) => true,
        Value::Map(entries) => entries.len() == 1 && matches!(entries[0].0, Value::Text(_)),
        _ => false,
    }
}

/// Puts the content of a body variant onto the tagged object: a map is spread
/// (struct fields camelCased, user-data keys kept), anything else goes under `value`.
fn spread<'a, S: Sink>(
    content: &'a Value,
    ty: &Ty,
    fields: Option<&'static [Field]>,
    sink: &mut S,
    entries: &mut Entries<'a, S>,
) -> Result<(), ShapeError> {
    let ty = peel(ty);
    let Value::Map(pairs) = content else {
        let converted = convert(content, ty, sink)?;
        entries.push((Cow::Borrowed("value"), converted));
        return Ok(());
    };
    let struct_fields = match (fields, ty) {
        (Some(fields), _) => Some(fields),
        (None, Ty::Named(id)) => match &DEFS[*id as usize] {
            Def::Struct { fields, .. } => Some(*fields),
            _ => None,
        },
        _ => None,
    };
    if let Some(fields) = struct_fields {
        return struct_entries(pairs, fields, None, sink, entries);
    }
    match ty {
        Ty::Named(_) => {
            // Internally tagged or tuple-like family: keep it whole instead of guessing a spread.
            let converted = convert(content, ty, sink)?;
            entries.push((Cow::Borrowed("value"), converted));
            Ok(())
        }
        Ty::Map(inner) => map_entries(pairs, inner, sink, entries),
        _ => map_entries(pairs, &Ty::Json, sink, entries),
    }
}

fn struct_entries<'a, S: Sink>(
    pairs: &'a [(Value, Value)],
    fields: &'static [Field],
    keep_key: Option<&str>,
    sink: &mut S,
    entries: &mut Entries<'a, S>,
) -> Result<(), ShapeError> {
    for (key, value) in pairs {
        let name = key_text(key)?;
        let ty = fields.iter().find(|field| field.name == name).map_or(&Ty::Json, |field| &field.ty);
        let out_key = if keep_key == Some(&*name) { name } else { camel_key(name) };
        entries.push((out_key, convert(value, ty, sink)?));
    }
    Ok(())
}

fn map_entries<'a, S: Sink>(
    pairs: &'a [(Value, Value)],
    inner: &Ty,
    sink: &mut S,
    entries: &mut Entries<'a, S>,
) -> Result<(), ShapeError> {
    for (key, value) in pairs {
        entries.push((key_text(key)?, convert(value, inner, sink)?));
    }
    Ok(())
}

fn key_text(key: &Value) -> Result<Cow<'_, str>, ShapeError> {
    match key {
        Value::Text(text) => Ok(Cow::Borrowed(text)),
        Value::Integer(integer) => Ok(Cow::Owned(i128::from(*integer).to_string())),
        _ => Err(ShapeError::BadMapKey),
    }
}

fn camel_key(name: Cow<'_, str>) -> Cow<'_, str> {
    match name {
        Cow::Borrowed(text) => camel(text),
        Cow::Owned(text) => Cow::Owned(camel(&text).into_owned()),
    }
}

/// `node_id` → `nodeId`, the inverse of the `camelToSnakePayload` the GUI applies when sending.
fn camel(key: &str) -> Cow<'_, str> {
    if !key.contains('_') {
        return Cow::Borrowed(key);
    }
    let mut out = String::with_capacity(key.len());
    let mut upper_next = false;
    for ch in key.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}

/// An `Option` is its inner type for shaping: `null` is converted generically anyway.
fn peel(ty: &Ty) -> &Ty {
    match ty {
        Ty::Opt(inner) => peel(inner),
        other => other,
    }
}

fn convert<'a, S: Sink>(value: &'a Value, ty: &Ty, sink: &mut S) -> Result<S::Out, ShapeError> {
    match (peel(ty), value) {
        (Ty::Seq(inner), Value::Array(items)) => {
            let converted = items.iter().map(|item| convert(item, inner, sink)).collect::<Result<_, _>>()?;
            Ok(sink.array(converted))
        }
        (Ty::Map(inner), Value::Map(pairs)) => {
            let mut entries = Vec::with_capacity(pairs.len());
            map_entries(pairs, inner, sink, &mut entries)?;
            Ok(sink.object(entries))
        }
        (Ty::Named(id), _) => convert_def(value, &DEFS[*id as usize], sink),
        _ => generic(value, sink),
    }
}

fn convert_def<'a, S: Sink>(value: &'a Value, def: &'static Def, sink: &mut S) -> Result<S::Out, ShapeError> {
    match (def, value) {
        (Def::Struct { fields, .. }, Value::Map(pairs)) => {
            let mut entries = Vec::with_capacity(pairs.len());
            struct_entries(pairs, fields, None, sink, &mut entries)?;
            Ok(sink.object(entries))
        }
        (Def::Enum { tag: None, variants, .. }, Value::Map(pairs)) if pairs.len() == 1 => {
            let name = key_text(&pairs[0].0)?;
            let converted = match variants.iter().find(|(variant, _)| *variant == name) {
                Some((_, shape)) => convert_shape(&pairs[0].1, shape, sink)?,
                None => generic(&pairs[0].1, sink)?,
            };
            Ok(sink.object(vec![(name, converted)]))
        }
        (Def::Enum { tag: Some(tag_key), content: None, variants, .. }, Value::Map(pairs)) => {
            let selected = pairs
                .iter()
                .find(|(key, _)| key.as_text() == Some(tag_key))
                .and_then(|(_, tag)| tag.as_text())
                .and_then(|name| variants.iter().find(|(variant, _)| *variant == name));
            let mut entries = Vec::with_capacity(pairs.len());
            match selected.map(|(_, shape)| shape) {
                Some(Shape::Struct(fields)) => struct_entries(pairs, fields, Some(tag_key), sink, &mut entries)?,
                Some(Shape::Newtype(Ty::Named(id))) => match &DEFS[*id as usize] {
                    Def::Struct { fields, .. } => struct_entries(pairs, fields, Some(tag_key), sink, &mut entries)?,
                    _ => keep_tag_camel(pairs, tag_key, sink, &mut entries)?,
                },
                _ => keep_tag_camel(pairs, tag_key, sink, &mut entries)?,
            }
            Ok(sink.object(entries))
        }
        (Def::Enum { tag: Some(tag_key), content: Some(content_key), variants, .. }, Value::Map(pairs)) => {
            let selected = pairs
                .iter()
                .find(|(key, _)| key.as_text() == Some(tag_key))
                .and_then(|(_, tag)| tag.as_text())
                .and_then(|name| variants.iter().find(|(variant, _)| *variant == name));
            let mut entries = Vec::with_capacity(pairs.len());
            for (key, item) in pairs {
                let name = key_text(key)?;
                let converted = match (selected, key.as_text() == Some(content_key)) {
                    (Some((_, shape)), true) => convert_shape(item, shape, sink)?,
                    _ => generic(item, sink)?,
                };
                entries.push((name, converted));
            }
            Ok(sink.object(entries))
        }
        _ => generic(value, sink),
    }
}

/// Internally tagged content the schema cannot narrow to a struct: keys other than
/// the tag are still struct-like, so they get camelCased and values pass through.
fn keep_tag_camel<'a, S: Sink>(
    pairs: &'a [(Value, Value)],
    tag_key: &str,
    sink: &mut S,
    entries: &mut Entries<'a, S>,
) -> Result<(), ShapeError> {
    struct_entries(pairs, &[], Some(tag_key), sink, entries)
}

fn convert_shape<'a, S: Sink>(value: &'a Value, shape: &'static Shape, sink: &mut S) -> Result<S::Out, ShapeError> {
    match shape {
        Shape::Unit => generic(value, sink),
        Shape::Newtype(ty) => convert(value, ty, sink),
        Shape::Struct(fields) => match value {
            Value::Map(pairs) => {
                let mut entries = Vec::with_capacity(pairs.len());
                struct_entries(pairs, fields, None, sink, &mut entries)?;
                Ok(sink.object(entries))
            }
            other => generic(other, sink),
        },
    }
}

/// Structure-agnostic conversion: map keys are preserved exactly.
fn generic<S: Sink>(value: &Value, sink: &mut S) -> Result<S::Out, ShapeError> {
    Ok(match value {
        Value::Null => sink.null(),
        Value::Bool(flag) => sink.bool(*flag),
        Value::Float(number) => sink.float(*number),
        Value::Text(text) => sink.text(text),
        Value::Bytes(bytes) => sink.bytes(bytes),
        Value::Integer(integer) => sink.int(int_repr(*integer)),
        Value::Tag(_, inner) => generic(inner, sink)?,
        Value::Array(items) => {
            let converted = items.iter().map(|item| generic(item, sink)).collect::<Result<_, _>>()?;
            sink.array(converted)
        }
        Value::Map(pairs) => {
            let mut entries = Vec::with_capacity(pairs.len());
            for (key, item) in pairs {
                entries.push((key_text(key)?, generic(item, sink)?));
            }
            sink.object(entries)
        }
        _ => return Err(ShapeError::BadMapKey),
    })
}

fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) => "integer",
        Value::Bytes(_) => "bytes",
        Value::Float(_) => "float",
        Value::Text(_) => "text",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        Value::Tag(..) => "tag",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::message_body::{ServiceOauthStartRequest, ServicePayload};
    use tentaflow_protocol::project_studio::access::ProjectAccessPayload;
    use tentaflow_protocol::project_studio::ProjectStudioPayload;
    use tentaflow_protocol::MessageBody;

    #[derive(Debug, PartialEq, Clone)]
    enum Tree {
        Null,
        Bool(bool),
        Int(f64),
        Big(i128),
        Float(f64),
        Text(String),
        Bytes(Vec<u8>),
        Array(Vec<Tree>),
        Object(Vec<(String, Tree)>),
    }

    struct TreeSink;

    impl Sink for TreeSink {
        type Out = Tree;
        fn null(&mut self) -> Tree {
            Tree::Null
        }
        fn bool(&mut self, value: bool) -> Tree {
            Tree::Bool(value)
        }
        fn int(&mut self, value: IntRepr) -> Tree {
            match value {
                IntRepr::Number(number) => Tree::Int(number),
                IntRepr::BigInt(big) => Tree::Big(big),
            }
        }
        fn float(&mut self, value: f64) -> Tree {
            Tree::Float(value)
        }
        fn text(&mut self, value: &str) -> Tree {
            Tree::Text(value.to_string())
        }
        fn bytes(&mut self, value: &[u8]) -> Tree {
            Tree::Bytes(value.to_vec())
        }
        fn array(&mut self, items: Vec<Tree>) -> Tree {
            Tree::Array(items)
        }
        fn object(&mut self, entries: Vec<(Cow<'_, str>, Tree)>) -> Tree {
            Tree::Object(entries.into_iter().map(|(key, value)| (key.into_owned(), value)).collect())
        }
    }

    impl Tree {
        fn get(&self, key: &str) -> &Tree {
            match self {
                Tree::Object(entries) => entries.iter().find(|(name, _)| name == key).map(|(_, v)| v).unwrap_or(&Tree::Null),
                _ => panic!("not an object: {self:?}"),
            }
        }
        fn keys(&self) -> Vec<&str> {
            match self {
                Tree::Object(entries) => entries.iter().map(|(name, _)| name.as_str()).collect(),
                _ => panic!("not an object"),
            }
        }
    }

    fn wire(body: &MessageBody) -> Value {
        let bytes = tentaflow_protocol::cbor::encode(body).expect("encode");
        ciborium::de::from_reader(&bytes[..]).expect("decode as Value")
    }

    fn decode(value: &Value) -> Tree {
        decode_body(value, &mut TreeSink).expect("decode_body")
    }

    fn text(value: &str) -> Value {
        Value::Text(value.into())
    }

    fn map(entries: Vec<(&str, Value)>) -> Value {
        Value::Map(entries.into_iter().map(|(k, v)| (text(k), v)).collect())
    }

    #[test]
    fn unit_variant_is_a_bare_tag() {
        let tree = decode(&wire(&MessageBody::MetaCancelStream));
        assert_eq!(tree, Tree::Object(vec![("_variant".into(), Tree::Text("MetaCancelStream".into()))]));
    }

    #[test]
    fn struct_variant_fields_become_camel_case() {
        let tree = decode(&wire(&MessageBody::MetaHeartbeat { sent_at_epoch: 7 }));
        assert_eq!(tree.get("_variant"), &Tree::Text("MetaHeartbeat".into()));
        assert_eq!(tree.get("sentAtEpoch"), &Tree::Int(7.0));
        assert_eq!(tree.keys(), ["_variant", "sentAtEpoch"]);
    }

    #[test]
    fn wrapper_variant_joins_the_tag_path() {
        let tree = decode(&wire(&MessageBody::ServiceBody(ServicePayload::ReqOauthStart(
            ServiceOauthStartRequest { provider: "github".into(), node_id: None },
        ))));
        assert_eq!(tree.get("_variant"), &Tree::Text("ServiceBody.ReqOauthStart".into()));
        assert_eq!(tree.get("provider"), &Tree::Text("github".into()));
        assert_eq!(tree.keys(), ["_variant", "provider", "nodeId"]);
        assert_eq!(tree.get("nodeId"), &Tree::Null);
    }

    #[test]
    fn nested_wrapper_descends_twice() {
        let tree = decode(&wire(&MessageBody::ProjectStudioBody(ProjectStudioPayload::Access(
            ProjectAccessPayload::CatalogueGetRequest { project_id: "p1".into() },
        ))));
        assert_eq!(tree.get("_variant"), &Tree::Text("ProjectStudioBody.Access.CatalogueGetRequest".into()));
        assert_eq!(tree.get("projectId"), &Tree::Text("p1".into()));
    }

    #[test]
    fn newtype_over_a_struct_spreads_its_fields() {
        let tree = decode(&wire(&MessageBody::ApiKeyCreateResponseBody(
            tentaflow_protocol::message_body::ApiKeyCreateResponse { key_id: "k".into(), token: "t".into() },
        )));
        assert_eq!(tree.get("_variant"), &Tree::Text("ApiKeyCreateResponseBody".into()));
        assert_eq!(tree.keys(), ["_variant", "keyId", "token"]);
    }

    #[test]
    fn user_data_map_keys_survive_while_struct_fields_are_renamed() {
        let body = map(vec![(
            "TentaQuantBody",
            map(vec![(
                "KataListResponse",
                map(vec![
                    ("instance_id", text("i")),
                    (
                        "groups",
                        Value::Array(vec![map(vec![
                            ("group_id", text("g")),
                            ("titles", map(vec![("my_lang_key", text("t"))])),
                            ("kata_count", Value::Integer(3.into())),
                        ])]),
                    ),
                ]),
            )]),
        )]);
        let tree = decode(&body);
        assert_eq!(tree.get("_variant"), &Tree::Text("TentaQuantBody.KataListResponse".into()));
        let Tree::Array(groups) = tree.get("groups") else { panic!("groups") };
        assert_eq!(groups[0].keys(), ["groupId", "titles", "kataCount"]);
        assert_eq!(groups[0].get("titles").keys(), ["my_lang_key"]);
    }

    #[test]
    fn a_wire_field_named_variant_does_not_clash_with_the_tag() {
        let body = map(vec![(
            "LegalAdminBody",
            map(vec![("GenerateRequest", map(vec![("variant", text("short"))]))]),
        )]);
        let tree = decode(&body);
        assert_eq!(tree.get("_variant"), &Tree::Text("LegalAdminBody.GenerateRequest".into()));
        assert_eq!(tree.get("variant"), &Tree::Text("short".into()));
    }

    #[test]
    fn unknown_variant_keeps_its_content_verbatim() {
        let body = map(vec![("FutureThingBody", map(vec![("some_field", Value::Integer(1.into()))]))]);
        let tree = decode(&body);
        assert_eq!(tree.get("_variant"), &Tree::Text("FutureThingBody".into()));
        assert_eq!(tree.get("some_field"), &Tree::Int(1.0));
    }

    #[test]
    fn non_struct_content_goes_under_value() {
        let body = map(vec![("FutureListBody", Value::Array(vec![Value::Integer(1.into())]))]);
        let tree = decode(&body);
        assert_eq!(tree.get("value"), &Tree::Array(vec![Tree::Int(1.0)]));
    }

    #[test]
    fn bytes_stay_bytes() {
        let body = map(vec![("FutureBody", map(vec![("data", Value::Bytes(vec![1, 2, 3]))]))]);
        assert_eq!(decode(&body).get("data"), &Tree::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn rejects_untagged_bodies() {
        let mut sink = TreeSink;
        assert_eq!(decode_body(&Value::Integer(1.into()), &mut sink), Err(ShapeError::NotTagged { found: "integer" }));
        let two = Value::Map(vec![(text("A"), Value::Null), (text("B"), Value::Null)]);
        assert_eq!(decode_body(&two, &mut sink), Err(ShapeError::NotTagged { found: "map" }));
    }

    #[test]
    fn integers_switch_to_bigint_past_the_safe_range() {
        assert_eq!(int_repr(Integer::from(42u8)), IntRepr::Number(42.0));
        assert_eq!(int_repr(Integer::from(-5i8)), IntRepr::Number(-5.0));
        assert_eq!(int_repr(Integer::try_from((1i128 << 53) - 1).unwrap()), IntRepr::Number(9007199254740991.0));
        assert_eq!(int_repr(Integer::try_from(1i128 << 53).unwrap()), IntRepr::BigInt(1 << 53));
        assert_eq!(int_repr(Integer::from(u64::MAX)), IntRepr::BigInt(u64::MAX as i128));
        assert_eq!(int_repr(Integer::from(i64::MIN)), IntRepr::BigInt(i64::MIN as i128));
    }
}
