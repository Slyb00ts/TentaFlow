// =============================================================================
// File: protocol/ui/event.rs — Event + Topic (§6.7)
// Purpose: pub/sub event message. Topic is a compiled structured pattern
// (literal segments + named id segments); admin UI approves the manifest
// patterns ahead of time — runtime can't escalate via glob strings.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::ui::typed_field::assert_no_dup_tstr;
use crate::protocol::value::Value;

/// One segment of a compiled Topic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TopicSegment {
    Literal { value: String },
    Id { value: String },
}

impl<C> Encode<C> for TopicSegment {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: kind(0x64..) < value(0x65..).
        e.map(2)?;
        e.str("kind")?;
        match self {
            TopicSegment::Literal { .. } => {
                e.str("literal")?;
            }
            TopicSegment::Id { .. } => {
                e.str("id")?;
            }
        }
        e.str("value")?;
        match self {
            TopicSegment::Literal { value } | TopicSegment::Id { value } => {
                e.str(value)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for TopicSegment {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value: Option<String> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "TopicSegment", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    assert_no_dup_tstr(&value, "TopicSegment", "value")?;
                    value = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown TopicSegment key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("TopicSegment missing kind"))?;
        let value =
            value.ok_or_else(|| minicbor::decode::Error::message("TopicSegment missing value"))?;
        match kind.as_str() {
            "literal" => Ok(TopicSegment::Literal { value }),
            "id" => Ok(TopicSegment::Id { value }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown TopicSegment.kind: {other}"
            ))),
        }
    }
}

/// Compiled topic pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Topic {
    pub segments: Vec<TopicSegment>,
}

impl Topic {
    pub fn new(segments: Vec<TopicSegment>) -> Self {
        Self { segments }
    }
}

impl<C> Encode<C> for Topic {
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

impl<'b, C> Decode<'b, C> for Topic {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let n = d
            .array()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length array forbidden"))?;
        let mut segments = Vec::with_capacity(n as usize);
        for _ in 0..n {
            segments.push(TopicSegment::decode(d, ctx)?);
        }
        Ok(Topic { segments })
    }
}

/// `Event` (0x0150). Bidirectional.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Event {
    #[n(0)]
    pub source_addon_id: String,
    #[n(1)]
    pub topic: Topic,
    #[n(2)]
    pub payload: Value,
    /// Event source time; may diverge from `Envelope.ts_ms`.
    #[n(3)]
    pub ts_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: T = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn topic_segment_variants_roundtrip() {
        rt(TopicSegment::Literal {
            value: "tentavision".into(),
        });
        rt(TopicSegment::Id {
            value: "camera-1".into(),
        });
    }

    #[test]
    fn topic_roundtrip() {
        rt(Topic::new(vec![
            TopicSegment::Literal {
                value: "tentavision".into(),
            },
            TopicSegment::Literal {
                value: "alert".into(),
            },
            TopicSegment::Id {
                value: "warn".into(),
            },
        ]));
    }

    #[test]
    fn event_roundtrip() {
        rt(Event {
            source_addon_id: "tentavision".into(),
            topic: Topic::new(vec![TopicSegment::Literal {
                value: "ping".into(),
            }]),
            payload: Value::U64(1),
            ts_ms: 1_700_000_000_000,
        });
    }

    #[test]
    fn topic_segment_unknown_kind_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("wildcard")
            .unwrap()
            .str("value")
            .unwrap()
            .str("x")
            .unwrap();
        let res: Result<TopicSegment, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
