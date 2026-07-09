// ===== File: services/vision_worker/fleet.rs — camera assignment authority + link-frame router =====
//
// Stage B of docs/VISION_WORKER_SHARDING.md. The fleet is the core-side brain
// of camera sharding:
//
//   * Assignment authority: `cameras.vision_worker_slot` is computed here
//     (`fnv1a(camera_id) % total_workers`), persisted by the CORE only, and
//     cached in memory so hot paths (ensure_analysis gating, dashboard
//     subscribe) never touch SQLite. Workers act solely on link commands.
//   * Replay: a worker (re)connecting gets one AssignCamera per owned camera —
//     workers are stateless, so a respawn re-learns its subset here.
//   * Router: worker-originated data frames land here. DetectionsBatch is
//     republished verbatim into the core `detection_bus` (dashboard consumer
//     unchanged), CameraHealthReport feeds the in-memory health merge the
//     local ingest registry otherwise provides, StreamFrame/StreamEnd drive
//     the lazy per-tile relay sources (`source::WorkerCameraStreamSource`).
//
// Everything here is a no-op when `[vision].workers_per_gpu = 0`: the fleet
// global is never installed and every helper returns "not worker-owned".

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::db::repository::{self, CameraRow};
use crate::db::DbPool;
use crate::services::camera_ingest::session::CameraHealth;
use crate::services::detection_bus;
use crate::services::stream_hub::{BinaryStreamSource, StreamHub, StreamHubError};

use super::link::{CameraAssignment, LinkFrame, LinkState};
use super::source::WorkerCameraStreamSource;

/// A worker health report older than this no longer masks the DB row's
/// persisted status — a dead worker must not keep a camera "online" forever.
const HEALTH_TTL: Duration = Duration::from_secs(10);

/// One active per-tile relay stream (core side).
struct StreamEntry {
    worker_id: u32,
    source: Weak<WorkerCameraStreamSource>,
}

/// Core-side fleet state. Constructed once by the vision-worker supervisor
/// when `workers_per_gpu > 0` and at least one vision GPU exists.
pub struct WorkerFleet {
    total_workers: u32,
    link: Arc<LinkState>,
    /// camera_id → slot cache. Written by the claim paths (hydrate, runtime
    /// add, replay); read by the hot gating paths.
    assigned: parking_lot::RwLock<HashMap<String, u32>>,
    /// Last link-reported health per worker camera.
    health: parking_lot::Mutex<HashMap<String, (CameraHealth, Instant)>>,
    /// Active relay streams keyed by the core-minted link stream id.
    streams: parking_lot::Mutex<HashMap<u64, StreamEntry>>,
    next_stream_id: AtomicU64,
}

static FLEET: OnceLock<Arc<WorkerFleet>> = OnceLock::new();

/// The installed fleet, if vision-worker sharding is enabled on this node.
pub fn global() -> Option<&'static Arc<WorkerFleet>> {
    FLEET.get()
}

/// Slot of a worker-owned camera (in-memory cache only — cheap enough for
/// per-subscribe gating). `None` = the camera runs in-process (or sharding is
/// disabled entirely).
pub fn is_worker_camera(camera_id: &str) -> Option<u32> {
    global().and_then(|f| f.slot_of(camera_id))
}

/// Fresh link-reported health for a worker-owned camera, if any.
pub fn worker_camera_health(camera_id: &str) -> Option<CameraHealth> {
    global().and_then(|f| f.health_of(camera_id))
}

impl WorkerFleet {
    /// Installs the process-wide fleet. Returns `None` (sharding off) when
    /// the worker count is zero. Called exactly once by the vision-worker
    /// supervisor before any worker is spawned.
    pub fn install(total_workers: u32, link: Arc<LinkState>) -> Option<Arc<Self>> {
        if total_workers == 0 {
            return None;
        }
        let fleet = Arc::new(Self {
            total_workers,
            link,
            assigned: parking_lot::RwLock::new(HashMap::new()),
            health: parking_lot::Mutex::new(HashMap::new()),
            streams: parking_lot::Mutex::new(HashMap::new()),
            next_stream_id: AtomicU64::new(1),
        });
        match FLEET.set(fleet) {
            Ok(()) => FLEET.get().cloned(),
            Err(_) => {
                // Double install is a supervisor bug — keep the first fleet.
                warn!("[vision-fleet] install called twice; keeping the existing fleet");
                FLEET.get().cloned()
            }
        }
    }

    // =========================================================================
    // Assignment authority
    // =========================================================================

    /// Deterministic slot for a NEW camera, or `None` when this camera kind
    /// never shards (WebRTC cameras are bound to a live in-core track).
    /// Pure planning — nothing is persisted; the caller commits after its DB
    /// insert succeeds.
    pub fn plan_slot(&self, camera_id: &str, vendor: &str) -> Option<u32> {
        if vendor == "webrtc" {
            return None;
        }
        Some(slot_for(camera_id, self.total_workers))
    }

    /// Claims a persisted camera row for the fleet: reuses a valid stored
    /// slot, (re)computes and PERSISTS one when the column is NULL or stale
    /// (e.g. the worker count shrank), caches the mapping and registers the
    /// relay stream factories. Returns `None` for cameras that stay local.
    pub fn claim_persisted(self: &Arc<Self>, pool: &DbPool, row: &CameraRow) -> Option<u32> {
        if row.vendor == "webrtc" {
            return None;
        }
        let slot = match row.vision_worker_slot {
            Some(s) if s >= 0 && (s as u32) < self.total_workers => s as u32,
            stale => {
                let s = slot_for(&row.camera_id, self.total_workers);
                if stale.is_some() {
                    info!(
                        camera_id = %row.camera_id,
                        stale = ?stale,
                        slot = s,
                        "[vision-fleet] stale worker slot rebalanced"
                    );
                }
                if let Err(e) =
                    repository::set_camera_worker_slot(pool, &row.camera_id, Some(s as i64))
                {
                    warn!(camera_id = %row.camera_id, "[vision-fleet] slot persist failed: {e}");
                }
                s
            }
        };
        self.note_assignment(&row.camera_id, slot);
        Some(slot)
    }

    /// Commits a runtime-added camera: persists the planned slot (the row
    /// must already exist), caches the mapping, registers the relay
    /// factories and pushes AssignCamera to the owning worker if its link is
    /// up (a disconnected worker receives it via replay on reconnect).
    pub fn commit_assignment(
        self: &Arc<Self>,
        pool: &DbPool,
        slot: u32,
        assignment: CameraAssignment,
    ) {
        if let Err(e) =
            repository::set_camera_worker_slot(pool, &assignment.camera_id, Some(slot as i64))
        {
            warn!(camera_id = %assignment.camera_id, "[vision-fleet] slot persist failed: {e}");
        }
        self.note_assignment(&assignment.camera_id, slot);
        self.send_background(slot, LinkFrame::AssignCamera { camera: assignment });
    }

    /// Re-sends the current config to the owning worker (credentials
    /// rotation for a live worker camera). Returns `false` when the camera
    /// is not worker-owned so the caller falls back to the local restart.
    pub fn dispatch_restart(self: &Arc<Self>, assignment: CameraAssignment) -> bool {
        let Some(slot) = self.slot_of(&assignment.camera_id) else {
            return false;
        };
        self.send_background(slot, LinkFrame::AssignCamera { camera: assignment });
        true
    }

    /// Drops a camera from the fleet (camera removal): clears the cache,
    /// unregisters the relay factories and tells the owning worker to stop
    /// the session. The soft-deleted row keeps its slot column (harmless —
    /// reads filter on `removed_at IS NULL`).
    pub fn forget_camera(self: &Arc<Self>, camera_id: &str) {
        let slot = self.assigned.write().remove(camera_id);
        self.health.lock().remove(camera_id);
        let hub = StreamHub::global();
        hub.unregister_factory(&format!("camera:{camera_id}"));
        hub.unregister_factory(&format!("camera:{camera_id}#preview"));
        if let Some(slot) = slot {
            self.send_background(
                slot,
                LinkFrame::RemoveCamera {
                    camera_id: camera_id.to_string(),
                },
            );
        }
    }

    /// Cached slot lookup.
    pub fn slot_of(&self, camera_id: &str) -> Option<u32> {
        self.assigned.read().get(camera_id).copied()
    }

    fn health_of(&self, camera_id: &str) -> Option<CameraHealth> {
        self.health
            .lock()
            .get(camera_id)
            .filter(|(_, at)| at.elapsed() < HEALTH_TTL)
            .map(|(h, _)| h.clone())
    }

    /// Caches the mapping and lazily registers the per-tile relay factories
    /// under the SAME hub keys the local ingest would have used, so the
    /// dashboard subscribe path needs no changes.
    fn note_assignment(self: &Arc<Self>, camera_id: &str, slot: u32) {
        {
            let mut assigned = self.assigned.write();
            if assigned.insert(camera_id.to_string(), slot) == Some(slot) {
                return; // unchanged — factories are already registered
            }
        }
        for preview in [false, true] {
            let stream_id = if preview {
                format!("camera:{camera_id}#preview")
            } else {
                format!("camera:{camera_id}")
            };
            let fleet = Arc::downgrade(self);
            let camera_id = camera_id.to_string();
            let factory = Box::new(move || {
                let fleet = fleet.upgrade().ok_or_else(|| {
                    StreamHubError::FactoryFailed("vision-worker fleet stopped".into())
                })?;
                let source = fleet.open_stream(&camera_id, preview)?;
                Ok(source as Arc<dyn BinaryStreamSource>)
            });
            if let Err(e) = StreamHub::global().register_factory(stream_id.clone(), factory) {
                warn!(stream_id = %stream_id, "[vision-fleet] register_factory failed: {e}");
            }
        }
    }

    // =========================================================================
    // Per-tile stream relay (core side)
    // =========================================================================

    /// Opens one relay stream for a hub cold-subscribe: mints a link stream
    /// id, registers the source in the router and asks the owning worker to
    /// start pumping. The source's `Drop` (last hub subscriber gone) calls
    /// [`Self::close_stream`].
    fn open_stream(
        self: &Arc<Self>,
        camera_id: &str,
        preview: bool,
    ) -> Result<Arc<WorkerCameraStreamSource>, StreamHubError> {
        let slot = self.slot_of(camera_id).ok_or_else(|| {
            StreamHubError::FactoryFailed(format!("camera {camera_id} is not worker-owned"))
        })?;
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let source =
            WorkerCameraStreamSource::new(camera_id, preview, stream_id, Arc::downgrade(self));
        self.streams.lock().insert(
            stream_id,
            StreamEntry {
                worker_id: slot,
                source: Arc::downgrade(&source),
            },
        );
        self.send_background(
            slot,
            LinkFrame::StreamStart {
                stream_id,
                camera_id: camera_id.to_string(),
                preview,
            },
        );
        Ok(source)
    }

    /// Tears one relay stream down (source dropped by the hub). Best-effort:
    /// a disconnected worker has already stopped pumping.
    pub(super) fn close_stream(&self, stream_id: u64) {
        if let Some(entry) = self.streams.lock().remove(&stream_id) {
            self.send_background(entry.worker_id, LinkFrame::StreamStop { stream_id });
        }
    }

    // =========================================================================
    // Link plumbing
    // =========================================================================

    /// Handles one worker-originated data frame (called from the link read
    /// loop — must stay non-blocking).
    pub fn handle_worker_frame(&self, worker_id: u32, frame: LinkFrame) {
        match frame {
            LinkFrame::DetectionsBatch { frames } => {
                for f in frames {
                    detection_bus::publish_detections(
                        &f.camera_id,
                        f.ts_ms,
                        f.pts_ns,
                        f.proc_ms,
                        f.items,
                    );
                }
            }
            LinkFrame::CameraHealthReport { cameras } => {
                let now = Instant::now();
                let mut health = self.health.lock();
                for h in cameras {
                    health.insert(h.camera_id.clone(), (h, now));
                }
            }
            LinkFrame::StreamFrame {
                stream_id,
                is_init,
                base_pts_ns,
                data,
            } => {
                let source = {
                    let streams = self.streams.lock();
                    streams
                        .get(&stream_id)
                        // A stale respawn must not feed another worker's stream.
                        .filter(|e| e.worker_id == worker_id)
                        .and_then(|e| e.source.upgrade())
                };
                match source {
                    Some(source) => source.push_frame(is_init, base_pts_ns, data),
                    // Source already dropped (tile closed) — StreamStop is on
                    // its way; a few in-flight frames are expected.
                    None => debug!(stream_id, "[vision-fleet] frame for gone stream dropped"),
                }
            }
            LinkFrame::StreamEnd { stream_id } => {
                let source = {
                    let mut streams = self.streams.lock();
                    match streams.get(&stream_id) {
                        Some(e) if e.worker_id == worker_id => {
                            streams.remove(&stream_id).and_then(|e| e.source.upgrade())
                        }
                        _ => None,
                    }
                };
                if let Some(source) = source {
                    source.mark_terminal();
                }
            }
            other => debug!(worker_id, ?other, "[vision-fleet] unexpected frame"),
        }
    }

    /// Replays camera assignments to a freshly connected worker. Claims run
    /// on the blocking pool (SQLite writes for NULL/stale slots), then the
    /// AssignCamera frames go out over the worker's outbound channel.
    pub fn on_worker_connected(self: &Arc<Self>, worker_id: u32) {
        let fleet = Arc::clone(self);
        tokio::spawn(async move {
            let claim_fleet = Arc::clone(&fleet);
            let assignments = tokio::task::spawn_blocking(move || {
                let Some(pool) = crate::db::global_pool() else {
                    warn!("[vision-fleet] no global DB pool — assignment replay skipped");
                    return Vec::new();
                };
                let rows = match repository::list_all_active_cameras(&pool) {
                    Ok(rows) => rows,
                    Err(e) => {
                        warn!("[vision-fleet] replay camera list failed: {e}");
                        return Vec::new();
                    }
                };
                rows.iter()
                    .filter(|row| claim_fleet.claim_persisted(&pool, row) == Some(worker_id))
                    .map(CameraAssignment::from_row)
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();

            if assignments.is_empty() {
                return;
            }
            let Some(tx) = fleet.link.sender(worker_id) else {
                return; // already gone again — the next reconnect replays
            };
            let count = assignments.len();
            for camera in assignments {
                if tx.send(LinkFrame::AssignCamera { camera }).await.is_err() {
                    return;
                }
            }
            info!(worker_id, count, "[vision-fleet] assignments replayed");
        });
    }

    /// Worker link dropped: every relay stream it fed is terminal (tiles
    /// resubscribe once the respawned worker replays). Health entries expire
    /// via [`HEALTH_TTL`] rather than being cleared here — the respawn
    /// usually reports fresh health within a heartbeat or two.
    pub fn on_worker_disconnected(&self, worker_id: u32) {
        let dropped: Vec<_> = {
            let mut streams = self.streams.lock();
            let ids: Vec<u64> = streams
                .iter()
                .filter(|(_, e)| e.worker_id == worker_id)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| streams.remove(&id))
                .filter_map(|e| e.source.upgrade())
                .collect()
        };
        for source in dropped {
            source.mark_terminal();
        }
    }

    /// Sends one frame to a worker without blocking the caller. Callers sit
    /// in sync contexts (hub factory, host functions, Drop impls), so the
    /// await rides a spawned task; outside a runtime the bounded `try_send`
    /// is the best effort left.
    fn send_background(&self, worker_id: u32, frame: LinkFrame) {
        let Some(tx) = self.link.sender(worker_id) else {
            debug!(
                worker_id,
                "[vision-fleet] worker not connected — frame deferred to replay"
            );
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = tx.send(frame).await;
                });
            }
            Err(_) => {
                let _ = tx.try_send(frame);
            }
        }
    }
}

/// Stable FNV-1a hash of the camera id, reduced modulo the worker count.
/// Deterministic across restarts and platforms so a re-claim always lands on
/// the persisted slot.
fn slot_for(camera_id: &str, total_workers: u32) -> u32 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in camera_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash % u64::from(total_workers)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_for_is_deterministic_and_in_range() {
        for total in [1u32, 2, 3, 6, 16] {
            for i in 0..64 {
                let id = format!("cam_550e8400-e29b-41d4-a716-4466554400{i:02}");
                let a = slot_for(&id, total);
                let b = slot_for(&id, total);
                assert_eq!(a, b, "hash must be stable");
                assert!(a < total, "slot {a} out of range for {total} workers");
            }
        }
    }

    #[test]
    fn slot_for_spreads_across_workers() {
        let total = 3u32;
        let mut counts = [0usize; 3];
        for i in 0..300 {
            let id = format!("cam_{i:08}-e29b-41d4-a716-446655440000");
            counts[slot_for(&id, total) as usize] += 1;
        }
        for (slot, &count) in counts.iter().enumerate() {
            assert!(
                count > 50,
                "slot {slot} starved ({count}/300) — hash is not spreading"
            );
        }
    }

    #[test]
    fn plan_slot_skips_webrtc_and_gates_on_install_size() {
        assert!(WorkerFleet::install(0, LinkState::new()).is_none());
        let fleet = Arc::new(WorkerFleet {
            total_workers: 3,
            link: LinkState::new(),
            assigned: parking_lot::RwLock::new(HashMap::new()),
            health: parking_lot::Mutex::new(HashMap::new()),
            streams: parking_lot::Mutex::new(HashMap::new()),
            next_stream_id: AtomicU64::new(1),
        });
        assert!(fleet.plan_slot("cam_x", "webrtc").is_none());
        let slot = fleet.plan_slot("cam_x", "rtsp").expect("rtsp shards");
        assert!(slot < 3);
        // Planning persists nothing and caches nothing.
        assert!(fleet.slot_of("cam_x").is_none());
    }
}
