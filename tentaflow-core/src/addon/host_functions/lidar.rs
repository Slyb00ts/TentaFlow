// =============================================================================
// File: addon/host_functions/lidar.rs
// Purpose: lidar_publish_v1 host ABI — a robot-driver addon publishes ONE
//          canonical, vendor-agnostic point-cloud frame (packed f32, the
//          tentaflow-sdk-spec `LidarFrameHeader` layout) to Core. Core stays a
//          dumb pipe: it validates the header, bounds the size, and stores the
//          LATEST frame per (addon_id, instance_id) in an in-memory holder
//          (`LidarLatest`), the seed of the L2 stream hub. No JSON, one
//          WASM->host copy/frame.
// =============================================================================

use std::sync::OnceLock;

use bytes::Bytes;
use dashmap::DashMap;
use tentaflow_sdk_spec::LidarFrameHeader;

use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
    ABI_ERR_OPERATION, ABI_ERR_PERMISSION, ABI_OK,
};
use crate::audit::RiskClass;

/// Capability a robot addon declares to publish its OWN sensor frames to Core.
/// Low risk: the addon emits data it already owns; it never reads other addons'
/// frames through this fn (the holder is keyed by the caller's own addon_id).
const PERM_LIDAR_PUBLISH: &str = "lidar.publish";

/// Hard cap on a single published canonical frame (header + packed f32 body).
/// At LIDAR_LAYOUT_XYZI that is ~262k points; the Go2 voxel map is far sparser
/// (a few 10k points). A larger frame is rejected before any copy out of guest
/// memory so a malformed length cannot make Core allocate without bound.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// In-memory holder for the LATEST canonical LiDAR frame per publisher. The
/// point cloud is a stream: only the newest frame per publisher is retained, so
/// memory stays bounded regardless of frame rate. This is the seed of the L2
/// LidarStreamHub (fan-out + cross-node relay land later); L1 only keeps a
/// latest-wins slot a future on-demand fetch / stream reads back.
///
/// The publisher is the running WASM INSTANCE, not just the addon: multiple Go2
/// robots are separate instances of the same `addon_id`, so the slot is keyed by
/// `(addon_id, instance_id)` to keep their frames apart.
pub struct LidarLatest {
    frames: DashMap<String, Bytes>,
}

impl LidarLatest {
    fn new() -> Self {
        Self {
            frames: DashMap::new(),
        }
    }

    /// Process-wide singleton holder.
    pub fn global() -> &'static LidarLatest {
        static INSTANCE: OnceLock<LidarLatest> = OnceLock::new();
        INSTANCE.get_or_init(LidarLatest::new)
    }

    /// Composite latest-frame key: a running instance of an addon. Two instances
    /// of the same `addon_id` (e.g. two Go2 robots) get distinct slots.
    fn key(addon_id: &str, instance_id: &str) -> String {
        format!("{addon_id}:{instance_id}")
    }

    /// Overwrite the latest frame for `(addon_id, instance_id)` (latest-wins).
    pub fn set(&self, addon_id: &str, instance_id: &str, frame: Bytes) {
        self.frames.insert(Self::key(addon_id, instance_id), frame);
    }

    /// Read the latest retained frame for `(addon_id, instance_id)`, if any.
    pub fn get(&self, addon_id: &str, instance_id: &str) -> Option<Bytes> {
        self.frames
            .get(&Self::key(addon_id, instance_id))
            .map(|e| e.value().clone())
    }

    /// Drop the latest-frame slot for one running instance. Called on instance
    /// stop/teardown so a stopped/restarted instance does not leave its last
    /// frame retained forever (bounded memory across instance churn).
    pub fn remove(&self, addon_id: &str, instance_id: &str) {
        self.frames.remove(&Self::key(addon_id, instance_id));
    }

    /// Drop every latest-frame slot belonging to `addon_id` (all its instances).
    /// Called on addon uninstall. Per-instance stop MUST use `remove` instead, or
    /// it would wipe sibling instances (e.g. other robots) sharing the addon_id.
    pub fn clear_addon(&self, addon_id: &str) {
        let prefix = format!("{addon_id}:");
        self.frames.retain(|k, _| !k.starts_with(&prefix));
    }
}

fn audit(state: &AddonState, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        "lidar.publish",
        Some("lidar"),
        Some(&state.addon_id),
        RiskClass::Unclassified,
        None,
        None,
        result,
        reason,
    );
}

/// Host function: publish ONE canonical LiDAR frame.
///
/// ABI: `lidar_publish_v1(in_ptr, in_len) -> i32`. `in_ptr/in_len` point at the
/// packed canonical frame in guest memory: a `LidarFrameHeader` followed by
/// `point_count * stride` little-endian f32. Returns `ABI_OK`, or an error code
/// on a missing permission / oversized / malformed frame.
///
/// INVARIANT: exactly one bounded copy from guest memory per frame; the bulk is
/// packed f32 (never CBOR/JSON); the header self-describes the body length so a
/// truncated or inconsistent frame is rejected, never partially stored.
pub fn lidar_publish_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    if !check_permission(caller.data(), PERM_LIDAR_PUBLISH, None) {
        audit(caller.data(), "denied", Some("missing_permission"));
        return ABI_ERR_PERMISSION;
    }

    if in_len < 0 || in_len as usize > MAX_FRAME_BYTES {
        audit(caller.data(), "error", Some("frame_too_large"));
        return ABI_ERR_OPERATION;
    }

    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => {
            audit(caller.data(), "error", Some("no_memory"));
            return ABI_ERR_OPERATION;
        }
    };

    // Validate the header and confirm the declared body length matches the
    // supplied buffer BEFORE retaining anything — never store a partial frame.
    let header = {
        let bytes = match read_guest_bytes(&memory, &caller, in_ptr, in_len) {
            Some(b) => b,
            None => {
                audit(caller.data(), "error", Some("oob_read"));
                return ABI_ERR_OPERATION;
            }
        };
        let header = match LidarFrameHeader::decode_header(bytes) {
            Some(h) => h,
            None => {
                audit(caller.data(), "error", Some("bad_header"));
                return ABI_ERR_OPERATION;
            }
        };
        match header.frame_len() {
            Some(expected) if expected == bytes.len() => {}
            _ => {
                audit(caller.data(), "error", Some("length_mismatch"));
                return ABI_ERR_OPERATION;
            }
        }
        header
    };

    // Single copy out of guest memory into an owned Bytes (the retained frame).
    let frame: Bytes = {
        let bytes = match read_guest_bytes(&memory, &caller, in_ptr, in_len) {
            Some(b) => b,
            None => {
                audit(caller.data(), "error", Some("oob_read"));
                return ABI_ERR_OPERATION;
            }
        };
        Bytes::copy_from_slice(bytes)
    };

    // Success path is lock-light and DB-free: this fn runs at frame rate from the
    // robot drain tick, so it must NOT take the audit/DB writer lock or grow
    // audit_log per frame. Audit rows are written only for the rare, security-
    // relevant deny + malformed-frame cases above. Store the single owned copy in
    // the in-RAM latest-wins slot, keyed by the running instance, and return.
    let addon_id = caller.data().addon_id.clone();
    let instance_id = caller.data().instance_id.clone();
    LidarLatest::global().set(&addon_id, &instance_id, frame);
    let _ = header;
    ABI_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_sdk_spec::{LIDAR_FRAME_VERSION, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ};

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

    #[test]
    fn lidar_latest_set_get_and_latest_wins() {
        let holder = LidarLatest::new();
        assert!(holder.get("go2", "robot-a").is_none());

        let f1 = build_frame(&[[1.0, 2.0, 3.0]], 1);
        holder.set("go2", "robot-a", Bytes::from(f1.clone()));
        let got = holder.get("go2", "robot-a").expect("frame");
        assert_eq!(&got[..], &f1[..]);

        // Latest-wins: a newer frame overwrites the prior one for the same slot.
        let f2 = build_frame(&[[4.0, 5.0, 6.0], [7.0, 8.0, 9.0]], 2);
        holder.set("go2", "robot-a", Bytes::from(f2.clone()));
        let got = holder.get("go2", "robot-a").expect("frame");
        assert_eq!(&got[..], &f2[..]);

        // Stored frame parses back to the expected header (2 points, seq 2).
        let h = LidarFrameHeader::decode_header(&got).expect("decode");
        assert_eq!(h.point_count, 2);
        assert_eq!(h.frame_seq, 2);
        assert_eq!(h.frame_len(), Some(LIDAR_HEADER_LEN + 2 * 3 * 4));

        // Per-instance isolation: a second instance of the SAME addon_id (e.g.
        // a second Go2 robot) gets its own slot and does NOT clobber robot-a.
        let f3 = build_frame(&[[10.0, 11.0, 12.0]], 7);
        holder.set("go2", "robot-b", Bytes::from(f3.clone()));
        let got_b = holder.get("go2", "robot-b").expect("frame b");
        assert_eq!(&got_b[..], &f3[..]);
        // robot-a is untouched by robot-b's publish.
        let got_a = holder.get("go2", "robot-a").expect("frame a");
        assert_eq!(&got_a[..], &f2[..]);

        // Per-instance isolation also holds in reverse: an unknown instance of
        // the same addon, and a different addon_id, both have empty slots.
        assert!(holder.get("go2", "robot-c").is_none());
        assert!(holder.get("other", "robot-a").is_none());
    }

    #[test]
    fn lidar_latest_remove_and_clear_addon() {
        let holder = LidarLatest::new();
        let fa = build_frame(&[[1.0, 2.0, 3.0]], 1);
        let fb = build_frame(&[[4.0, 5.0, 6.0]], 2);
        holder.set("go2", "robot-a", Bytes::from(fa.clone()));
        holder.set("go2", "robot-b", Bytes::from(fb.clone()));

        // remove drops exactly one instance's slot; the sibling remains.
        holder.remove("go2", "robot-a");
        assert!(holder.get("go2", "robot-a").is_none());
        let got_b = holder.get("go2", "robot-b").expect("sibling frame survives");
        assert_eq!(&got_b[..], &fb[..]);

        // A second addon_id sharing an instance name is untouched by clear_addon.
        let fo = build_frame(&[[7.0, 8.0, 9.0]], 3);
        holder.set("other", "robot-b", Bytes::from(fo.clone()));

        // clear_addon drops every slot of the given addon_id and only that addon.
        holder.clear_addon("go2");
        assert!(holder.get("go2", "robot-a").is_none());
        assert!(holder.get("go2", "robot-b").is_none());
        let got_other = holder.get("other", "robot-b").expect("other addon survives");
        assert_eq!(&got_other[..], &fo[..]);
    }
}
