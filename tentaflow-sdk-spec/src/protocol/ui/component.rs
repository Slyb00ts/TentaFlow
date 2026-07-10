// =============================================================================
// File: protocol/ui/component.rs — Component envelope (catalog §1.6)
// Purpose: every UI tree node is a Component { tag, id, fields, handlers,
// bind, a11y, visibility, test_id }. `fields` stays opaque (typed per-tag
// schemas land in later chunks). `handlers` is an ordered list rendered as a
// CBOR map<EventKind, Handler> in canonical bytewise key order.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::value::Value;

use super::a11y::{Accessibility, EventKind, Visibility};
use super::bind::BindSpec;
use super::handler::Handler;

/// Maximum length of `Component.test_id` (catalog §1.6).
pub const TEST_ID_MAX_LEN: usize = 64;

/// Opaque per-component field bag. CBOR-wire form: `map<u8, Value>` — u8 key
/// canonical sort matches numeric u8 ordering, so we just keep entries sorted
/// in Rust.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldMap(pub Vec<(u8, Value)>);

impl<C> Encode<C> for FieldMap {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: sort by u8 key. Single-byte CBOR encoding (0..23) and
        // 2-byte encoding (24..255) both order numerically with the u8 value.
        let mut sorted: Vec<&(u8, Value)> = self.0.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        e.map(sorted.len() as u64)?;
        let mut prev: Option<u8> = None;
        for (k, _) in &sorted {
            if let Some(p) = prev {
                if p == *k {
                    return Err(minicbor::encode::Error::message(
                        "FieldMap: duplicate u8 key",
                    ));
                }
            }
            prev = Some(*k);
        }
        for (k, v) in sorted {
            e.u8(*k)?;
            v.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FieldMap {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut entries = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let k = d.u8()?;
            let v = Value::decode(d, ctx)?;
            entries.push((k, v));
        }
        Ok(FieldMap(entries))
    }
}

/// Ordered list of (EventKind, Handler) emitted on the wire as a tstr-keyed map.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HandlerMap(pub Vec<(EventKind, Handler)>);

impl<C> Encode<C> for HandlerMap {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: sort by bytewise CBOR encoding of EventKind's tstr form.
        let mut indexed: Vec<(Vec<u8>, &(EventKind, Handler))> = Vec::with_capacity(self.0.len());
        for entry in &self.0 {
            let mut k_bytes = Vec::new();
            entry
                .0
                .encode(&mut Encoder::new(&mut k_bytes), ctx)
                .expect("Vec writer infallible; EventKind::encode only fails on writer errors");
            indexed.push((k_bytes, entry));
        }
        indexed.sort_by(|a, b| a.0.cmp(&b.0));
        let mut prev: Option<&Vec<u8>> = None;
        for (b, _) in &indexed {
            if let Some(p) = prev {
                if p == b {
                    return Err(minicbor::encode::Error::message(
                        "HandlerMap: duplicate EventKind key",
                    ));
                }
            }
            prev = Some(b);
        }
        e.map(self.0.len() as u64)?;
        for (_, (k, v)) in &indexed {
            k.encode(e, ctx)?;
            v.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for HandlerMap {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut entries = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let k = EventKind::decode(d, ctx)?;
            let v = Handler::decode(d, ctx)?;
            entries.push((k, v));
        }
        Ok(HandlerMap(entries))
    }
}

/// Newtype for a Component test_id. Decoder validates `[a-z0-9_-]+`, length ≤ 64.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestId(String);

impl TestId {
    /// Construct after validating against the catalog §1.6 grammar.
    pub fn new(value: impl Into<String>) -> Result<Self, TestIdError> {
        let s: String = value.into();
        if s.is_empty() || s.len() > TEST_ID_MAX_LEN {
            return Err(TestIdError::Length);
        }
        if !s
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        {
            return Err(TestIdError::Charset);
        }
        Ok(TestId(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors produced by [`TestId::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestIdError {
    Length,
    Charset,
}

impl core::fmt::Display for TestIdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Length => write!(f, "test_id length must be 1..={TEST_ID_MAX_LEN}"),
            Self::Charset => write!(f, "test_id must match [a-z0-9_-]+"),
        }
    }
}

impl std::error::Error for TestIdError {}

impl<C> Encode<C> for TestId {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.str(&self.0)?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for TestId {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let s = d.str()?;
        TestId::new(s).map_err(|err| minicbor::decode::Error::message(err.to_string()))
    }
}

/// CBOR envelope every UI component shares (catalog §1.6).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Component {
    /// Stable wire discriminant (see catalog §2–§7 tag tables).
    #[n(0)]
    pub tag: u16,
    /// Unique id within the panel.
    #[n(1)]
    pub id: String,
    /// Per-tag field schema, encoded opaquely as `map<u8, Value>` here.
    #[n(2)]
    pub fields: FieldMap,
    /// Event handlers attached to this component.
    #[n(3)]
    pub handlers: Option<HandlerMap>,
    /// Reactive binding declaration (catalog §1.4).
    #[n(4)]
    pub bind: Option<BindSpec>,
    /// ARIA / a11y metadata.
    #[n(5)]
    pub a11y: Option<Accessibility>,
    /// Responsive visibility metadata.
    #[n(6)]
    pub visibility: Option<Visibility>,
    /// Stable identifier for E2E / instrumentation. Validated `[a-z0-9_-]+`, ≤ 64.
    #[n(7)]
    pub test_id: Option<TestId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::handler::{Handler, LocalAction};

    #[test]
    fn component_minimal_roundtrip() {
        let c = Component {
            tag: 0x0401,
            id: "save_btn".into(),
            fields: FieldMap(vec![(0, Value::Text("Save".into()))]),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&c, &mut buf).unwrap();
        let d: Component = minicbor::decode(&buf).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn component_with_handlers_roundtrip() {
        let handlers = HandlerMap(vec![
            (EventKind::Click, Handler::Local(LocalAction::Noop)),
            (
                EventKind::Focus,
                Handler::Local(LocalAction::Focus {
                    component_id: "input1".into(),
                }),
            ),
        ]);
        let c = Component {
            tag: 0x0401,
            id: "btn".into(),
            fields: FieldMap::default(),
            handlers: Some(handlers),
            bind: None,
            a11y: None,
            visibility: None,
            test_id: Some(TestId::new("save-button-1").unwrap()),
        };
        let mut buf = Vec::new();
        minicbor::encode(&c, &mut buf).unwrap();
        let d: Component = minicbor::decode(&buf).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn field_map_canonical_order_after_encode() {
        let fm = FieldMap(vec![
            (25, Value::U64(1)),
            (1, Value::U64(2)),
            (5, Value::U64(3)),
        ]);
        let mut buf = Vec::new();
        minicbor::encode(&fm, &mut buf).unwrap();
        let d: FieldMap = minicbor::decode(&buf).unwrap();
        let keys: Vec<u8> = d.0.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![1, 5, 25]);
    }

    #[test]
    fn handler_map_canonical_order_after_encode() {
        let hm = HandlerMap(vec![
            (EventKind::FilesSelected, Handler::Local(LocalAction::Noop)),
            (EventKind::Click, Handler::Local(LocalAction::Noop)),
            (EventKind::Focus, Handler::Local(LocalAction::Noop)),
        ]);
        let mut buf = Vec::new();
        minicbor::encode(&hm, &mut buf).unwrap();
        let d: HandlerMap = minicbor::decode(&buf).unwrap();
        // After canonical sort by encoded bytes: "click" (5) < "focus" (5) <
        // "files_selected" (14). Both "click" and "focus" share header 0x65;
        // bytewise: c (0x63) < f (0x66) → click first, focus second.
        let kinds: Vec<EventKind> = d.0.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![EventKind::Click, EventKind::Focus, EventKind::FilesSelected]
        );
    }

    #[test]
    fn test_id_invalid_charset_rejected() {
        assert_eq!(TestId::new("Foo!"), Err(TestIdError::Charset));
        assert_eq!(TestId::new(""), Err(TestIdError::Length));
        let too_long: String = std::iter::repeat('a').take(TEST_ID_MAX_LEN + 1).collect();
        assert_eq!(TestId::new(too_long), Err(TestIdError::Length));
    }

    #[test]
    fn test_id_decode_rejects_uppercase() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("Foo").unwrap();
        let res: Result<TestId, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
