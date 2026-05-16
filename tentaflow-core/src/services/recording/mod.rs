// =============================================================================
// File: services/recording/mod.rs — public API for recording manager (M1.W8)
// =============================================================================
//
// PNG snapshots are always available. MP4 segments require the `camera`
// feature because they pull in GStreamer. The `SavedRecording` struct is the
// catalog entry the host-function layer (Chunk C) will persist into the
// `recordings` table.

mod error;
#[cfg(feature = "camera")]
mod segment;
mod snapshot;
mod storage;

use std::path::PathBuf;

pub use error::{RecordingError, Result};
pub use snapshot::save_snapshot_rgb24;
pub use storage::{
    camera_subdir, purge_recording, read_recording, recording_base_dir, sha256_hex,
    validate_camera_id, RecordingKind, RecordingRef,
};

#[cfg(feature = "camera")]
pub use segment::save_segment_mp4;

/// Catalog entry returned by every save_*. The host-function layer (Chunk C)
/// inserts this into the `recordings` table; the HTTP handler (Chunk D) reads
/// it back to serve the file.
#[derive(Debug, Clone)]
pub struct SavedRecording {
    pub recording_ref: RecordingRef,
    pub kind: RecordingKind,
    pub file_path: PathBuf,
    pub file_size_bytes: u64,
    /// Segment duration in milliseconds. `None` for snapshots.
    pub duration_ms: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Container/codec hint — `"png"` for snapshots, `None` for segments
    /// because we don't probe mp4 metadata in F1a.
    pub pixel_format: Option<String>,
    pub hash_sha256: String,
    pub created_at: u64,
}
