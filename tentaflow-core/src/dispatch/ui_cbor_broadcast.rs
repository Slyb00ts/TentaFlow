// =============================================================================
// File: dispatch/ui_cbor_broadcast.rs
// Tokio broadcast channel for UI CBOR push messages (addon → frontend).
// Host function `ui_render_cbor` publishes here; each WS connection subscribes
// and filters by user_id before forwarding as an unsolicited UiChannelCbor frame.
// =============================================================================

use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 512;

/// Payload carried over the broadcast channel.
/// Uses `Arc<[u8]>` for CBOR bytes so cloning across N subscribers is
/// a refcount bump (8 bytes), not N × full-payload memcpy.
#[derive(Debug, Clone)]
pub struct UiCborPush {
    /// User that owns the panel session receiving this message.
    pub user_id: i64,
    /// Raw CBOR bytes (UiPayload wire encoding). Arc-shared to avoid
    /// cloning per broadcast subscriber.
    pub cbor: Arc<[u8]>,
}

static SENDER: OnceLock<broadcast::Sender<UiCborPush>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<UiCborPush> {
    SENDER.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        tx
    })
}

pub fn publish(push: UiCborPush) {
    let _ = channel().send(push);
}

pub fn subscribe() -> broadcast::Receiver<UiCborPush> {
    channel().subscribe()
}
