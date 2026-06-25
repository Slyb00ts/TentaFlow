// =============================================================================
// File: protocol/webrtc.rs — generic webrtc.* host-function ABI payloads
// Vendor-agnostic WebRTC channel the host exposes to addons. The addon drives
// signaling (offer out / answer in) and the data channel (send / drain); the
// host owns the native peer. Binary payloads ride as base64 strings (same
// convention as camera/recording `data_b64`).
// =============================================================================

use minicbor::{Decode, Encode};

/// Input for `webrtc_connect_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcConnectInput {
    #[n(0)]
    pub data_channel_label: String,
    #[n(1)]
    pub want_video: bool,
    #[n(2)]
    pub disable_mdns: bool,
    #[n(3)]
    pub gather_timeout_ms: u64,
    #[n(4)]
    pub inbound_capacity: u32,
    /// Optional app-level keepalive for precise RTT. Text pinged every
    /// `keepalive_interval_ms` (0 = disabled); `keepalive_marker` identifies the
    /// peer's reply. Supplied by the addon (vendor-specific heartbeat).
    #[n(5)]
    pub keepalive_text: Option<String>,
    #[n(6)]
    pub keepalive_interval_ms: u64,
    #[n(7)]
    pub keepalive_marker: Option<String>,
    /// Target peer's IPv4 (the robot's LAN address). The host uses it to narrow
    /// ICE candidate gathering to the local interface on the SAME subnet as the
    /// peer, so a multi-homed host does not advertise unreachable candidates.
    #[n(8)]
    pub peer_ipv4: Option<String>,
}

/// Output of `webrtc_connect_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcConnectOutput {
    #[n(0)]
    pub channel_id: String,
    #[n(1)]
    pub offer_sdp: String,
}

/// Input for `webrtc_set_answer_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcSetAnswerInput {
    #[n(0)]
    pub channel_id: String,
    #[n(1)]
    pub answer_sdp: String,
}

/// Shared status output for set_answer / send / close.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcStatusOutput {
    #[n(0)]
    pub ok: bool,
}

/// Input for `webrtc_state_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcStateInput {
    #[n(0)]
    pub channel_id: String,
}

/// Output of `webrtc_state_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcStateOutput {
    #[n(0)]
    pub peer_state: String,
    #[n(1)]
    pub dc_open: bool,
    #[n(2)]
    pub dropped_count: u64,
    #[n(3)]
    pub queue_len: u32,
    /// Transport round-trip latency in ms (nominated ICE pair); None until measured.
    #[n(4)]
    pub rtt_ms: Option<f64>,
}

/// Input for `webrtc_send_v1`. `data_b64` is base64 of the raw payload bytes
/// (for text messages, the UTF-8 bytes).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcSendInput {
    #[n(0)]
    pub channel_id: String,
    #[n(1)]
    pub is_text: bool,
    #[n(2)]
    pub data_b64: String,
}

/// One inbound data-channel message.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcMessage {
    #[n(0)]
    pub is_text: bool,
    #[n(1)]
    pub data_b64: String,
}

/// Input for `webrtc_drain_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcDrainInput {
    #[n(0)]
    pub channel_id: String,
    #[n(1)]
    pub max_messages: u32,
}

/// Output of `webrtc_drain_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcDrainOutput {
    #[n(0)]
    pub messages: Vec<WebRtcMessage>,
    #[n(1)]
    pub dropped_count: u64,
    #[n(2)]
    pub queue_len: u32,
    #[n(3)]
    pub closed: bool,
}

/// Borrowed encode-only view of `WebRtcDrainOutput`. Field indices MUST stay
/// identical so the addon decodes `WebRtcDrainOutput` from the bytes this emits.
/// Lets the host encode a drain batch from a borrow of the staging buffer
/// instead of deep-cloning every staged message on the happy path.
#[derive(Debug, Encode)]
#[cbor(map)]
pub struct WebRtcDrainOutputRef<'a> {
    #[n(0)]
    pub messages: &'a [WebRtcMessage],
    #[n(1)]
    pub dropped_count: u64,
    #[n(2)]
    pub queue_len: u32,
    #[n(3)]
    pub closed: bool,
}

/// Input for `webrtc_close_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcCloseInput {
    #[n(0)]
    pub channel_id: String,
}

/// Input for `webrtc_register_camera_v1` — bind a channel's video track to a
/// camera consumable by the normal camera/streaming stack.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcRegisterCameraInput {
    #[n(0)]
    pub channel_id: String,
    #[n(1)]
    pub display_name: String,
    #[n(2)]
    pub target_fps: u32,
    #[n(3)]
    pub analysis_fps: u32,
    /// Camera horizontal field of view (deg). The robot addon knows its own lens, so
    /// it supplies the intrinsics instead of core guessing. `None` ⇒ core default.
    #[n(4)]
    pub camera_fov_deg: Option<f32>,
    /// Camera vertical field of view (deg). Differs sharply from horizontal because the
    /// depth model runs on a square frame stretched from a wide 16:9 stream. `None` ⇒
    /// square pixels (`fy = fx`).
    #[n(5)]
    pub camera_fov_v_deg: Option<f32>,
    /// Metric scale correction for the depth model on this camera (the monocular
    /// depth model has a systematic scale bias; the addon knows the model+camera
    /// pair). `None` ⇒ core default (1.0 = no correction).
    #[n(6)]
    pub camera_depth_scale: Option<f32>,
}

/// Output of `webrtc_register_camera_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct WebRtcRegisterCameraOutput {
    #[n(0)]
    pub camera_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_roundtrip() {
        let v = WebRtcConnectInput {
            data_channel_label: "data".into(),
            want_video: false,
            disable_mdns: true,
            gather_timeout_ms: 8000,
            inbound_capacity: 2048,
            keepalive_text: Some("ping".into()),
            keepalive_interval_ms: 1000,
            keepalive_marker: Some("pong".into()),
            peer_ipv4: Some("192.168.0.188".into()),
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: WebRtcConnectInput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn drain_roundtrip() {
        let v = WebRtcDrainOutput {
            messages: vec![WebRtcMessage { is_text: true, data_b64: "aGk=".into() }],
            dropped_count: 3,
            queue_len: 5,
            closed: false,
        };
        let mut buf = Vec::new();
        minicbor::encode(&v, &mut buf).unwrap();
        let back: WebRtcDrainOutput = minicbor::decode(&buf).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn drain_ref_encodes_same_bytes_as_owned() {
        let owned = WebRtcDrainOutput {
            messages: vec![
                WebRtcMessage { is_text: true, data_b64: "aGk=".into() },
                WebRtcMessage { is_text: false, data_b64: "AAEC".into() },
            ],
            dropped_count: 7,
            queue_len: 11,
            closed: true,
        };
        let view = WebRtcDrainOutputRef {
            messages: &owned.messages,
            dropped_count: owned.dropped_count,
            queue_len: owned.queue_len,
            closed: owned.closed,
        };
        let mut a = Vec::new();
        let mut b = Vec::new();
        minicbor::encode(&owned, &mut a).unwrap();
        minicbor::encode(&view, &mut b).unwrap();
        assert_eq!(a, b);
        let back: WebRtcDrainOutput = minicbor::decode(&b).unwrap();
        assert_eq!(owned, back);
    }
}
