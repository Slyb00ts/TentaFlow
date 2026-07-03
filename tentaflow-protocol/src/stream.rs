// =============================================================================
// File: stream.rs
// Purpose: Binary stream pub/sub protocol — packed into a single
//          `MessageBody::StreamBody(StreamPayload)` slot so the whole live
//          streaming surface burns one discriminant of the CBOR 0.8 256-variant
//          budget (same pattern as `CameraAdminPayload` / `LegalAdminPayload`).
//
//          Wire shape:
//            * client -> server: `SubscribeRequest { stream_id }` on a fresh
//              correlation_id; optional `CloseRequest { stream_id }` reusing
//              the same correlation_id to detach early.
//            * server -> client: `SubscribeResponse` (one), then a series of
//              `Frame` payloads (init segment first when present, then media
//              chunks), terminated by a single `Closed` carrying a static
//              reason string. The server-side streaming task wraps each chunk
//              in an envelope with `IS_STREAM_CHUNK` / `IS_STREAM_END`.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Inner payload for `MessageBody::StreamBody`. See module docs for the
/// request/response ordering contract.
#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum StreamPayload {
    /// Client -> server. Asks the hub to subscribe this WS connection to the
    /// named stream. The server first answers with `SubscribeResponse`
    /// (mime + has_init_segment hint), then pushes a sequence of `Frame`
    /// chunks on the same correlation id.
    SubscribeRequest(StreamSubscribeRequest),
    /// Server -> client. Single message confirming subscription metadata
    /// (MIME for `MediaSource.addSourceBuffer`, init-segment availability).
    SubscribeResponse(StreamSubscribeResponse),
    /// Server -> client. Binary chunk. The first chunk after
    /// `SubscribeResponse` carries `is_init = true` when the hub has a cached
    /// init segment (ftyp+moov for fMP4); subsequent chunks carry media
    /// segments (moof+mdat) with `is_init = false`.
    Frame(StreamFramePayload),
    /// Client -> server. Releases the subscription early (e.g. UI tile
    /// navigates away). Matched against the WS connection's active
    /// subscription by `stream_id`.
    CloseRequest(StreamCloseRequest),
    /// Server -> client. Terminal frame for the subscription. `reason` is one
    /// of the static strings `subscriber_lagged`, `source_unregistered`,
    /// `client_request`, `internal_error` — callers match on the string.
    Closed(StreamClosedPayload),
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct StreamSubscribeRequest {
    /// Hub-registered stream id, e.g. `camera:<uuid>` for the camera tier.
    pub stream_id: String,
    /// `true` = wariant podglądu (transkod 720p/~1,5 Mbit/s) zamiast pełnej
    /// jakości źródła — kafelki Live view są małe, więc pełny strumień 1080p
    /// marnuje pasmo WAN i głodzi WebSocket detekcji na tym samym łączu.
    /// `serde(default)` = `false` (pełna jakość), kompatybilne wstecz ze
    /// starszymi klientami, które pola nie wysyłają. Wariant dotyczy tylko
    /// strumieni `camera:`; pozostałe prefiksy go ignorują.
    #[serde(default)]
    pub preview: bool,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct StreamSubscribeResponse {
    pub stream_id: String,
    /// MIME type ready to feed into `MediaSource.addSourceBuffer`.
    pub mime_type: String,
    /// `true` when the next `Frame` will carry `is_init = true`.
    pub has_init_segment: bool,
    /// Base PTS of the media timeline (nanoseconds) for fMP4 camera streams —
    /// the offset the client subtracts from each detection's `pts_ns` to anchor
    /// the overlay on the exact video frame. `None` for streams with no shared
    /// clock with detections (LiDAR, audio, relay).
    #[serde(default)]
    pub base_pts_ns: Option<u64>,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct StreamFramePayload {
    pub stream_id: String,
    /// `true` for the MSE init segment (ftyp+moov), `false` for media chunks.
    pub is_init: bool,
    /// `serde_bytes` so ciborium emits a CBOR byte string (one bulk copy) instead of
    /// a CBOR array-of-integers. Plain `Vec<u8>` via serde encodes each byte as a
    /// separate CBOR item (~100ns/byte en+decode) — for a ~300KB LiDAR frame / fMP4
    /// chunk that was the dominant push-path latency. Byte string = length-prefixed
    /// bulk on both server encode and wasm decode.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct StreamCloseRequest {
    pub stream_id: String,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct StreamClosedPayload {
    pub stream_id: String,
    /// Static reason tag. One of `subscriber_lagged`, `source_unregistered`,
    /// `client_request`, `internal_error`.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = crate::cbor::encode(&$value).expect("encode");
            crate::cbor::decode::<$ty>(&bytes).expect("decode")
        }};
    }

    #[test]
    fn subscribe_request_round_trip() {
        let v = StreamPayload::SubscribeRequest(StreamSubscribeRequest {
            stream_id: "camera:550e8400-e29b-41d4-a716-446655440000".into(),
            preview: true,
        });
        assert_eq!(round_trip!(StreamPayload, v.clone()), v);
    }

    #[test]
    fn subscribe_response_round_trip() {
        let v = StreamPayload::SubscribeResponse(StreamSubscribeResponse {
            stream_id: "camera:xyz".into(),
            mime_type: "video/mp4; codecs=\"avc1.64001f\"".into(),
            has_init_segment: true,
            base_pts_ns: Some(1_234_567_890),
        });
        assert_eq!(round_trip!(StreamPayload, v.clone()), v);
    }

    #[test]
    fn frame_payload_round_trip() {
        let v = StreamPayload::Frame(StreamFramePayload {
            stream_id: "camera:xyz".into(),
            is_init: false,
            data: vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        });
        assert_eq!(round_trip!(StreamPayload, v.clone()), v);
    }

    #[test]
    fn close_request_round_trip() {
        let v = StreamPayload::CloseRequest(StreamCloseRequest {
            stream_id: "camera:xyz".into(),
        });
        assert_eq!(round_trip!(StreamPayload, v.clone()), v);
    }

    #[test]
    fn closed_payload_round_trip() {
        let v = StreamPayload::Closed(StreamClosedPayload {
            stream_id: "camera:xyz".into(),
            reason: "subscriber_lagged".into(),
        });
        assert_eq!(round_trip!(StreamPayload, v.clone()), v);
    }

    #[test]
    fn message_body_stream_subscribe_round_trip() {
        let body =
            MessageBody::StreamBody(StreamPayload::SubscribeRequest(StreamSubscribeRequest {
                stream_id: "camera:abc".into(),
                preview: false,
            }));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_stream_frame_round_trip() {
        let body = MessageBody::StreamBody(StreamPayload::Frame(StreamFramePayload {
            stream_id: "camera:abc".into(),
            is_init: true,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
