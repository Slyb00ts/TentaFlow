// =============================================================================
// File: services/mobile_sensors.rs
// Purpose: MobileSensorQueue — the hand-off buffer between a mobile device's NATIVE
//          sensor capture (Swift/Kotlin, via the tentaflow-mobile FFI) and the phone
//          addon's tick. The native layer pushes canonical sample bytes (ImuSample /
//          GnssFix / BaroSample / a LiDAR depth frame) at sensor rate; the phone
//          addon's `on_tick` drains them through the permission-checked
//          `mobile_sensor_drain_v1` host-fn, which feeds the fusion engine + shared
//          map keyed by the addon's own id. ONE device per node, so a single global
//          FIFO preserving capture order (so IMU/GNSS interleave in time for the ESKF).
//          Bounded with drop-oldest: a stalled tick can never grow it without bound,
//          and the ESKF tolerates dropped IMU steps (covariance widens).
// =============================================================================

use std::collections::VecDeque;
use std::sync::OnceLock;

use bytes::Bytes;
use parking_lot::Mutex;

/// Sensor kind tags shared with the mobile FFI (`tentaflow_mobile_push_sensor`).
pub const SENSOR_KIND_IMU: u8 = 1;
pub const SENSOR_KIND_GNSS: u8 = 2;
pub const SENSOR_KIND_BARO: u8 = 3;
/// A canonical `LidarFrame` (depth/LiDAR) → shared map.
pub const SENSOR_KIND_DEPTH: u8 = 4;

/// Max buffered samples before the oldest is dropped. At 100 Hz IMU + a 100 ms drain
/// tick that is ~10 entries/tick; 4096 covers long tick stalls with vast headroom.
const QUEUE_CAP: usize = 4096;

/// Process-wide native-sensor hand-off queue.
pub struct MobileSensorQueue {
    q: Mutex<VecDeque<(u8, Bytes)>>,
}

impl MobileSensorQueue {
    fn new() -> Self {
        Self { q: Mutex::new(VecDeque::new()) }
    }

    pub fn global() -> &'static MobileSensorQueue {
        static INSTANCE: OnceLock<MobileSensorQueue> = OnceLock::new();
        INSTANCE.get_or_init(MobileSensorQueue::new)
    }

    /// Enqueue one captured sample (native → here). Drops the oldest entry on overflow
    /// so a non-draining consumer can never make this grow without bound.
    pub fn push(&self, kind: u8, bytes: Bytes) {
        let mut q = self.q.lock();
        if q.len() >= QUEUE_CAP {
            q.pop_front();
        }
        q.push_back((kind, bytes));
    }

    /// Enqueue owned bytes (the mobile FFI path: no `bytes` dep needed on the caller).
    pub fn push_vec(&self, kind: u8, bytes: Vec<u8>) {
        self.push(kind, Bytes::from(bytes));
    }

    /// Drain everything in capture order (the addon tick → fusion engine).
    pub fn drain(&self) -> Vec<(u8, Bytes)> {
        let mut q = self.q.lock();
        q.drain(..).collect()
    }

    /// Current buffered count (diagnostics / tests).
    pub fn len(&self) -> usize {
        self.q.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.q.lock().is_empty()
    }

    /// Drop all buffered samples (device disconnect).
    pub fn clear(&self) {
        self.q.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_order_and_drain() {
        let qu = MobileSensorQueue::new();
        qu.push(SENSOR_KIND_IMU, Bytes::from_static(b"a"));
        qu.push(SENSOR_KIND_GNSS, Bytes::from_static(b"b"));
        qu.push(SENSOR_KIND_IMU, Bytes::from_static(b"c"));
        assert_eq!(qu.len(), 3);
        let drained = qu.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].0, SENSOR_KIND_IMU);
        assert_eq!(&drained[1].1[..], b"b");
        assert_eq!(drained[2].0, SENSOR_KIND_IMU);
        assert!(qu.is_empty(), "drain empties the queue");
    }

    #[test]
    fn bounded_drops_oldest() {
        let qu = MobileSensorQueue::new();
        for i in 0..(QUEUE_CAP + 10) {
            qu.push(SENSOR_KIND_IMU, Bytes::from(vec![i as u8]));
        }
        assert_eq!(qu.len(), QUEUE_CAP, "never exceeds cap");
    }
}
