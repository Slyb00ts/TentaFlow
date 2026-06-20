// =============================================================================
// File: services/lidar_hub.rs
// Purpose: LidarStreamHub — the L2 in-memory fan-out point for canonical LiDAR
//          frames. It owns the LATEST-frame-per-robot storage plus a per-robot
//          notify (tokio watch) so future subscribers (the push stream / the
//          cross-node relay in L3) wake on a new frame without polling. Latest-
//          wins: only the newest frame per robot is retained, so memory and the
//          notify channel stay bounded regardless of frame rate.
//
//          Keyed by `robot_id`, which for the go2 addon equals `addon_id` (one
//          robot per install; the host advertises `robot_id = addon_id`). The
//          publishing WASM INSTANCE id is deliberately NOT part of the key: a
//          robot's frame must survive pooled-worker churn / restart and be found
//          by a consumer that only knows the robot_id, never the runtime UUID.
//
//          INVARIANT (why a bare robot_id key cannot collide across orgs): the
//          addon-install id is globally unique with a single owner org. It is
//          minted in `addon::lifecycle::unique_instance_id` as
//          `{package_id}-{uuidv4[..8]}` with a DB-uniqueness retry, and a robot
//          row is single-org. So `robot_id == addon_id` never names two different
//          robots in different tenants; the publish host-fn (which only has the
//          caller's `addon_id`, not its org) can key by the bare id safely, and
//          org-scoping at the consumption layer (`enforce_lidar_subscribe`) is
//          sufficient. Do NOT add org to the key — it is redundant for an
//          invariant that already holds and the publisher has no org to thread.
// =============================================================================

use std::sync::OnceLock;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::watch;

/// Per-robot slot: the latest retained canonical frame plus the notify sender.
/// The watched value is the latest `frame_seq` — a subscriber wakes on a change,
/// then pulls the bytes via `latest()` (latest-wins; no per-frame queue, so a
/// slow subscriber can never build backpressure, it just sees the newest frame).
struct RobotSlot {
    frame: Bytes,
    notify: watch::Sender<u32>,
}

/// Process-wide hub. `slots` maps `robot_id` → its latest frame + notify. The
/// notify is created lazily on first publish OR first subscribe, so a subscriber
/// that arrives before the robot has ever published still gets a live receiver
/// (it observes the initial seq 0 and waits for the first real frame).
pub struct LidarStreamHub {
    slots: DashMap<String, RobotSlot>,
}

impl LidarStreamHub {
    fn new() -> Self {
        Self {
            slots: DashMap::new(),
        }
    }

    /// Process-wide singleton hub.
    pub fn global() -> &'static LidarStreamHub {
        static INSTANCE: OnceLock<LidarStreamHub> = OnceLock::new();
        INSTANCE.get_or_init(LidarStreamHub::new)
    }

    /// Store the latest frame for `robot_id` (latest-wins) and notify subscribers
    /// with its `frame_seq`. The frame bytes are the canonical L1 layout
    /// (`LidarFrameHeader` + packed f32); the caller validates the header BEFORE
    /// publishing, so the hub never has to parse the body. Creates the per-robot
    /// notify channel lazily on first publish.
    ///
    /// Uses a single atomic `entry()` so publish converges on the SAME slot a
    /// concurrent `subscribe()` may have just created: without this, a publish
    /// could see `None`, then `insert()` a fresh slot that replaces the one a
    /// subscriber already holds a receiver from — dropping that sender closes the
    /// subscriber's channel before it ever sees the first frame.
    pub fn publish(&self, robot_id: &str, frame_seq: u32, frame: Bytes) {
        // Mutate the stored bytes and CLONE the (cheap) watch::Sender under the
        // entry guard, but DO NOT send while holding it. `watch::Sender::send()`
        // takes the watch's internal write lock, which blocks behind any
        // outstanding `Receiver::borrow()` read-guard. A consumer that holds a
        // borrow and then calls `latest()` (a DashMap read on the same shard)
        // while we hold the entry guard and wait on `send()` would form a lock
        // cycle. Sending the notify AFTER the guard drops breaks that cycle.
        let sender = {
            let entry = self
                .slots
                .entry(robot_id.to_string())
                .and_modify(|slot| {
                    slot.frame = frame.clone();
                })
                .or_insert_with(|| {
                    let (notify, _rx) = watch::channel(frame_seq);
                    RobotSlot {
                        frame: frame.clone(),
                        notify,
                    }
                });
            entry.notify.clone()
        };
        // A watch send error means there are no receivers; that is fine — the
        // latest frame is still retained for a future subscriber/pull.
        let _ = sender.send(frame_seq);
    }

    /// Read the latest retained frame for `robot_id`, if any.
    pub fn latest(&self, robot_id: &str) -> Option<Bytes> {
        self.slots.get(robot_id).map(|s| s.frame.clone())
    }

    /// Subscribe to new-frame notifications for `robot_id`. The watched value is
    /// the latest `frame_seq`; on a change the subscriber calls `latest()` to pull
    /// the newest bytes. Creates the per-robot slot lazily (with an empty frame)
    /// if the robot has not published yet, so a subscriber can attach first.
    pub fn subscribe(&self, robot_id: &str) -> watch::Receiver<u32> {
        // Single atomic `entry()` so subscribe converges on the SAME slot a
        // concurrent `publish()` may create: if we did a get-then-insert, a
        // publish could replace the slot between our miss and our insert,
        // handing back a receiver on an already-dead sender. First subscriber
        // before any publish seeds an empty frame at seq 0 — `latest()` returns
        // empty Bytes until the first publish (consumer treats it as "no frame
        // yet" because the header decode fails).
        // Clone the (cheap) watch::Sender under the entry guard, drop the guard,
        // THEN call `.subscribe()` on the clone. `.subscribe()` takes the watch's
        // internal lock; holding the DashMap entry guard across it would let it
        // block behind an outstanding `Receiver::borrow()` whose holder is itself
        // waiting on this shard, forming a lock cycle. No DashMap guard may be
        // held across a watch send/subscribe call.
        let sender = {
            let entry = self
                .slots
                .entry(robot_id.to_string())
                .or_insert_with(|| {
                    let (notify, _rx) = watch::channel(0);
                    RobotSlot {
                        frame: Bytes::new(),
                        notify,
                    }
                });
            entry.notify.clone()
        };
        sender.subscribe()
    }

    /// Drop a robot's slot (latest frame + notify). Called when the robot's last
    /// instance is gone or the addon is uninstalled. Existing subscribers observe
    /// the sender closing (their `changed()` resolves with an error) and stop.
    pub fn remove(&self, robot_id: &str) {
        self.slots.remove(robot_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{
        LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ,
    };

    fn build_frame(points: &[[f32; 3]], seq: u32) -> Vec<u8> {
        let header = LidarFrameHeader {
            version: LIDAR_FRAME_VERSION,
            layout: LIDAR_LAYOUT_XYZ,
            point_count: points.len() as u32,
            frame_seq: seq,
            timestamp_us: 123_456,
            resolution: 0.05,
            origin: [0.0, 0.0, 0.0],
        };
        let mut buf = Vec::with_capacity(header.frame_len().unwrap());
        buf.extend_from_slice(&header.encode_header());
        for p in points {
            for c in p {
                buf.extend_from_slice(&c.to_le_bytes());
            }
        }
        buf
    }

    /// Parse the header seq off a retained frame, mirroring the dispatch seq-gate.
    fn seq_of(frame: &Bytes) -> Option<u32> {
        LidarFrameHeader::decode_header(frame).map(|h| h.frame_seq)
    }

    #[test]
    fn publish_latest_wins_per_robot_and_isolation() {
        let hub = LidarStreamHub::new();
        assert!(hub.latest("go2-a").is_none());

        let f1 = build_frame(&[[1.0, 2.0, 3.0]], 1);
        hub.publish("go2-a", 1, Bytes::from(f1.clone()));
        let got = hub.latest("go2-a").expect("frame");
        assert_eq!(&got[..], &f1[..]);

        // Latest-wins: a newer frame overwrites the prior one for the same robot.
        let f2 = build_frame(&[[4.0, 5.0, 6.0], [7.0, 8.0, 9.0]], 2);
        hub.publish("go2-a", 2, Bytes::from(f2.clone()));
        let got = hub.latest("go2-a").expect("frame");
        assert_eq!(&got[..], &f2[..]);
        let h = LidarFrameHeader::decode_header(&got).expect("decode");
        assert_eq!(h.point_count, 2);
        assert_eq!(h.frame_seq, 2);
        assert_eq!(h.frame_len(), Some(LIDAR_HEADER_LEN + 2 * 3 * 4));

        // A different robot_id keeps a fully isolated slot.
        let f3 = build_frame(&[[10.0, 11.0, 12.0]], 7);
        hub.publish("go2-b", 7, Bytes::from(f3.clone()));
        assert_eq!(&hub.latest("go2-b").expect("frame b")[..], &f3[..]);
        assert_eq!(&hub.latest("go2-a").expect("frame a")[..], &f2[..]);

        // Unknown robot stays empty.
        assert!(hub.latest("go2-c").is_none());
    }

    #[test]
    fn remove_drops_only_that_robot() {
        let hub = LidarStreamHub::new();
        hub.publish("go2-a", 1, Bytes::from(build_frame(&[[1.0, 2.0, 3.0]], 1)));
        hub.publish("go2-b", 1, Bytes::from(build_frame(&[[4.0, 5.0, 6.0]], 1)));

        hub.remove("go2-a");
        assert!(hub.latest("go2-a").is_none());
        assert!(hub.latest("go2-b").is_some());
    }

    /// The L2 consumer-proxy decision: the seq-gate the dispatch handler applies.
    /// Latest-wins + wrap-immune: the client gets the current frame whenever its
    /// `frame_seq` DIFFERS from the `since_seq` it last saw (a `>` gate breaks
    /// after u32 wrap). `frame_seq` is 1-based, so a fresh client (since_seq=0)
    /// always receives the first real frame. Unknown robot returns None.
    #[test]
    fn fetch_seq_gate_decision() {
        let hub = LidarStreamHub::new();
        let frame = build_frame(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], 5);
        hub.publish("go2-a", 5, Bytes::from(frame.clone()));

        // Helper mirroring dispatch: return Some(bytes) only if current seq
        // differs from since (latest-wins, wrap-immune).
        let fetch = |robot: &str, since: u32| -> Option<Bytes> {
            let bytes = hub.latest(robot)?;
            let seq = seq_of(&bytes)?;
            if seq != since {
                Some(bytes)
            } else {
                None
            }
        };

        // since differs from current → bytes returned (older case).
        let got = fetch("go2-a", 4).expect("differing frame returned");
        assert_eq!(&got[..], &frame[..]);
        // since == current → None (client already has it).
        assert!(fetch("go2-a", 5).is_none());
        // since differs from current (even if numerically "newer", e.g. after a
        // wrap the client's last-seen is higher) → bytes returned.
        let got = fetch("go2-a", 6).expect("differing frame returned");
        assert_eq!(&got[..], &frame[..]);
        // Fresh client (since_seq=0) on a 1-based first frame → bytes returned.
        let got = fetch("go2-a", 0).expect("first frame returned");
        assert_eq!(&got[..], &frame[..]);
        // unknown robot → None.
        assert!(fetch("go2-unknown", 0).is_none());
    }

    #[tokio::test]
    async fn subscribe_wakes_on_publish() {
        let hub = LidarStreamHub::new();
        // Subscribe before any publish: the slot is created lazily at seq 0.
        let mut rx = hub.subscribe("go2-a");
        assert_eq!(*rx.borrow(), 0);
        assert!(hub.latest("go2-a").map_or(true, |b| b.is_empty()));

        let f = build_frame(&[[1.0, 2.0, 3.0]], 9);
        hub.publish("go2-a", 9, Bytes::from(f.clone()));
        rx.changed().await.expect("notified");
        assert_eq!(*rx.borrow(), 9);
        assert_eq!(&hub.latest("go2-a").expect("frame")[..], &f[..]);
    }

    /// First-publish race: a subscriber that attached BEFORE the first publish
    /// must receive the first frame's seq via its existing watch receiver — the
    /// publish must converge on the slot the subscriber created, not replace it
    /// (which would close the channel and make the subscriber miss the frame).
    #[tokio::test]
    async fn subscribe_before_publish_receives_first_frame_seq() {
        let hub = LidarStreamHub::new();
        let mut rx = hub.subscribe("go2-a");

        // Frame seq is 1-based: the very first real frame is seq 1.
        let f = build_frame(&[[1.0, 2.0, 3.0]], 1);
        hub.publish("go2-a", 1, Bytes::from(f.clone()));

        // The receiver must observe the new seq, NOT a channel-close error.
        rx.changed().await.expect("first frame seq, not channel close");
        assert_eq!(*rx.borrow(), 1);
        assert_eq!(&hub.latest("go2-a").expect("frame")[..], &f[..]);
    }
}
