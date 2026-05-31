// =============================================================================
// File: protocol/streaming.rs — streaming host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// three `stream_*_v1` host functions (`stream_subscribe`, `stream_next`,
// `stream_close`). Shared verbatim by the core host (decode input / encode
// output) and the addon SDK (encode input / decode output) so the wire format
// cannot drift between the two. Maps use integer keys (compact canonical form)
// via `#[cbor(map)]` + `#[n(N)]`.
//
// `stream_next` returns one of five variants discriminated by the `kind` tag;
// it is encoded as a single map with `kind` plus the fields that variant uses,
// rather than a CBOR-tagged enum, so the host can build any variant without a
// borrow gymnastics and the SDK can match on `kind` after a single decode.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `stream_subscribe_v1`. `target` is `camera:<camera_id>` for F1a;
/// `filter` is optional — when absent the host applies the default
/// `StreamFilter` (no fps cap, no skipping).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamSubscribeInput {
    #[n(0)]
    pub target: String,
    #[n(1)]
    pub filter: Option<StreamSubscribeFilter>,
}

/// Optional subscribe filter. `max_fps = None` means no cap; `skip_frames`
/// defaults to `0` on the wire when omitted (resolved via
/// [`StreamSubscribeFilter::skip_frames_or_default`]).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamSubscribeFilter {
    #[n(0)]
    pub max_fps: Option<u32>,
    #[n(1)]
    pub skip_frames: Option<u32>,
}

/// Legacy default for `skip_frames` when the filter omits it.
pub const STREAM_SUBSCRIBE_DEFAULT_SKIP_FRAMES: u32 = 0;

impl StreamSubscribeFilter {
    /// `skip_frames` with the legacy default applied when absent.
    pub fn skip_frames_or_default(&self) -> u32 {
        self.skip_frames
            .unwrap_or(STREAM_SUBSCRIBE_DEFAULT_SKIP_FRAMES)
    }
}

/// Input for `stream_next_v1`. `timeout_ms` is clamped to the host ceiling
/// (5000 ms) after decode.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamNextInput {
    #[n(0)]
    pub stream_id: String,
    #[n(1)]
    pub timeout_ms: u64,
}

/// Input for `stream_close_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamCloseInput {
    #[n(0)]
    pub stream_id: String,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// Output of `stream_subscribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamSubscribeOutput {
    #[n(0)]
    pub stream_id: String,
}

/// Discriminator for the `stream_next_v1` output variant.
pub const STREAM_NEXT_KIND_FRAME: &str = "frame";
pub const STREAM_NEXT_KIND_DROP: &str = "drop";
pub const STREAM_NEXT_KIND_CAMERA_OFFLINE: &str = "camera_offline";
pub const STREAM_NEXT_KIND_TIMEOUT: &str = "timeout";
pub const STREAM_NEXT_KIND_STREAM_CLOSED: &str = "stream_closed";

/// Output of `stream_next_v1`. `kind` selects which set of fields is populated:
///   - `frame` — `frame_ref` + frame metadata (`camera_id` .. `timestamp_unix_ms`),
///   - `drop` — `count` (frames dropped by the bus backpressure),
///   - `camera_offline` — `reason`,
///   - `timeout` — no extra fields,
///   - `stream_closed` — no extra fields.
///
/// Frame bytes are never inlined here — only `frame_ref` + metadata travel to
/// the addon; the actual buffer moves through a service via `service_call_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamNextOutput {
    #[n(0)]
    pub kind: String,
    #[n(1)]
    pub frame_ref: Option<String>,
    #[n(2)]
    pub camera_id: Option<String>,
    #[n(3)]
    pub width: Option<u32>,
    #[n(4)]
    pub height: Option<u32>,
    #[n(5)]
    pub pixel_format: Option<String>,
    #[n(6)]
    pub timestamp_unix_ms: Option<u64>,
    #[n(7)]
    pub count: Option<u64>,
    #[n(8)]
    pub reason: Option<String>,
}

impl StreamNextOutput {
    /// Builds a `frame` variant.
    pub fn frame(
        frame_ref: String,
        camera_id: String,
        width: u32,
        height: u32,
        pixel_format: String,
        timestamp_unix_ms: u64,
    ) -> Self {
        Self {
            kind: STREAM_NEXT_KIND_FRAME.to_string(),
            frame_ref: Some(frame_ref),
            camera_id: Some(camera_id),
            width: Some(width),
            height: Some(height),
            pixel_format: Some(pixel_format),
            timestamp_unix_ms: Some(timestamp_unix_ms),
            count: None,
            reason: None,
        }
    }

    /// Builds a `drop` variant.
    pub fn drop(count: u64) -> Self {
        Self {
            kind: STREAM_NEXT_KIND_DROP.to_string(),
            frame_ref: None,
            camera_id: None,
            width: None,
            height: None,
            pixel_format: None,
            timestamp_unix_ms: None,
            count: Some(count),
            reason: None,
        }
    }

    /// Builds a `camera_offline` variant.
    pub fn camera_offline(reason: String) -> Self {
        Self {
            kind: STREAM_NEXT_KIND_CAMERA_OFFLINE.to_string(),
            frame_ref: None,
            camera_id: None,
            width: None,
            height: None,
            pixel_format: None,
            timestamp_unix_ms: None,
            count: None,
            reason: Some(reason),
        }
    }

    /// Builds a `timeout` variant.
    pub fn timeout() -> Self {
        Self::no_payload(STREAM_NEXT_KIND_TIMEOUT)
    }

    /// Builds a `stream_closed` variant.
    pub fn stream_closed() -> Self {
        Self::no_payload(STREAM_NEXT_KIND_STREAM_CLOSED)
    }

    fn no_payload(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            frame_ref: None,
            camera_id: None,
            width: None,
            height: None,
            pixel_format: None,
            timestamp_unix_ms: None,
            count: None,
            reason: None,
        }
    }
}

/// Output of `stream_close_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StreamCloseOutput {
    #[n(0)]
    pub closed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_subscribe_input_with_filter() {
        roundtrip(&StreamSubscribeInput {
            target: "camera:cam_00000000-0000-0000-0000-000000000000".into(),
            filter: Some(StreamSubscribeFilter {
                max_fps: Some(15),
                skip_frames: Some(2),
            }),
        });
    }

    #[test]
    fn roundtrip_subscribe_input_minimal() {
        roundtrip(&StreamSubscribeInput {
            target: "camera:cam_00000000-0000-0000-0000-000000000000".into(),
            filter: None,
        });
    }

    #[test]
    fn omitted_skip_frames_resolves_to_default() {
        let f = StreamSubscribeFilter {
            max_fps: Some(10),
            skip_frames: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&f, &mut buf).unwrap();
        let decoded: StreamSubscribeFilter = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.skip_frames_or_default(), 0);
    }

    #[test]
    fn roundtrip_next_input() {
        roundtrip(&StreamNextInput {
            stream_id: "stream_00000000-0000-0000-0000-000000000000".into(),
            timeout_ms: 1000,
        });
    }

    #[test]
    fn roundtrip_next_output_frame() {
        roundtrip(&StreamNextOutput::frame(
            "frame_ref_1".into(),
            "cam_00000000-0000-0000-0000-000000000000".into(),
            1920,
            1080,
            "rgb24".into(),
            1_700_000_000_000,
        ));
    }

    #[test]
    fn roundtrip_next_output_drop_offline_timeout_closed() {
        roundtrip(&StreamNextOutput::drop(7));
        roundtrip(&StreamNextOutput::camera_offline("supervisor_exit".into()));
        roundtrip(&StreamNextOutput::timeout());
        roundtrip(&StreamNextOutput::stream_closed());
    }

    #[test]
    fn roundtrip_close_input_and_output() {
        roundtrip(&StreamCloseInput {
            stream_id: "stream_00000000-0000-0000-0000-000000000000".into(),
        });
        roundtrip(&StreamCloseOutput { closed: true });
    }
}
