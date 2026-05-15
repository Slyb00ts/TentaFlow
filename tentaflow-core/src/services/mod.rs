// =============================================================================
// Plik: services/mod.rs
// Opis: Klienci serwisow zewnetrznych — TTS, embeddingi.
//       Eksportuje klientow QUIC/HTTP do komunikacji z silnikami AI.
// =============================================================================

pub mod gpu_snapshot;
pub mod manifest;
pub mod model_download;
pub mod models;
pub mod nim;
pub mod portainer;
pub mod stt;
pub mod teams_bot_bootstrap;
pub mod tts;

// Unified services refactor (Phase 1 — additive, runs alongside legacy code).
pub mod auto_detect;
pub mod backend;
#[cfg(feature = "camera")]
pub mod camera_ingest;
pub mod catalog;
pub mod deploy;
pub mod frame_storage;
pub mod handles_cache;
pub mod lifecycle;
pub mod mesh_registry;
pub mod ports;
pub mod registry;
pub mod runtime;
pub mod snapshot_builder;
pub mod streaming;
pub mod supervisor;
pub mod transport;

pub use tts::{TTSClient, TTSConfigCompat};

// -----------------------------------------------------------------------------
// Global singletons: shared frame storage + streaming bus
// -----------------------------------------------------------------------------
//
// Camera ingest, future media producers, and Service-to-Core consumers all
// reach these through `frame_storage()` / `streaming_bus()`. Storage capacity
// is fixed at 1024 frames — overridable later by config when we move past F1a.

use std::sync::{Arc, OnceLock};

static FRAME_STORAGE: OnceLock<Arc<frame_storage::FrameStorage>> = OnceLock::new();
static STREAMING_BUS: OnceLock<Arc<streaming::StreamingBus>> = OnceLock::new();

pub fn frame_storage() -> &'static Arc<frame_storage::FrameStorage> {
    FRAME_STORAGE.get_or_init(|| Arc::new(frame_storage::FrameStorage::new(1024)))
}

pub fn streaming_bus() -> &'static Arc<streaming::StreamingBus> {
    STREAMING_BUS.get_or_init(|| Arc::new(streaming::StreamingBus::new()))
}
