// =============================================================================
// File: protocol/recording.rs — recording + frame_url host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// seven recording host functions (`recording_save_snapshot`,
// `recording_save_segment`, `recording_get_url`, `recording_get_stream`,
// `recording_purge`, `recording_stats`, `frame_url`). Shared verbatim by the
// core host (decode input / encode output) and the addon SDK (encode input /
// decode output) so the wire format cannot drift between the two. Maps use
// integer keys (compact canonical form) via `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `recording_save_snapshot_v1`. `retention_class` is optional on the
/// wire — when absent the host falls back to the owning camera row's class.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingSaveSnapshotInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub frame_ref: String,
    #[n(2)]
    pub retention_class: Option<String>,
}

/// Input for `recording_save_segment_v1`. `retention_class` is optional on the
/// wire — when absent the host falls back to the owning camera row's class.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingSaveSegmentInput {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub duration_secs: u32,
    #[n(2)]
    pub retention_class: Option<String>,
}

/// Input carrying a single `recording_ref` — shared by `get_stream` / `purge`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingRefInput {
    #[n(0)]
    pub recording_ref: String,
}

/// Input for `recording_get_url_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingGetUrlInput {
    #[n(0)]
    pub recording_ref: String,
    #[n(1)]
    pub ttl_secs: u64,
}

/// Input for `recording_stats_v1`. `camera_id` is optional — absent means "no
/// filter" (aggregate across all of the addon's cameras).
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingStatsInput {
    #[n(0)]
    pub camera_id: Option<String>,
}

/// Input for `recording_list_v1`. `camera_id` is optional (absent = all of the
/// addon's cameras); `limit` caps the returned rows (host clamps to a sane max).
/// Only `kind = "segment"` per-vehicle event clips are returned — snapshots are
/// intentionally excluded from the browsable recordings list. The optional
/// server-side search filters compose with AND: `date_from`/`date_to` are unix
/// MILLISECONDS bounding `created_at`; `plate`/`adr` are case-insensitive
/// substring matches over the event's gated OCR winners. All new fields carry
/// high CBOR keys so an older client that omits them still decodes.
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingListInput {
    #[n(0)]
    pub camera_id: Option<String>,
    #[n(1)]
    pub limit: u32,
    #[n(2)]
    pub date_from: Option<i64>,
    #[n(3)]
    pub date_to: Option<i64>,
    #[n(4)]
    pub plate: Option<String>,
    #[n(5)]
    pub adr: Option<String>,
}

/// Input for `frame_url_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FrameUrlInput {
    #[n(0)]
    pub frame_ref: String,
    #[n(1)]
    pub ttl_secs: u64,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// Output of `recording_save_snapshot_v1` / `recording_save_segment_v1`. For
/// snapshots `duration_ms` is `None`; for segments `width`/`height` are `None`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SaveRecordingOut {
    #[n(0)]
    pub recording_ref: String,
    #[n(1)]
    pub file_path: String,
    #[n(2)]
    pub file_size_bytes: u64,
    #[n(3)]
    pub duration_ms: Option<u32>,
    #[n(4)]
    pub width: Option<u32>,
    #[n(5)]
    pub height: Option<u32>,
    #[n(6)]
    pub hash_sha256: String,
    #[n(7)]
    pub created_at: u64,
}

/// Output of `recording_get_url_v1` / `frame_url_v1`. Multi-use signed URL until
/// `expires_unix_ms`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct UrlOut {
    #[n(0)]
    pub url: String,
    #[n(1)]
    pub expires_unix_ms: u64,
}

/// Output of `recording_get_stream_v1`. `data_b64` is the base64-encoded raw
/// recording payload (PNG or MP4 bytes).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GetStreamOut {
    #[n(0)]
    pub data_b64: String,
    #[n(1)]
    pub file_size_bytes: u64,
    #[n(2)]
    pub hash_sha256: String,
}

/// Output of `recording_purge_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PurgeOut {
    #[n(0)]
    pub purged: bool,
}

/// Per-camera breakdown element of `recording_stats_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StatsPerCamera {
    #[n(0)]
    pub camera_id: String,
    #[n(1)]
    pub snapshots: u64,
    #[n(2)]
    pub segments: u64,
    #[n(3)]
    pub size_bytes: u64,
}

/// Aggregate totals nested inside `StatsOut`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StatsTotals {
    #[n(0)]
    pub total_snapshots: u64,
    #[n(1)]
    pub total_segments: u64,
    #[n(2)]
    pub total_size_bytes: u64,
}

/// Output of `recording_stats_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct StatsOut {
    #[n(0)]
    pub stats: StatsTotals,
    #[n(1)]
    pub per_camera: Vec<StatsPerCamera>,
}

/// One row of `recording_list_v1`: the catalog fields a recordings browser
/// needs to render a list and open a signed playback URL. `event_meta` is the
/// raw JSON summary string written by the per-vehicle event recorder (plate/ADR
/// OCR votes) — the addon parses it client-side; it is `None` for artifacts with
/// no summary.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingListItem {
    #[n(0)]
    pub recording_ref: String,
    #[n(1)]
    pub camera_id: String,
    #[n(2)]
    pub created_at: i64,
    #[n(3)]
    pub duration_ms: Option<i64>,
    #[n(4)]
    pub file_size_bytes: i64,
    #[n(5)]
    pub event_meta: Option<String>,
    /// Gated plate OCR winner for the event (NULL when unreadable). Also the
    /// server-side `plate` search target.
    #[n(6)]
    pub plate_text: Option<String>,
    /// Gated ADR OCR winner for the event (NULL when unreadable).
    #[n(7)]
    pub adr_text: Option<String>,
    /// Snapshot ref of the full downscaled frame at the event's best plate read
    /// (whole scene, not a crop). Resolve to an image URL via `frame_url_v1`.
    #[n(8)]
    pub plate_thumb_ref: Option<String>,
    /// Snapshot ref of the full downscaled frame at the event's best ADR read.
    #[n(9)]
    pub adr_thumb_ref: Option<String>,
}

/// Output of `recording_list_v1`.
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct RecordingListOut {
    #[n(0)]
    pub items: Vec<RecordingListItem>,
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
    fn roundtrip_save_snapshot_input() {
        roundtrip(&RecordingSaveSnapshotInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            frame_ref: "frame_00000000-0000-0000-0000-000000000000".into(),
            retention_class: Some("A".into()),
        });
        roundtrip(&RecordingSaveSnapshotInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            frame_ref: "frame_00000000-0000-0000-0000-000000000000".into(),
            retention_class: None,
        });
    }

    #[test]
    fn roundtrip_save_segment_input() {
        roundtrip(&RecordingSaveSegmentInput {
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            duration_secs: 5,
            retention_class: None,
        });
    }

    #[test]
    fn roundtrip_ref_and_url_inputs() {
        roundtrip(&RecordingRefInput {
            recording_ref: "snap_00000000-0000-0000-0000-000000000000".into(),
        });
        roundtrip(&RecordingGetUrlInput {
            recording_ref: "clip_00000000-0000-0000-0000-000000000000".into(),
            ttl_secs: 300,
        });
        roundtrip(&FrameUrlInput {
            frame_ref: "frame_00000000-0000-0000-0000-000000000000".into(),
            ttl_secs: 120,
        });
    }

    #[test]
    fn roundtrip_stats_input_default() {
        roundtrip(&RecordingStatsInput::default());
        roundtrip(&RecordingStatsInput {
            camera_id: Some("cam_00000000-0000-0000-0000-000000000000".into()),
        });
    }

    #[test]
    fn roundtrip_save_recording_out() {
        roundtrip(&SaveRecordingOut {
            recording_ref: "snap_00000000-0000-0000-0000-000000000000".into(),
            file_path: "/tmp/snap.png".into(),
            file_size_bytes: 1024,
            duration_ms: None,
            width: Some(640),
            height: Some(480),
            hash_sha256: "ab".repeat(32),
            created_at: 1_700_000_000_000,
        });
    }

    #[test]
    fn roundtrip_url_stream_purge_outs() {
        roundtrip(&UrlOut {
            url: "/recordings/snap_x?token=y".into(),
            expires_unix_ms: 1_700_000_000_000,
        });
        roundtrip(&GetStreamOut {
            data_b64: "AAAA".into(),
            file_size_bytes: 3,
            hash_sha256: "cd".repeat(32),
        });
        roundtrip(&PurgeOut { purged: true });
    }

    #[test]
    fn roundtrip_recording_list() {
        roundtrip(&RecordingListInput {
            camera_id: Some("cam_00000000-0000-0000-0000-000000000000".into()),
            limit: 200,
            date_from: Some(1_700_000_000_000),
            date_to: Some(1_700_500_000_000),
            plate: Some("WGM".into()),
            adr: Some("30/1202".into()),
        });
        roundtrip(&RecordingListInput::default());
        roundtrip(&RecordingListOut {
            items: vec![RecordingListItem {
                recording_ref: "clip_00000000-0000-0000-0000-000000000000".into(),
                camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
                created_at: 1_700_000_000,
                duration_ms: Some(12_000),
                file_size_bytes: 4_096,
                event_meta: Some("{\"texts\":{}}".into()),
                plate_text: Some("WGM12345".into()),
                adr_text: Some("30/1202".into()),
                plate_thumb_ref: Some("snap_00000000-0000-0000-0000-000000000000".into()),
                adr_thumb_ref: None,
            }],
        });
        roundtrip(&RecordingListOut::default());
    }

    /// An OLDER encoder that only knew keys 0..5 for `RecordingListItem` must
    /// still decode into the extended struct (new `Option` fields default to
    /// `None`) — the CBOR map keys are additive and absent maps to `None`.
    #[test]
    fn recording_list_item_back_compat_missing_new_keys() {
        // Hand-build the pre-extension map: keys 0..5 only.
        let old = RecordingListItemLegacy {
            recording_ref: "clip_00000000-0000-0000-0000-000000000000".into(),
            camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
            created_at: 1_700_000_000,
            duration_ms: Some(12_000),
            file_size_bytes: 4_096,
            event_meta: None,
        };
        let mut buf = Vec::new();
        minicbor::encode(&old, &mut buf).unwrap();
        let decoded: RecordingListItem = minicbor::decode(&buf).unwrap();
        assert_eq!(decoded.recording_ref, old.recording_ref);
        assert_eq!(decoded.plate_text, None);
        assert_eq!(decoded.adr_text, None);
        assert_eq!(decoded.plate_thumb_ref, None);
        assert_eq!(decoded.adr_thumb_ref, None);
    }

    /// Mirror of the pre-extension `RecordingListItem` (keys 0..5), used only to
    /// prove forward-compatible decode of an old-encoder payload.
    #[derive(Encode)]
    #[cbor(map)]
    struct RecordingListItemLegacy {
        #[n(0)]
        recording_ref: String,
        #[n(1)]
        camera_id: String,
        #[n(2)]
        created_at: i64,
        #[n(3)]
        duration_ms: Option<i64>,
        #[n(4)]
        file_size_bytes: i64,
        #[n(5)]
        event_meta: Option<String>,
    }

    #[test]
    fn roundtrip_stats_out() {
        roundtrip(&StatsOut {
            stats: StatsTotals {
                total_snapshots: 3,
                total_segments: 1,
                total_size_bytes: 4096,
            },
            per_camera: vec![StatsPerCamera {
                camera_id: "cam_00000000-0000-0000-0000-000000000000".into(),
                snapshots: 3,
                segments: 1,
                size_bytes: 4096,
            }],
        });
    }
}
