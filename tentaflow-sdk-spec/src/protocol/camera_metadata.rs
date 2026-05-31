// =============================================================================
// File: protocol/camera_metadata.rs — ONVIF analytics metadata host-function
// ABI payloads. Single source of truth for the CBOR request/response structs of
// the three `camera_metadata_*_v1` host functions. Shared verbatim by the core
// host (decode input / encode output) and the addon SDK (encode input / decode
// output) so the wire format cannot drift. Maps use integer keys via
// `#[cbor(map)]` + `#[n(N)]`. `max_items` / `timeout_ms` are `Option` on the
// wire so a minimal poll payload can omit them; the host resolves the legacy
// TOML defaults (`10` / `5000`) right after decode via the `*_or_default`
// accessors.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// subscribe
// -----------------------------------------------------------------------------

/// Input for `camera_metadata_subscribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataSubscribeInput {
    #[n(0)]
    pub camera_id: String,
}

/// Output of `camera_metadata_subscribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataSubscribeOutput {
    #[n(0)]
    pub subscription_id: String,
    #[n(1)]
    pub status: String,
}

// -----------------------------------------------------------------------------
// unsubscribe
// -----------------------------------------------------------------------------

/// Input for `camera_metadata_unsubscribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataUnsubscribeInput {
    #[n(0)]
    pub subscription_id: String,
}

/// Output of `camera_metadata_unsubscribe_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataUnsubscribeOutput {
    #[n(0)]
    pub unsubscribed: bool,
}

// -----------------------------------------------------------------------------
// poll
// -----------------------------------------------------------------------------

/// Legacy TOML default for `max_items` when the payload omits it.
pub const METADATA_POLL_DEFAULT_MAX_ITEMS: u32 = 10;
/// Legacy TOML default for `timeout_ms` when the payload omits it.
pub const METADATA_POLL_DEFAULT_TIMEOUT_MS: u32 = 5_000;

/// Input for `camera_metadata_poll_v1`. `max_items` / `timeout_ms` default to
/// the legacy TOML values when absent.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataPollInput {
    #[n(0)]
    pub subscription_id: String,
    #[n(1)]
    pub max_items: Option<u32>,
    #[n(2)]
    pub timeout_ms: Option<u32>,
}

impl MetadataPollInput {
    /// `max_items` with the legacy default applied when absent.
    pub fn max_items_or_default(&self) -> u32 {
        self.max_items.unwrap_or(METADATA_POLL_DEFAULT_MAX_ITEMS)
    }

    /// `timeout_ms` with the legacy default applied when absent.
    pub fn timeout_ms_or_default(&self) -> u32 {
        self.timeout_ms.unwrap_or(METADATA_POLL_DEFAULT_TIMEOUT_MS)
    }
}

/// One analytics object inside a `MetadataFrameOut`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataItemOut {
    #[n(0)]
    pub class: String,
    #[n(1)]
    pub confidence: f64,
    /// `[left, top, right, bottom]` in normalised 0..1 device coords.
    #[n(2)]
    pub bbox: Option<[f64; 4]>,
    #[n(3)]
    pub track_id: Option<String>,
}

/// One metadata frame returned by `camera_metadata_poll_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataFrameOut {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub ts_unix_ms: i64,
    #[n(2)]
    pub items: Vec<MetadataItemOut>,
}

/// Output of `camera_metadata_poll_v1`. `camera_offline` is set when the bus
/// signalled `CameraOffline` mid-poll; `dropped` accumulates backpressure drops.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct MetadataPollOutput {
    #[n(0)]
    pub frames: Vec<MetadataFrameOut>,
    #[n(1)]
    pub camera_offline: bool,
    #[n(2)]
    pub dropped: u64,
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
    fn roundtrip_subscribe() {
        roundtrip(&MetadataSubscribeInput {
            camera_id: "cam_1".into(),
        });
        roundtrip(&MetadataSubscribeOutput {
            subscription_id: "stream_1".into(),
            status: "subscribed".into(),
        });
    }

    #[test]
    fn poll_input_defaults_resolve_when_absent() {
        let minimal = MetadataPollInput {
            subscription_id: "stream_1".into(),
            max_items: None,
            timeout_ms: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&minimal, &mut buf).unwrap();
        let decoded: MetadataPollInput = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.max_items_or_default(), 10);
        assert_eq!(decoded.timeout_ms_or_default(), 5_000);
    }

    #[test]
    fn roundtrip_poll_output() {
        roundtrip(&MetadataPollOutput {
            frames: vec![MetadataFrameOut {
                camera_id: "cam_1".into(),
                ts_unix_ms: 1_700_000_000_000,
                items: vec![MetadataItemOut {
                    class: "person".into(),
                    confidence: 0.93,
                    bbox: Some([0.1, 0.2, 0.3, 0.4]),
                    track_id: Some("t7".into()),
                }],
            }],
            camera_offline: false,
            dropped: 0,
        });
    }
}
