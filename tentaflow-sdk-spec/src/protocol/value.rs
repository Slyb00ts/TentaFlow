// =============================================================================
// File: protocol/value.rs — generic CBOR Value for opaque payload fields
// Purpose: typed representation of arbitrary CBOR data items used where the
// protocol declares `Value` or `map<tstr, Value>` (e.g. Capability.params,
// Event.payload). Refuses indefinite-length items per §2.1 canonical profile.
// =============================================================================

use minicbor::data::Type;
use minicbor::{Decode, Decoder, Encode, Encoder};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    F64(f64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
}

impl<C> Encode<C> for Value {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            Value::Null => {
                e.null()?;
            }
            Value::Bool(b) => {
                e.bool(*b)?;
            }
            Value::U64(n) => {
                e.u64(*n)?;
            }
            Value::I64(n) => {
                e.i64(*n)?;
            }
            Value::F64(f) => {
                e.f64(*f)?;
            }
            Value::Bytes(b) => {
                e.bytes(b)?;
            }
            Value::Text(s) => {
                e.str(s)?;
            }
            Value::Array(items) => {
                e.array(items.len() as u64)?;
                for item in items {
                    item.encode(e, ctx)?;
                }
            }
            Value::Map(entries) => {
                // Canonical key order (§2.1): sort by bytewise CBOR encoding of key.
                let mut indexed: Vec<(Vec<u8>, &(Value, Value))> =
                    Vec::with_capacity(entries.len());
                for entry in entries {
                    let mut key_bytes = Vec::new();
                    {
                        let mut key_enc = Encoder::new(&mut key_bytes);
                        entry.0.encode(&mut key_enc, ctx).expect(
                            "Vec writer is infallible; Value::encode only fails on writer errors",
                        );
                    }
                    indexed.push((key_bytes, entry));
                }
                indexed.sort_by(|a, b| a.0.cmp(&b.0));
                e.map(entries.len() as u64)?;
                for (_, (k, v)) in &indexed {
                    k.encode(e, ctx)?;
                    v.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Value {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        match d.datatype()? {
            Type::Null => {
                d.null()?;
                Ok(Value::Null)
            }
            Type::Undefined => Err(minicbor::decode::Error::message(
                "CBOR `undefined` is not allowed in deterministic profile",
            )),
            Type::Bool => Ok(Value::Bool(d.bool()?)),
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => Ok(Value::U64(d.u64()?)),
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => Ok(Value::I64(d.i64()?)),
            Type::F16 | Type::F32 | Type::F64 => Ok(Value::F64(d.f64()?)),
            Type::Bytes => Ok(Value::Bytes(d.bytes()?.to_vec())),
            Type::String => Ok(Value::Text(d.str()?.to_string())),
            Type::Array => {
                let len = d.array()?.ok_or_else(|| {
                    minicbor::decode::Error::message(
                        "indefinite-length array forbidden by canonical profile",
                    )
                })?;
                let mut v = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    v.push(Value::decode(d, ctx)?);
                }
                Ok(Value::Array(v))
            }
            Type::Map => {
                let len = d.map()?.ok_or_else(|| {
                    minicbor::decode::Error::message(
                        "indefinite-length map forbidden by canonical profile",
                    )
                })?;
                let mut entries = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let k = Value::decode(d, ctx)?;
                    let v = Value::decode(d, ctx)?;
                    entries.push((k, v));
                }
                Ok(Value::Map(entries))
            }
            Type::BytesIndef | Type::StringIndef | Type::ArrayIndef | Type::MapIndef => {
                Err(minicbor::decode::Error::message(
                    "indefinite-length items forbidden by canonical profile",
                ))
            }
            Type::Tag => Err(minicbor::decode::Error::message(
                "CBOR semantic tags are not allowed in v1 (see §2.3)",
            )),
            Type::Simple => Err(minicbor::decode::Error::message(
                "unknown CBOR simple value",
            )),
            Type::Break => Err(minicbor::decode::Error::message(
                "unexpected CBOR break stop code",
            )),
            Type::Unknown(_) => Err(minicbor::decode::Error::message(
                "unsupported CBOR major type",
            )),
            Type::Int => Err(minicbor::decode::Error::message(
                "ambiguous CBOR integer width — encoder must use preferred serialization",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: &Value) -> Value {
        let mut buf = Vec::new();
        minicbor::encode(v, &mut buf).expect("encode");
        minicbor::decode(&buf).expect("decode")
    }

    #[test]
    fn roundtrip_scalars() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::U64(0),
            Value::U64(23),
            Value::U64(24),
            Value::U64(u64::MAX),
            Value::I64(-1),
            Value::I64(-256),
            Value::Text("café".into()),
            Value::Bytes(vec![0, 1, 2, 3]),
        ] {
            assert_eq!(roundtrip(&v), v);
        }
    }

    #[test]
    fn roundtrip_nested() {
        let v = Value::Map(vec![
            (
                Value::Text("a".into()),
                Value::Array(vec![Value::U64(1), Value::U64(2)]),
            ),
            (Value::Text("b".into()), Value::Null),
        ]);
        assert_eq!(roundtrip(&v), v);
    }
}
