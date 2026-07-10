// =============================================================================
// File: protocol/stream.rs — stream-channel typed wire messages (§7)
// Purpose: long-running operations — LLM token streaming, file upload/download,
// video frame previews, search-results pagination. Per-stream credit pool
// (32 chunks default) lives in the host validator; this module only defines
// the typed wire shapes.
//
// Soft-constraint validation deferred to Krok 4 host validator (same policy as
// the lib.rs strict-decode note): `StreamProgress.percent` accepts the full
// `u8` range here but §7.1 narrows to `0..=100`; `message` / `reason` strings
// across §7 are capped at 256 chars by §7.1 / §9 (`max_string_bytes`), but
// type-level enforcement happens in the host validator, not at decode time.
// Wire-encoded canonical CBOR remains the authoritative contract here.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;
use crate::protocol::ui::error_code::ErrorCode;

string_enum! {
    /// Negotiated stream kind shipped in StreamOpen.kind (§7.1).
    pub enum StreamKind {
        LlmTokenStream = "llm_token_stream",
        FileUpload = "file_upload",
        FileDownload = "file_download",
        VideoFramePreview = "video_frame_preview",
        SearchResults = "search_results",
        Custom = "custom",
    }
}

/// `StreamOpen` (0x0301). Initiator → peer.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamOpen {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub kind: StreamKind,
    /// Kind-specific metadata. For `Custom` the addon-defined identifier lives
    /// in this map (e.g. `metadata.custom_name`).
    #[n(2)]
    pub metadata: CborMap,
    #[n(3)]
    pub expected_total_bytes: Option<u64>,
    /// Default true. False enters datagram mode and requires the
    /// `webtransport_datagrams` capability.
    #[n(4)]
    pub reliable: bool,
}

/// `StreamAccepted` (0x0302). Peer → initiator. Confirms negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamAccepted {
    #[n(0)]
    pub stream_id: u32,
}

/// `StreamRejected` (0x0303). Peer → initiator.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamRejected {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub code: ErrorCode,
    #[n(2)]
    pub message: String,
}

/// `StreamChunk` (0x0310). Bidirectional.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamChunk {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub sequence: u32,
    #[n(2)]
    pub data: Vec<u8>,
    #[n(3)]
    pub end_of_stream: bool,
}

/// `StreamProgress` (0x0311). Producer → consumer.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamProgress {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub bytes_processed: Option<u64>,
    #[n(2)]
    pub items_processed: Option<u64>,
    /// 0..=100 when total is known; absent otherwise.
    #[n(3)]
    pub percent: Option<u8>,
    #[n(4)]
    pub message: Option<String>,
}

/// `StreamEnd` (0x0320). Producer → consumer. Terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamEnd {
    #[n(0)]
    pub stream_id: u32,
}

/// `StreamCancel` (0x0321). Bidirectional. Abort with reason.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamCancel {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub reason: String,
}

/// `StreamError` (0x0322). Producer → consumer. Fatal stream-scoped error.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct StreamError {
    #[n(0)]
    pub stream_id: u32,
    #[n(1)]
    pub code: ErrorCode,
    #[n(2)]
    pub message: String,
}

/// Wire tags for stream-channel payloads (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum StreamTag {
    Open = 0x0301,
    Accepted = 0x0302,
    Rejected = 0x0303,
    Chunk = 0x0310,
    Progress = 0x0311,
    End = 0x0320,
    Cancel = 0x0321,
    Error = 0x0322,
}

impl StreamTag {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x0301 => Self::Open,
            0x0302 => Self::Accepted,
            0x0303 => Self::Rejected,
            0x0310 => Self::Chunk,
            0x0311 => Self::Progress,
            0x0320 => Self::End,
            0x0321 => Self::Cancel,
            0x0322 => Self::Error,
            _ => return None,
        })
    }
}

/// Discriminated union over all §7 stream-channel payloads.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamPayload {
    Open(StreamOpen),
    Accepted(StreamAccepted),
    Rejected(StreamRejected),
    Chunk(StreamChunk),
    Progress(StreamProgress),
    End(StreamEnd),
    Cancel(StreamCancel),
    Error(StreamError),
}

impl StreamPayload {
    pub fn tag(&self) -> StreamTag {
        match self {
            Self::Open(_) => StreamTag::Open,
            Self::Accepted(_) => StreamTag::Accepted,
            Self::Rejected(_) => StreamTag::Rejected,
            Self::Chunk(_) => StreamTag::Chunk,
            Self::Progress(_) => StreamTag::Progress,
            Self::End(_) => StreamTag::End,
            Self::Cancel(_) => StreamTag::Cancel,
            Self::Error(_) => StreamTag::Error,
        }
    }
}

impl<C> Encode<C> for StreamPayload {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(2)?;
        e.u16(self.tag().as_u16())?;
        match self {
            Self::Open(v) => v.encode(e, ctx)?,
            Self::Accepted(v) => v.encode(e, ctx)?,
            Self::Rejected(v) => v.encode(e, ctx)?,
            Self::Chunk(v) => v.encode(e, ctx)?,
            Self::Progress(v) => v.encode(e, ctx)?,
            Self::End(v) => v.encode(e, ctx)?,
            Self::Cancel(v) => v.encode(e, ctx)?,
            Self::Error(v) => v.encode(e, ctx)?,
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for StreamPayload {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let n = d
            .array()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length array forbidden"))?;
        if n != 2 {
            return Err(minicbor::decode::Error::message(
                "Envelope payload tuple MUST be [tag, body]",
            ));
        }
        let tag_raw = d.u16()?;
        let tag = StreamTag::from_u16(tag_raw)
            .ok_or_else(|| minicbor::decode::Error::message("unknown stream-channel tag"))?;
        Ok(match tag {
            StreamTag::Open => Self::Open(StreamOpen::decode(d, ctx)?),
            StreamTag::Accepted => Self::Accepted(StreamAccepted::decode(d, ctx)?),
            StreamTag::Rejected => Self::Rejected(StreamRejected::decode(d, ctx)?),
            StreamTag::Chunk => Self::Chunk(StreamChunk::decode(d, ctx)?),
            StreamTag::Progress => Self::Progress(StreamProgress::decode(d, ctx)?),
            StreamTag::End => Self::End(StreamEnd::decode(d, ctx)?),
            StreamTag::Cancel => Self::Cancel(StreamCancel::decode(d, ctx)?),
            StreamTag::Error => Self::Error(StreamError::decode(d, ctx)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::envelope::{Channel, Envelope, Flags, Priority, ProtocolVersion};
    use crate::protocol::ids::SessionId;
    use crate::protocol::value::Value;

    fn envelope_with(payload: StreamPayload) -> Envelope<StreamPayload> {
        Envelope {
            protocol_version: ProtocolVersion::V1,
            channel: Channel::Stream,
            msg_id: 1,
            correlation_id: None,
            ts_ms: 1_700_000_000_000,
            session_id: SessionId::from_bytes([0; 16]),
            trace_id: None,
            deadline_ms: None,
            priority: Priority::Normal,
            flags: Flags::RELIABLE,
            payload,
        }
    }

    fn rt(env: &Envelope<StreamPayload>) {
        let mut b1 = Vec::new();
        minicbor::encode(env, &mut b1).unwrap();
        let d: Envelope<StreamPayload> = minicbor::decode(&b1).unwrap();
        assert_eq!(&d, env);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    #[test]
    fn envelope_all_stream_variants_roundtrip() {
        rt(&envelope_with(StreamPayload::Open(StreamOpen {
            stream_id: 1,
            kind: StreamKind::LlmTokenStream,
            metadata: CborMap(vec![("model".into(), Value::Text("opus-4.7".into()))]),
            expected_total_bytes: None,
            reliable: true,
        })));
        rt(&envelope_with(StreamPayload::Accepted(StreamAccepted {
            stream_id: 1,
        })));
        rt(&envelope_with(StreamPayload::Rejected(StreamRejected {
            stream_id: 1,
            code: ErrorCode::RateLimited,
            message: "quota exceeded".into(),
        })));
        rt(&envelope_with(StreamPayload::Chunk(StreamChunk {
            stream_id: 1,
            sequence: 0,
            data: b"hello".to_vec(),
            end_of_stream: false,
        })));
        rt(&envelope_with(StreamPayload::Progress(StreamProgress {
            stream_id: 1,
            bytes_processed: Some(1024),
            items_processed: None,
            percent: Some(33),
            message: Some("indexing".into()),
        })));
        rt(&envelope_with(StreamPayload::End(StreamEnd {
            stream_id: 1,
        })));
        rt(&envelope_with(StreamPayload::Cancel(StreamCancel {
            stream_id: 1,
            reason: "user_aborted".into(),
        })));
        rt(&envelope_with(StreamPayload::Error(StreamError {
            stream_id: 1,
            code: ErrorCode::FuelExhausted,
            message: "wasm fuel ran out".into(),
        })));
    }

    #[test]
    fn unknown_stream_tag_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap().u16(0x03FE).unwrap().map(0).unwrap();
        let res: Result<StreamPayload, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn stream_kind_unknown_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.str("future_kind").unwrap();
        let res: Result<StreamKind, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn stream_open_chunk_terminal_flag() {
        // end_of_stream=true on a chunk acts as inline StreamEnd.
        rt(&envelope_with(StreamPayload::Chunk(StreamChunk {
            stream_id: 7,
            sequence: 42,
            data: b"final".to_vec(),
            end_of_stream: true,
        })));
    }
}
