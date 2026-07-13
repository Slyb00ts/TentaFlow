// =============================================================================
// File: protocol/ui/bind.rs — StatePath, PathSegment, BindRef, BindSpec
// Purpose: reactive state path + binding declarations from catalog §1.4 and
// protocol §6.4. Decoders reject paths > 32 segments (§6 ServerLimits) and
// foreign per-variant fields in tagged unions.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::ui::typed_field::assert_no_dup_tstr;
use crate::protocol::value::Value;

/// Maximum number of segments per StatePath (matches ServerLimits.max_state_path_segments).
pub const MAX_STATE_PATH_SEGMENTS: usize = 32;

/// One segment of a StatePath: either a map key or an array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSegment {
    Key(String),
    Index(u32),
}

impl<C> Encode<C> for PathSegment {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order: "kind"(0x64..) < "value"(0x65..).
        e.map(2)?;
        e.str("kind")?;
        match self {
            PathSegment::Key(_) => {
                e.str("key")?;
            }
            PathSegment::Index(_) => {
                e.str("index")?;
            }
        }
        e.str("value")?;
        match self {
            PathSegment::Key(s) => {
                e.str(s)?;
            }
            PathSegment::Index(i) => {
                e.u32(*i)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for PathSegment {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut key_value: Option<String> = None;
        let mut index_value: Option<u32> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "PathSegment", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    // Either slot already populated → duplicate `value` key.
                    if key_value.is_some() || index_value.is_some() {
                        return Err(minicbor::decode::Error::message(
                            "PathSegment: duplicate key 'value'",
                        ));
                    }
                    // Value type depends on already-known kind. If kind not yet
                    // seen we decode as either tstr or u32 by peeking datatype.
                    match kind.as_deref() {
                        Some("key") => key_value = Some(d.str()?.to_string()),
                        Some("index") => index_value = Some(d.u32()?),
                        _ => {
                            // Type-peek fallback (canonical-order maps put kind first).
                            match d.datatype()? {
                                minicbor::data::Type::String => {
                                    key_value = Some(d.str()?.to_string());
                                }
                                minicbor::data::Type::U8
                                | minicbor::data::Type::U16
                                | minicbor::data::Type::U32
                                | minicbor::data::Type::U64 => {
                                    index_value = Some(d.u32()?);
                                }
                                _ => {
                                    return Err(minicbor::decode::Error::message(
                                        "PathSegment.value: unsupported type",
                                    ));
                                }
                            }
                        }
                    }
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown PathSegment key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("PathSegment missing kind"))?;
        match kind.as_str() {
            "key" => {
                if index_value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "PathSegment.key must carry a tstr value, not integer",
                    ));
                }
                Ok(PathSegment::Key(key_value.ok_or_else(|| {
                    minicbor::decode::Error::message("PathSegment.key missing value")
                })?))
            }
            "index" => {
                if key_value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "PathSegment.index must carry an integer value, not tstr",
                    ));
                }
                Ok(PathSegment::Index(index_value.ok_or_else(|| {
                    minicbor::decode::Error::message("PathSegment.index missing value")
                })?))
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown PathSegment.kind: {other}"
            ))),
        }
    }
}

/// Strongly-typed dotted/indexed state path. Max 32 segments enforced on decode.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StatePath {
    pub segments: Vec<PathSegment>,
}

impl StatePath {
    pub fn new(segments: Vec<PathSegment>) -> Self {
        Self { segments }
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

impl<C> Encode<C> for StatePath {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(self.segments.len() as u64)?;
        for seg in &self.segments {
            seg.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for StatePath {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let n = d
            .array()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length array forbidden"))?;
        if (n as usize) > MAX_STATE_PATH_SEGMENTS {
            return Err(minicbor::decode::Error::message(
                "StatePath exceeds MAX_STATE_PATH_SEGMENTS (32)",
            ));
        }
        let mut segments = Vec::with_capacity(n as usize);
        for _ in 0..n {
            segments.push(PathSegment::decode(d, ctx)?);
        }
        Ok(StatePath { segments })
    }
}

/// Reference to either a literal value or a state path.
#[derive(Debug, Clone, PartialEq)]
pub enum BindRef {
    Literal(Value),
    Bound(StatePath),
}

impl<C> Encode<C> for BindRef {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical order: "kind"(0x64..) < "path"(0x64..) < "value"(0x65..).
        // "kind"=[0x64 6b 69 6e 64], "path"=[0x64 70 61 74 68], "value"=[0x65 76 ...].
        // kind < path < value.
        match self {
            BindRef::Literal(v) => {
                e.map(2)?;
                e.str("kind")?.str("literal")?;
                e.str("value")?;
                v.encode(e, ctx)?;
            }
            BindRef::Bound(path) => {
                e.map(2)?;
                e.str("kind")?.str("bound")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for BindRef {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        let mut path: Option<StatePath> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "BindRef", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "BindRef", "value")?;
                    value = Some(Value::decode(d, ctx)?);
                }
                "path" => {
                    assert_no_dup_tstr(&path, "BindRef", "path")?;
                    path = Some(StatePath::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown BindRef key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("BindRef missing kind"))?;
        match kind.as_str() {
            "literal" => {
                if path.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "BindRef.literal must not carry path",
                    ));
                }
                Ok(BindRef::Literal(value.ok_or_else(|| {
                    minicbor::decode::Error::message("BindRef.literal missing value")
                })?))
            }
            "bound" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "BindRef.bound must not carry value",
                    ));
                }
                Ok(BindRef::Bound(path.ok_or_else(|| {
                    minicbor::decode::Error::message("BindRef.bound missing path")
                })?))
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown BindRef.kind: {other}"
            ))),
        }
    }
}

/// Declarative reactive binding attached to a component (§1.4).
#[derive(Debug, Clone, PartialEq)]
pub enum BindSpec {
    /// Bind text content of the element to a state path; optional format applied.
    Text {
        path: StatePath,
        format: Option<super::value_format::ValueFormat>,
    },
    /// Bind an HTML attribute to a state path.
    Attr { name: String, path: StatePath },
    /// Toggle a CSS class based on a boolean state path.
    ClassToggle {
        class_name: String,
        path: StatePath,
        negate: bool,
    },
    /// Show/hide the element based on a truthy state path.
    Show { path: StatePath, negate: bool },
    /// Render a list by iterating a state array, instantiating a fragment template.
    List {
        path: StatePath,
        item_template_id: String,
        key_field: Option<String>,
    },
    /// Two-way binding (form fields only). Renderer writes input back into state.
    TwoWay { path: StatePath },
}

impl<C> Encode<C> for BindSpec {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Encoded as {kind: tstr, ...variant_fields}. Keys emitted in canonical
        // bytewise tstr order. We compute and emit per-variant explicitly to
        // avoid runtime sort cost for fixed shapes.
        match self {
            // class_toggle:
            //   "class_name"(0x6a..) < "kind"(0x64..) — wait, 0x6a > 0x64; recompute.
            // Recompute canonical order for involved keys:
            //   "format"      [0x66 66 6f 72 6d 61 74]                      (header 0x66)
            //   "kind"        [0x64 6b 69 6e 64]                            (header 0x64)
            //   "key_field"   [0x69 6b 65 79 5f 66 69 65 6c 64]              (header 0x69)
            //   "name"        [0x64 6e 61 6d 65]                             (header 0x64)
            //   "negate"      [0x66 6e 65 67 61 74 65]                       (header 0x66)
            //   "path"        [0x64 70 61 74 68]                             (header 0x64)
            //   "class_name"  [0x6a 63 6c 61 73 73 5f 6e 61 6d 65]            (header 0x6a)
            //   "item_template_id" [0x70 69 74 65 6d ...]                    (header 0x70)
            // Sorted by full bytes:
            //   "kind" (0x64 6b...) < "name" (0x64 6e...) < "path" (0x64 70...)
            //   < "format" (0x66 66...) < "negate" (0x66 6e...)
            //   < "key_field" (0x69 6b...) < "class_name" (0x6a 63...)
            //   < "item_template_id" (0x70 ...)
            BindSpec::Text { path, format } => {
                let n = if format.is_some() { 3 } else { 2 };
                e.map(n)?;
                e.str("kind")?.str("text")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                if let Some(fmt) = format {
                    e.str("format")?;
                    fmt.encode(e, ctx)?;
                }
            }
            BindSpec::Attr { name, path } => {
                e.map(3)?;
                e.str("kind")?.str("attr")?;
                e.str("name")?.str(name)?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
            BindSpec::ClassToggle {
                class_name,
                path,
                negate,
            } => {
                e.map(4)?;
                e.str("kind")?.str("class_toggle")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("negate")?.bool(*negate)?;
                e.str("class_name")?.str(class_name)?;
            }
            BindSpec::Show { path, negate } => {
                e.map(3)?;
                e.str("kind")?.str("show")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("negate")?.bool(*negate)?;
            }
            BindSpec::List {
                path,
                item_template_id,
                key_field,
            } => {
                let n = if key_field.is_some() { 4 } else { 3 };
                e.map(n)?;
                e.str("kind")?.str("list")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                if let Some(kf) = key_field {
                    e.str("key_field")?.str(kf)?;
                }
                e.str("item_template_id")?.str(item_template_id)?;
            }
            BindSpec::TwoWay { path } => {
                e.map(2)?;
                e.str("kind")?.str("two_way")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for BindSpec {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut path: Option<StatePath> = None;
        let mut format: Option<super::value_format::ValueFormat> = None;
        let mut name: Option<String> = None;
        let mut class_name: Option<String> = None;
        let mut negate: Option<bool> = None;
        let mut item_template_id: Option<String> = None;
        let mut key_field: Option<String> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "BindSpec", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "path" => {
                    assert_no_dup_tstr(&path, "BindSpec", "path")?;
                    path = Some(StatePath::decode(d, ctx)?);
                }
                "format" => {
                    assert_no_dup_tstr(&format, "BindSpec", "format")?;
                    format = Some(super::value_format::ValueFormat::decode(d, ctx)?)
                }
                "name" => {
                    assert_no_dup_tstr(&name, "BindSpec", "name")?;
                    name = Some(d.str()?.to_string());
                }
                "class_name" => {
                    assert_no_dup_tstr(&class_name, "BindSpec", "class_name")?;
                    class_name = Some(d.str()?.to_string());
                }
                "negate" => {
                    assert_no_dup_tstr(&negate, "BindSpec", "negate")?;
                    negate = Some(d.bool()?);
                }
                "item_template_id" => {
                    assert_no_dup_tstr(&item_template_id, "BindSpec", "item_template_id")?;
                    item_template_id = Some(d.str()?.to_string());
                }
                "key_field" => {
                    assert_no_dup_tstr(&key_field, "BindSpec", "key_field")?;
                    key_field = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown BindSpec key: {other}"
                    )))
                }
            }
        }
        let kind = kind.ok_or_else(|| minicbor::decode::Error::message("BindSpec missing kind"))?;
        let no_extras = |allow_format: bool,
                         allow_name: bool,
                         allow_class: bool,
                         allow_negate: bool,
                         allow_item_template: bool,
                         allow_key_field: bool|
         -> Result<(), minicbor::decode::Error> {
            if !allow_format && format.is_some()
                || !allow_name && name.is_some()
                || !allow_class && class_name.is_some()
                || !allow_negate && negate.is_some()
                || !allow_item_template && item_template_id.is_some()
                || !allow_key_field && key_field.is_some()
            {
                return Err(minicbor::decode::Error::message(
                    "BindSpec variant carries fields not allowed by its kind",
                ));
            }
            Ok(())
        };
        let take_path = |path: Option<StatePath>| -> Result<StatePath, minicbor::decode::Error> {
            path.ok_or_else(|| minicbor::decode::Error::message("BindSpec missing path"))
        };
        match kind.as_str() {
            "text" => {
                no_extras(true, false, false, false, false, false)?;
                Ok(BindSpec::Text {
                    path: take_path(path)?,
                    format,
                })
            }
            "attr" => {
                no_extras(false, true, false, false, false, false)?;
                Ok(BindSpec::Attr {
                    name: name.ok_or_else(|| {
                        minicbor::decode::Error::message("BindSpec.attr missing name")
                    })?,
                    path: take_path(path)?,
                })
            }
            "class_toggle" => {
                no_extras(false, false, true, true, false, false)?;
                Ok(BindSpec::ClassToggle {
                    class_name: class_name.ok_or_else(|| {
                        minicbor::decode::Error::message("BindSpec.class_toggle missing class_name")
                    })?,
                    path: take_path(path)?,
                    negate: negate.ok_or_else(|| {
                        minicbor::decode::Error::message("BindSpec.class_toggle missing negate")
                    })?,
                })
            }
            "show" => {
                no_extras(false, false, false, true, false, false)?;
                Ok(BindSpec::Show {
                    path: take_path(path)?,
                    negate: negate.ok_or_else(|| {
                        minicbor::decode::Error::message("BindSpec.show missing negate")
                    })?,
                })
            }
            "list" => {
                no_extras(false, false, false, false, true, true)?;
                Ok(BindSpec::List {
                    path: take_path(path)?,
                    item_template_id: item_template_id.ok_or_else(|| {
                        minicbor::decode::Error::message("BindSpec.list missing item_template_id")
                    })?,
                    key_field,
                })
            }
            "two_way" => {
                no_extras(false, false, false, false, false, false)?;
                Ok(BindSpec::TwoWay {
                    path: take_path(path)?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown BindSpec.kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::value::Value;

    fn rt<T>(v: T) -> T
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf1 = Vec::new();
        minicbor::encode(&v, &mut buf1).unwrap();
        let d: T = minicbor::decode(&buf1).unwrap();
        assert_eq!(d, v);
        let mut buf2 = Vec::new();
        minicbor::encode(&d, &mut buf2).unwrap();
        assert_eq!(buf1, buf2);
        v
    }

    #[test]
    fn state_path_roundtrip() {
        rt(StatePath::new(vec![
            PathSegment::Key("cameras".into()),
            PathSegment::Index(5),
            PathSegment::Key("status".into()),
        ]));
    }

    #[test]
    fn state_path_too_long_rejected() {
        let segs = (0..33).map(PathSegment::Index).collect::<Vec<_>>();
        let path = StatePath::new(segs);
        let mut buf = Vec::new();
        minicbor::encode(&path, &mut buf).unwrap();
        let res: Result<StatePath, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn bind_ref_roundtrip_both_variants() {
        rt(BindRef::Literal(Value::U64(42)));
        rt(BindRef::Bound(StatePath::new(vec![PathSegment::Key(
            "a".into(),
        )])));
    }

    #[test]
    fn bind_ref_literal_with_path_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("literal")
            .unwrap()
            .str("path")
            .unwrap()
            .array(0)
            .unwrap()
            .str("value")
            .unwrap()
            .u8(1)
            .unwrap();
        let res: Result<BindRef, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn bind_spec_text_with_format_roundtrip() {
        use crate::protocol::ui::value_format::ValueFormat;
        rt(BindSpec::Text {
            path: StatePath::new(vec![PathSegment::Key("v".into())]),
            format: Some(ValueFormat::Percent { decimals: 1 }),
        });
    }

    #[test]
    fn bind_spec_show_negate() {
        rt(BindSpec::Show {
            path: StatePath::new(vec![PathSegment::Key("expanded".into())]),
            negate: true,
        });
    }

    #[test]
    fn bind_spec_list_with_key_field() {
        rt(BindSpec::List {
            path: StatePath::new(vec![PathSegment::Key("rows".into())]),
            item_template_id: "row_tpl".into(),
            key_field: Some("id".into()),
        });
    }

    #[test]
    fn bind_spec_unknown_kind_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(1)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("future")
            .unwrap();
        let res: Result<BindSpec, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn path_segment_kind_index_with_tstr_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("index")
            .unwrap()
            .str("value")
            .unwrap()
            .str("not_an_int")
            .unwrap();
        let res: Result<PathSegment, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
