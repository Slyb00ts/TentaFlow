// =============================================================================
// File: protocol/ui/patch.rs — PatchOp + PatchOpKind (§6.4)
// Purpose: typed state mutation operations used both in StatePatch wire
// messages and embedded in Handler.Backend.optimistic / Handler.Both.optimistic.
// Tagged union schema lives in catalog §6.4 of ADDON_BINARY_PROTOCOL_v1.md.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;
use crate::protocol::value::Value;

use super::bind::StatePath;
use crate::protocol::ui::typed_field::assert_no_dup_tstr;

/// Single mutation applied at a state path.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PatchOp {
    #[n(0)]
    pub path: StatePath,
    #[n(1)]
    pub op: PatchOpKind,
}

/// Discriminated mutation kind.
#[derive(Debug, Clone, PartialEq)]
pub enum PatchOpKind {
    Set { value: Value },
    Delete,
    AppendArray { value: Value },
    PrependArray { value: Value },
    InsertArray { index: u32, value: Value },
    RemoveArray { index: u32 },
    MergeMap { value: CborMap },
    Increment { delta: i64 },
}

impl<C> Encode<C> for PatchOpKind {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order across all variants by full encoded tstr bytes:
        //   "kind"  (0x64 6b 69 6e 64)
        //   "delta" (0x65 64 65 6c 74 61)
        //   "index" (0x65 69 6e 64 65 78)
        //   "value" (0x65 76 61 6c 75 65)
        // So when both 'delta' and 'value' present: kind < delta < value.
        // When 'index' and 'value' present: kind < index < value.
        // When only 'value' present: kind < value.
        match self {
            PatchOpKind::Set { value } => {
                e.map(2)?;
                e.str("kind")?.str("set")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            PatchOpKind::Delete => {
                e.map(1)?;
                e.str("kind")?.str("delete")?;
            }
            PatchOpKind::AppendArray { value } => {
                e.map(2)?;
                e.str("kind")?.str("append_array")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            PatchOpKind::PrependArray { value } => {
                e.map(2)?;
                e.str("kind")?.str("prepend_array")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            PatchOpKind::InsertArray { index, value } => {
                e.map(3)?;
                e.str("kind")?.str("insert_array")?;
                e.str("index")?.u32(*index)?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            PatchOpKind::RemoveArray { index } => {
                e.map(2)?;
                e.str("kind")?.str("remove_array")?;
                e.str("index")?.u32(*index)?;
            }
            PatchOpKind::MergeMap { value } => {
                e.map(2)?;
                e.str("kind")?.str("merge_map")?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            PatchOpKind::Increment { delta } => {
                e.map(2)?;
                e.str("kind")?.str("increment")?;
                e.str("delta")?.i64(*delta)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for PatchOpKind {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<Value> = None;
        let mut index: Option<u32> = None;
        let mut delta: Option<i64> = None;
        let mut map_value: Option<CborMap> = None;
        // For 'value' we need to know whether to decode as Value or CborMap.
        // Strategy: read raw key, peek which kind we're in. For canonical
        // encoding 'kind' comes before 'value' so we already know by then.
        // Fallback for non-canonical input: defer decision by buffering as Value
        // first, then converting if necessary. Here we use kind to dispatch.
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "PatchOpKind", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "index" => {
                    assert_no_dup_tstr(&index, "PatchOpKind", "index")?;
                    index = Some(d.u32()?);
                }
                "delta" => {
                    assert_no_dup_tstr(&delta, "PatchOpKind", "delta")?;
                    delta = Some(d.i64()?);
                }
                "value" => {
                    if value.is_some() || map_value.is_some() {
                        return Err(minicbor::decode::Error::message(
                            "PatchOpKind: duplicate key 'value'",
                        ));
                    }
                    match kind.as_deref() {
                        Some("merge_map") => map_value = Some(CborMap::decode(d, ctx)?),
                        _ => value = Some(Value::decode(d, ctx)?),
                    }
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown PatchOpKind key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("PatchOpKind missing kind"))?;
        let no_extras = |allow_value: bool,
                         allow_index: bool,
                         allow_delta: bool,
                         allow_map_value: bool|
         -> Result<(), minicbor::decode::Error> {
            if !allow_value && value.is_some()
                || !allow_index && index.is_some()
                || !allow_delta && delta.is_some()
                || !allow_map_value && map_value.is_some()
            {
                return Err(minicbor::decode::Error::message(
                    "PatchOpKind variant carries fields not allowed by its kind",
                ));
            }
            Ok(())
        };
        match kind.as_str() {
            "set" => {
                no_extras(true, false, false, false)?;
                Ok(PatchOpKind::Set {
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.set missing value")
                    })?,
                })
            }
            "delete" => {
                no_extras(false, false, false, false)?;
                Ok(PatchOpKind::Delete)
            }
            "append_array" => {
                no_extras(true, false, false, false)?;
                Ok(PatchOpKind::AppendArray {
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.append_array missing value")
                    })?,
                })
            }
            "prepend_array" => {
                no_extras(true, false, false, false)?;
                Ok(PatchOpKind::PrependArray {
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.prepend_array missing value")
                    })?,
                })
            }
            "insert_array" => {
                no_extras(true, true, false, false)?;
                Ok(PatchOpKind::InsertArray {
                    index: index.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.insert_array missing index")
                    })?,
                    value: value.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.insert_array missing value")
                    })?,
                })
            }
            "remove_array" => {
                no_extras(false, true, false, false)?;
                Ok(PatchOpKind::RemoveArray {
                    index: index.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.remove_array missing index")
                    })?,
                })
            }
            "merge_map" => {
                no_extras(false, false, false, true)?;
                Ok(PatchOpKind::MergeMap {
                    value: map_value.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.merge_map missing value")
                    })?,
                })
            }
            "increment" => {
                no_extras(false, false, true, false)?;
                Ok(PatchOpKind::Increment {
                    delta: delta.ok_or_else(|| {
                        minicbor::decode::Error::message("PatchOpKind.increment missing delta")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown PatchOpKind.kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};

    fn rt<T>(v: T)
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
    }

    fn p(seg: &str) -> StatePath {
        StatePath::new(vec![PathSegment::Key(seg.into())])
    }

    #[test]
    fn patch_op_set_roundtrip() {
        rt(PatchOp {
            path: p("count"),
            op: PatchOpKind::Set {
                value: Value::U64(42),
            },
        });
    }

    #[test]
    fn patch_op_kind_all_variants_roundtrip() {
        rt(PatchOpKind::Set {
            value: Value::Bool(true),
        });
        rt(PatchOpKind::Delete);
        rt(PatchOpKind::AppendArray {
            value: Value::Text("x".into()),
        });
        rt(PatchOpKind::PrependArray {
            value: Value::U64(1),
        });
        rt(PatchOpKind::InsertArray {
            index: 2,
            value: Value::Null,
        });
        rt(PatchOpKind::RemoveArray { index: 0 });
        rt(PatchOpKind::MergeMap {
            value: CborMap(vec![("k".into(), Value::U64(7))]),
        });
        rt(PatchOpKind::Increment { delta: -3 });
    }

    #[test]
    fn delete_with_extra_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("delete")
            .unwrap()
            .str("value")
            .unwrap()
            .u8(1)
            .unwrap();
        let res: Result<PatchOpKind, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
