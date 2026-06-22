// =============================================================================
// File: voxel_map.rs — bounded, dedup'd world-frame occupancy accumulation.
// Purpose: the "common point map" geometry for PRE-FUSED occupancy sensors (the
// Go2 `voxel_map_compressed`, which arrives already in a fixed odom WORLD frame
// and is re-sent every frame). Such a source must NOT go through scan-to-map LIO
// (UNIFIED_SLAM_ARCHITECTURE §15, option B): we trust the device pose and simply
// fold its world cells into ONE growing set, deduplicated on a fixed voxel grid.
// Re-seeing a cell is idempotent, so the set converges to the room rather than
// duplicating. Bounded by a cell cap with refresh-on-touch FIFO eviction, so a
// long session stays in memory while currently-observed geometry survives.
//
// A per-source placement transform lets several robots' world frames be folded
// into one shared scene frame (cross-robot fusion): identity for a single robot,
// the robot→scene alignment otherwise.
// =============================================================================

use std::collections::HashMap;
use std::collections::VecDeque;

use nalgebra::Point3;

use crate::pose::Pose;

/// Integer voxel-cell coordinate on the accumulation grid (`round(world / res)`).
pub type Cell = [i32; 3];

/// A bounded, deduplicated set of occupied voxel cells in a single world frame.
///
/// Geometry is keyed by quantized [`Cell`], so the same surface re-observed across
/// frames collapses to one entry. The cap bounds memory; eviction is FIFO by last
/// touch — re-seeing a cell refreshes it, so a robot revisiting a room keeps that
/// room alive while stale, no-longer-seen geometry ages out first.
#[derive(Debug, Clone)]
pub struct SceneVoxelMap {
    resolution: f32,
    inv_res: f32,
    cap: usize,
    /// Occupied cells → the sequence number of their most recent touch. The seq
    /// disambiguates stale eviction-queue entries (lazy deletion).
    cells: HashMap<Cell, u64>,
    /// FIFO of `(touch_seq, cell)`; the front is the oldest *candidate*. An entry
    /// whose seq no longer matches `cells[cell]` was refreshed later and is skipped.
    order: VecDeque<(u64, Cell)>,
    next_seq: u64,
    /// Bumped ONLY when the occupied-cell SET changes (a cell added or evicted) — not
    /// on a pure refresh of an already-present cell. Lets a consumer (the scene push
    /// source) skip re-broadcasting a static map even while frames keep re-arriving.
    revision: u64,
}

impl SceneVoxelMap {
    /// `resolution` = voxel edge in metres (Go2 = 0.05). `cap` = max occupied cells
    /// retained (the browser viewer uses 400k; mirror that as a sane default upstream).
    pub fn new(resolution: f32, cap: usize) -> Self {
        assert!(resolution > 0.0, "voxel resolution must be positive");
        assert!(cap > 0, "voxel cap must be positive");
        SceneVoxelMap {
            resolution,
            inv_res: 1.0 / resolution,
            cap,
            cells: HashMap::new(),
            order: VecDeque::new(),
            next_seq: 0,
            revision: 0,
        }
    }

    /// Occupied-set change counter — bumps only when a cell is added or evicted (see
    /// `revision`). Stable across pure refreshes of the same geometry.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn resolution(&self) -> f32 {
        self.resolution
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn clear(&mut self) {
        if !self.cells.is_empty() {
            self.revision += 1;
        }
        self.cells.clear();
        self.order.clear();
        self.next_seq = 0;
    }

    /// Quantize a world point to its cell. `round` (not floor) so a point sits in the
    /// cell whose CENTRE is nearest — matching the Go2 grid where every voxel already
    /// lands on `origin + index * resolution`. Returns `None` for a non-finite
    /// coordinate: `as i32` would silently fold `NaN` to cell 0 and saturate `Inf` to
    /// the integer extreme, polluting the map with a bogus cell — drop it instead,
    /// mirroring `decode_lidar_frame`'s rejection of malformed frames.
    #[inline]
    fn cell_of(&self, p: [f32; 3]) -> Option<Cell> {
        if !(p[0].is_finite() && p[1].is_finite() && p[2].is_finite()) {
            return None;
        }
        Some([
            (p[0] * self.inv_res).round() as i32,
            (p[1] * self.inv_res).round() as i32,
            (p[2] * self.inv_res).round() as i32,
        ])
    }

    /// World-frame centre of a cell (the value handed to the renderer as an instance).
    #[inline]
    fn cell_center(&self, c: Cell) -> [f32; 3] {
        [
            c[0] as f32 * self.resolution,
            c[1] as f32 * self.resolution,
            c[2] as f32 * self.resolution,
        ]
    }

    /// Insert one already-world-frame point (idempotent per cell; refreshes touch).
    /// A non-finite coordinate is dropped.
    pub fn insert_world(&mut self, p: [f32; 3]) {
        if let Some(cell) = self.cell_of(p) {
            self.touch(cell);
        }
    }

    /// Fold a batch of world-frame points (the common single-robot path: the source
    /// is already in the scene frame, so no placement transform is applied).
    /// Non-finite points are skipped.
    pub fn insert_world_points(&mut self, points: &[Point3<f32>]) {
        for p in points {
            if let Some(cell) = self.cell_of([p.x, p.y, p.z]) {
                self.touch(cell);
            }
        }
    }

    /// Fold a batch of points expressed in a SOURCE frame, placed into this scene
    /// frame by `source_to_scene` first (cross-robot fusion). For a single robot
    /// whose odom frame IS the scene frame, pass identity / use [`insert_world_points`].
    /// Non-finite points are skipped.
    pub fn insert_points_via(&mut self, points: &[Point3<f32>], source_to_scene: &Pose) {
        for p in points {
            if !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()) {
                continue;
            }
            let w = source_to_scene.transform_point([p.x as f64, p.y as f64, p.z as f64]);
            if let Some(cell) = self.cell_of([w[0] as f32, w[1] as f32, w[2] as f32]) {
                self.touch(cell);
            }
        }
    }

    /// Mark a cell occupied (or refresh it) and enforce the cap.
    #[inline]
    fn touch(&mut self, cell: Cell) {
        let seq = self.next_seq;
        self.next_seq += 1;
        // A brand-new cell changes the set (bump revision); re-touching an existing
        // cell only refreshes its eviction order, so the set is unchanged.
        if self.cells.insert(cell, seq).is_none() {
            self.revision += 1;
        }
        self.order.push_back((seq, cell));
        // Cap is on DISTINCT cells; refreshes grow `order` without growing `cells`,
        // so trim those stale heads opportunistically, then evict true overflow.
        self.evict_to_cap();
    }

    fn evict_to_cap(&mut self) {
        while self.cells.len() > self.cap {
            match self.order.pop_front() {
                Some((seq, cell)) => {
                    // Only evict if this queue entry is still the cell's CURRENT touch;
                    // otherwise it is a stale pre-refresh entry — drop it and continue.
                    if self.cells.get(&cell) == Some(&seq) {
                        self.cells.remove(&cell);
                        self.revision += 1;
                    }
                }
                None => break,
            }
        }
        // Keep the lazy-deletion queue from growing unbounded under heavy refresh:
        // when it carries far more entries than live cells, the head is mostly stale.
        if self.order.len() > self.cells.len().saturating_mul(2) + self.cap {
            self.compact_order();
        }
    }

    /// Rebuild `order` from the live cells (drops all stale entries). O(n log n) but
    /// rare (only when the queue bloats from refresh churn), so amortized cheap.
    fn compact_order(&mut self) {
        let mut live: Vec<(u64, Cell)> = self.cells.iter().map(|(c, s)| (*s, *c)).collect();
        live.sort_unstable_by_key(|(s, _)| *s);
        self.order = live.into_iter().collect();
    }

    /// Iterate the occupied cells' world-frame centres (renderer instance stream).
    /// Order is unspecified (it is a set); callers that need stability sort.
    pub fn iter_world(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.cells.keys().map(move |&c| self.cell_center(c))
    }

    /// Packed `[x,y,z,x,y,z,...]` world-frame centres for the binary render stream.
    pub fn to_packed_xyz(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.cells.len() * 3);
        for &c in self.cells.keys() {
            let w = self.cell_center(c);
            out.extend_from_slice(&w);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pose::translation_pose;

    fn p(x: f32, y: f32, z: f32) -> Point3<f32> {
        Point3::new(x, y, z)
    }

    #[test]
    fn dedups_repeated_and_same_cell_points() {
        let mut m = SceneVoxelMap::new(0.05, 1000);
        m.insert_world([1.00, 2.00, 3.00]);
        m.insert_world([1.00, 2.00, 3.00]); // identical → same cell
        m.insert_world([1.01, 2.00, 3.00]); // within 5 cm → same cell (rounds equal)
        assert_eq!(m.len(), 1);
        // A point a full voxel away is a distinct cell.
        m.insert_world([1.06, 2.00, 3.00]);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn cell_center_snaps_to_grid() {
        let mut m = SceneVoxelMap::new(0.10, 1000);
        m.insert_world([0.34, -0.04, 0.96]); // → cells (3, 0, 10)
        let centers: Vec<[f32; 3]> = m.iter_world().collect();
        assert_eq!(centers.len(), 1);
        let c = centers[0];
        assert!((c[0] - 0.30).abs() < 1e-5);
        assert!((c[1] - 0.00).abs() < 1e-5);
        assert!((c[2] - 1.00).abs() < 1e-5);
    }

    #[test]
    fn bounded_by_cap_with_fifo_eviction() {
        let mut m = SceneVoxelMap::new(1.0, 3);
        for i in 0..6 {
            m.insert_world([i as f32, 0.0, 0.0]); // 6 distinct cells, cap 3
        }
        assert_eq!(m.len(), 3, "never exceeds cap");
        let xs: Vec<i32> = m.iter_world().map(|c| c[0].round() as i32).collect();
        // Oldest (0,1,2) evicted; newest (3,4,5) survive.
        for old in [0, 1, 2] {
            assert!(!xs.contains(&old), "stale cell {old} should be evicted");
        }
        for keep in [3, 4, 5] {
            assert!(xs.contains(&keep), "recent cell {keep} should survive");
        }
    }

    #[test]
    fn refresh_keeps_touched_cell_alive() {
        let mut m = SceneVoxelMap::new(1.0, 2);
        m.insert_world([0.0, 0.0, 0.0]); // A
        m.insert_world([1.0, 0.0, 0.0]); // B
        m.insert_world([0.0, 0.0, 0.0]); // refresh A → A now newer than B
        m.insert_world([2.0, 0.0, 0.0]); // C → evicts oldest, which is now B
        let xs: Vec<i32> = m.iter_world().map(|c| c[0].round() as i32).collect();
        assert!(xs.contains(&0), "refreshed cell A survives");
        assert!(xs.contains(&2), "newest cell C present");
        assert!(!xs.contains(&1), "un-refreshed cell B evicted");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn refresh_churn_does_not_bloat_queue_unbounded() {
        let mut m = SceneVoxelMap::new(1.0, 4);
        // Hammer the same 2 cells thousands of times: cells stays at 2, and the
        // lazy-deletion queue must be compacted rather than grow without bound.
        for _ in 0..5000 {
            m.insert_world([0.0, 0.0, 0.0]);
            m.insert_world([1.0, 0.0, 0.0]);
        }
        assert_eq!(m.len(), 2);
        assert!(m.order.len() <= m.cells.len() * 2 + m.cap + 2, "queue stays bounded");
    }

    #[test]
    fn insert_points_via_applies_placement_transform() {
        let mut scene = SceneVoxelMap::new(0.5, 1000);
        // A source-frame point at origin, placed by a +10 m X shift → scene cell at 10.
        scene.insert_points_via(&[p(0.0, 0.0, 0.0)], &translation_pose([10.0, 0.0, 0.0]));
        let c = scene.iter_world().next().unwrap();
        assert!((c[0] - 10.0).abs() < 1e-4);
        // Same physical point, identity placement → different cell (origin).
        let mut local = SceneVoxelMap::new(0.5, 1000);
        local.insert_world_points(&[p(0.0, 0.0, 0.0)]);
        let c2 = local.iter_world().next().unwrap();
        assert!((c2[0]).abs() < 1e-4);
    }

    #[test]
    fn packed_xyz_matches_cell_count() {
        let mut m = SceneVoxelMap::new(0.05, 1000);
        m.insert_world_points(&[p(0.0, 0.0, 0.0), p(5.0, 5.0, 5.0), p(0.0, 0.0, 0.0)]);
        assert_eq!(m.len(), 2);
        assert_eq!(m.to_packed_xyz().len(), 2 * 3);
    }

    #[test]
    fn non_finite_points_are_dropped_not_quantized_to_zero() {
        let mut m = SceneVoxelMap::new(0.05, 1000);
        m.insert_world([f32::NAN, 0.0, 0.0]);
        m.insert_world([0.0, f32::INFINITY, 0.0]);
        m.insert_world([0.0, 0.0, f32::NEG_INFINITY]);
        assert!(m.is_empty(), "bad coords must not create a cell-0 / saturated cell");
        m.insert_world_points(&[p(f32::NAN, 1.0, 1.0), p(2.0, 2.0, 2.0)]);
        assert_eq!(m.len(), 1, "only the finite point is kept");
        m.insert_points_via(&[p(f32::NAN, 0.0, 0.0)], &translation_pose([10.0, 0.0, 0.0]));
        assert_eq!(m.len(), 1, "non-finite source point skipped before transform");
    }

    #[test]
    fn revision_bumps_only_on_set_change_not_refresh() {
        let mut m = SceneVoxelMap::new(1.0, 10);
        assert_eq!(m.revision(), 0);
        m.insert_world([0.0, 0.0, 0.0]); // new cell → +1
        assert_eq!(m.revision(), 1);
        m.insert_world([0.0, 0.0, 0.0]); // refresh same cell → no change
        m.insert_world([0.0, 0.0, 0.0]);
        assert_eq!(m.revision(), 1, "pure refresh does not change the set");
        m.insert_world([5.0, 0.0, 0.0]); // new cell → +1
        assert_eq!(m.revision(), 2);
        let before = m.revision();
        m.clear(); // non-empty clear → +1
        assert_eq!(m.revision(), before + 1);
    }

    #[test]
    fn revision_bumps_on_eviction() {
        let mut m = SceneVoxelMap::new(1.0, 2);
        m.insert_world([0.0, 0.0, 0.0]);
        m.insert_world([1.0, 0.0, 0.0]);
        let r = m.revision();
        m.insert_world([2.0, 0.0, 0.0]); // overflow: +1 new cell AND +1 eviction
        assert!(m.revision() >= r + 2, "add + evict both change the set");
    }

    #[test]
    fn clear_resets() {
        let mut m = SceneVoxelMap::new(0.05, 1000);
        m.insert_world_points(&[p(0.0, 0.0, 0.0), p(1.0, 1.0, 1.0)]);
        assert_eq!(m.len(), 2);
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.to_packed_xyz().len(), 0);
    }
}
