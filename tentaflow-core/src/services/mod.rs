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
pub mod frame_proxy;
pub mod frame_storage;
pub mod handles_cache;
pub mod key_storage;
pub mod lifecycle;
pub mod mesh_keys;
pub mod mesh_registry;
pub mod pickup_tokens;
pub mod ports;
pub mod recording;
pub mod registry;
pub mod runtime;
pub mod signed_urls;
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
static PICKUP_TOKEN_ISSUER: OnceLock<Arc<pickup_tokens::PickupTokenIssuer>> = OnceLock::new();
static FRAME_URL_ISSUER: OnceLock<Arc<signed_urls::SignedUrlIssuer>> = OnceLock::new();
static RECORDING_URL_ISSUER: OnceLock<Arc<signed_urls::SignedUrlIssuer>> = OnceLock::new();

pub fn frame_storage() -> &'static Arc<frame_storage::FrameStorage> {
    FRAME_STORAGE.get_or_init(|| Arc::new(frame_storage::FrameStorage::new(1024)))
}

pub fn streaming_bus() -> &'static Arc<streaming::StreamingBus> {
    STREAMING_BUS.get_or_init(|| Arc::new(streaming::StreamingBus::new()))
}

/// Poll interval for the on-disk key watchers. 5 s is the standard
/// compromise: fast enough that an operator running `tentaflow-cli keys
/// rotate <name>` sees the new key engage before the next outstanding
/// signature minted under the previous key is checked, slow enough that
/// the cost (one `stat()` per key per 5 s) is invisible.
const KEY_WATCHER_POLL: std::time::Duration = std::time::Duration::from_secs(5);

pub fn pickup_token_issuer() -> &'static Arc<pickup_tokens::PickupTokenIssuer> {
    PICKUP_TOKEN_ISSUER.get_or_init(|| {
        let issuer = Arc::new(pickup_tokens::PickupTokenIssuer::new());
        if let Ok(path) = key_storage::key_path(pickup_tokens::KEY_NAME) {
            let weak = Arc::downgrade(&issuer);
            key_storage::watcher::spawn_key_watcher(
                pickup_tokens::KEY_NAME,
                path,
                KEY_WATCHER_POLL,
                move |_old, new| {
                    if let Some(iss) = weak.upgrade() {
                        iss.rotate_in_memory(*new);
                    }
                },
            );
        }
        issuer
    })
}

pub fn frame_url_issuer() -> &'static Arc<signed_urls::SignedUrlIssuer> {
    FRAME_URL_ISSUER.get_or_init(|| {
        let issuer =
            Arc::new(signed_urls::SignedUrlIssuer::new(signed_urls::UrlScope::FrameUrl));
        if let Ok(path) = key_storage::key_path(signed_urls::UrlScope::FrameUrl.key_name()) {
            let weak = Arc::downgrade(&issuer);
            key_storage::watcher::spawn_key_watcher(
                signed_urls::UrlScope::FrameUrl.key_name(),
                path,
                KEY_WATCHER_POLL,
                move |_old, new| {
                    if let Some(iss) = weak.upgrade() {
                        iss.rotate_in_memory(*new);
                    }
                },
            );
        }
        issuer
    })
}

pub fn recording_url_issuer() -> &'static Arc<signed_urls::SignedUrlIssuer> {
    RECORDING_URL_ISSUER.get_or_init(|| {
        let issuer = Arc::new(signed_urls::SignedUrlIssuer::new(
            signed_urls::UrlScope::Recording,
        ));
        if let Ok(path) = key_storage::key_path(signed_urls::UrlScope::Recording.key_name()) {
            let weak = Arc::downgrade(&issuer);
            key_storage::watcher::spawn_key_watcher(
                signed_urls::UrlScope::Recording.key_name(),
                path,
                KEY_WATCHER_POLL,
                move |_old, new| {
                    if let Some(iss) = weak.upgrade() {
                        iss.rotate_in_memory(*new);
                    }
                },
            );
        }
        issuer
    })
}
