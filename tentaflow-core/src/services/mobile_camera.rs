// =============================================================================
// File: services/mobile_camera.rs
// Purpose: the phone's camera joins the SAME camera pipeline as any robot — it is a
//          normal `webrtc`-vendor camera source whose H.264 byte channel is fed by
//          the device's NATIVE encoder (MediaCodec / VideoToolbox) over the mobile
//          FFI, instead of by a WebRTC peer. Once registered, the GStreamer tee
//          fans the SAME stream out to the MSE dashboard tile (Branch B) AND the
//          decoded-frame mailbox that TentaVision + the depth-AI consumer read
//          (Branch A) — no MJPEG, no second stream.
//
//          This module just holds the per-device H.264 `Sender` so the FFI push can
//          find the channel the camera registration created. One camera per phone
//          node, keyed by addon_id (== robot_id), same as the other mobile hubs.
// =============================================================================

use std::sync::OnceLock;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::mpsc;

/// Process-wide registry of phone-camera H.264 senders.
pub struct MobileCameraIngest {
    senders: DashMap<String, mpsc::Sender<Bytes>>,
}

impl MobileCameraIngest {
    fn new() -> Self {
        Self { senders: DashMap::new() }
    }

    pub fn global() -> &'static MobileCameraIngest {
        static INSTANCE: OnceLock<MobileCameraIngest> = OnceLock::new();
        INSTANCE.get_or_init(MobileCameraIngest::new)
    }

    /// Record the H.264 sender created by a pushed-camera registration.
    pub fn set_sender(&self, addon_id: &str, tx: mpsc::Sender<Bytes>) {
        self.senders.insert(addon_id.to_string(), tx);
    }

    /// Push one H.264 Annex-B access unit to a specific device's camera channel.
    /// `false` if no camera is registered for it or the channel is full/closed
    /// (latest-relevant video: a dropped frame under backpressure is fine).
    pub fn push(&self, addon_id: &str, au: Bytes) -> bool {
        match self.senders.get(addon_id) {
            Some(tx) => tx.try_send(au).is_ok(),
            None => false,
        }
    }

    /// Push to whatever phone camera is registered (the FFI has no addon_id; one
    /// camera per node). Returns how many channels accepted it.
    pub fn push_any(&self, au: Bytes) -> usize {
        let mut n = 0;
        for e in self.senders.iter() {
            if e.value().try_send(au.clone()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Drop a device's camera channel (uninstall / camera stopped).
    pub fn remove(&self, addon_id: &str) {
        self.senders.remove(addon_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn push_routes_to_registered_sender() {
        let ing = MobileCameraIngest::new();
        let (tx, mut rx) = mpsc::channel::<Bytes>(4);
        ing.set_sender("phone-a", tx);
        assert!(ing.push("phone-a", Bytes::from_static(b"nal")));
        assert_eq!(rx.recv().await.as_deref(), Some(&b"nal"[..]));
        // Unknown device → no sender.
        assert!(!ing.push("phone-x", Bytes::from_static(b"x")));
        // push_any reaches the single registered camera.
        assert_eq!(ing.push_any(Bytes::from_static(b"y")), 1);
        ing.remove("phone-a");
        assert!(!ing.push("phone-a", Bytes::from_static(b"z")));
    }
}
