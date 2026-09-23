// =============================================================================
// File: reloc.rs — placing a device session inside a known scene.
// Purpose: answer "where in this building did the robot just wake up?"
// (docs/SHARED_MAP_PLAN.md §2.4). A new session has its own odometry frame, so
// its scans are consistent with each other but not with the stored map; this
// module finds `T_odom→scene` from the scans alone, and refuses when the answer
// is not unique.
//
// Two stages, because each fails where the other is strong:
//   1. A branch-and-bound correlative search over (x, y, yaw) on a 2-D slice of
//      the map (Hess et al., 2016 — Cartographer's fast scan matcher). It is
//      global: it finds the right basin anywhere in the search window, but only
//      to the resolution of its grid.
//   2. ICP from that pose. It is precise, but only converges from inside the
//      right basin — which is exactly what stage 1 delivers.
//
// A wrong placement is worse than none: every frame of the session would then
// be folded into the wrong place in a map other people rely on. So besides the
// fit quality there are two refusals that have nothing to do with how good the
// best answer looks — a second, clearly different answer almost as good (a
// long corridor, two identical rooms) means "ambiguous", and a single good
// answer must be confirmed by an independent one at least a second later.
// =============================================================================

use nalgebra::{Point3, UnitQuaternion};

use crate::lidar::{register, voxel_downsample, IcpConfig, VoxelMap};
use crate::occupancy::{FrameDelta, FrameStamp, OccupancyGrid};
use crate::pose::Pose;

#[derive(Debug, Clone)]
pub struct RelocConfig {
    /// Resolution of the finest search grid. The ICP stage recovers what this
    /// leaves on the table, so it only has to land inside ICP's basin.
    pub base_res_m: f32,
    pub yaw_step_deg: f32,
    /// Translation window around the prior; `None` in `relocalize` searches the
    /// whole scene.
    pub search_radius_m: f32,
    /// The slice both clouds are compared in, relative to each one's floor.
    /// Floor and ceiling are everywhere and say nothing about position.
    pub band_min_m: f32,
    pub band_max_m: f32,
    /// Fraction of scan points ICP must match.
    pub min_inlier_ratio: f32,
    /// Mean point distance of ICP inliers, metres.
    pub max_residual_m: f32,
    /// A distinct candidate scoring at least this fraction of the best makes the
    /// result ambiguous (0.7 = the runner-up must be 30 % worse).
    pub ambiguity_ratio: f32,
    /// Two candidates closer than this (in translation AND yaw) are the same
    /// answer, not a rival.
    pub distinct_m: f32,
    pub distinct_yaw_deg: f32,
    /// A confirmation must come from a frame at least this much later and land
    /// within these tolerances of the first answer.
    pub confirm_min_gap_us: i64,
    pub confirm_pos_m: f32,
    pub confirm_yaw_deg: f32,
}

impl Default for RelocConfig {
    fn default() -> Self {
        Self {
            base_res_m: 0.1,
            yaw_step_deg: 2.0,
            search_radius_m: 30.0,
            band_min_m: 0.2,
            band_max_m: 2.0,
            min_inlier_ratio: 0.6,
            max_residual_m: 0.1,
            ambiguity_ratio: 0.7,
            distinct_m: 1.0,
            distinct_yaw_deg: 15.0,
            confirm_min_gap_us: 1_000_000,
            confirm_pos_m: 0.2,
            confirm_yaw_deg: 3.0,
        }
    }
}

/// A placement proposal with the evidence behind it.
#[derive(Debug, Clone)]
pub struct RelocCandidate {
    pub placement: Pose,
    /// Fraction of slice points landing on occupied map cells at the search
    /// stage (before ICP).
    pub search_score: f32,
    pub inlier_ratio: f32,
    pub residual_m: f32,
}

#[derive(Debug, Clone)]
pub enum RelocOutcome {
    /// A unique, well-fitting answer. Still needs `ConfirmationGate` before it
    /// becomes a placement.
    Candidate(RelocCandidate),
    /// Two clearly different poses fit almost equally well.
    Ambiguous {
        best: RelocCandidate,
        runner_up_score: f32,
    },
    /// No answer good enough; the session stays `relocalizing`.
    Rejected(RejectReason),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RejectReason {
    /// Too few points in the comparison slice to say anything.
    ScanTooSparse,
    /// The scene has no visible geometry in the slice.
    MapEmpty,
    /// ICP matched too little of the scan.
    TooFewInliers { ratio: f32 },
    /// ICP matched, but loosely.
    ResidualTooHigh { residual_m: f32 },
}

/// Finds the placement of a scan (points in the device's odometry frame) in
/// `scene`. `prior` narrows the translation search to `search_radius_m` around
/// it; without one the whole scene is searched.
pub fn relocalize(
    scene: &OccupancyGrid,
    scan_odom: &[[f32; 3]],
    prior: Option<&Pose>,
    cfg: &RelocConfig,
) -> RelocOutcome {
    let map_points = scene.stable_points();
    let Some(map_floor) = floor_height(&map_points) else {
        return RelocOutcome::Rejected(RejectReason::MapEmpty);
    };
    let Some(scan_floor) = floor_height(scan_odom) else {
        return RelocOutcome::Rejected(RejectReason::ScanTooSparse);
    };
    let map_slice = slice(&map_points, map_floor, cfg);
    if map_slice.is_empty() {
        return RelocOutcome::Rejected(RejectReason::MapEmpty);
    }
    let scan_slice = dedup_2d(&slice(scan_odom, scan_floor, cfg), cfg.base_res_m);
    if scan_slice.len() < 20 {
        return RelocOutcome::Rejected(RejectReason::ScanTooSparse);
    }

    let grid = SliceGrid::build(&map_slice, cfg.base_res_m, cfg.search_radius_m);
    let window = match prior {
        Some(p) => {
            let t = p.translation();
            grid.window_around([t[0] as f32, t[1] as f32], cfg.search_radius_m)
        }
        None => grid.whole_window(&scan_slice),
    };
    let search = branch_and_bound(&grid, &scan_slice, window, cfg);
    let Some(best) = search.best else {
        return RelocOutcome::Rejected(RejectReason::MapEmpty);
    };
    let runner_up = search
        .contenders
        .iter()
        .filter(|c| !same_answer(c, &best, cfg))
        .map(|c| c.score)
        .fold(0.0f32, f32::max);

    let z_offset = map_floor - scan_floor;
    let coarse = pose_2d(&best, cfg.base_res_m, z_offset);
    let refined = refine(&map_points, scan_odom, &coarse, cfg.base_res_m);
    let candidate = RelocCandidate {
        placement: refined.pose,
        search_score: best.score,
        inlier_ratio: refined.inlier_ratio,
        residual_m: refined.residual_m,
    };
    if runner_up >= cfg.ambiguity_ratio * best.score {
        return RelocOutcome::Ambiguous {
            best: candidate,
            runner_up_score: runner_up,
        };
    }
    if candidate.inlier_ratio < cfg.min_inlier_ratio {
        return RelocOutcome::Rejected(RejectReason::TooFewInliers {
            ratio: candidate.inlier_ratio,
        });
    }
    if candidate.residual_m > cfg.max_residual_m {
        return RelocOutcome::Rejected(RejectReason::ResidualTooHigh {
            residual_m: candidate.residual_m,
        });
    }
    RelocOutcome::Candidate(candidate)
}

/// The second, independent confirmation a placement needs (§2.4): a candidate
/// becomes a placement only when a later frame, at least `confirm_min_gap_us`
/// after it, lands within tolerance. Both candidates are `T_odom→scene` of the
/// same session, which is constant while the robot moves — so they must agree
/// however far it drove in between.
#[derive(Debug, Clone, Default)]
pub struct ConfirmationGate {
    pending: Option<(Pose, i64)>,
}

impl ConfirmationGate {
    /// Offers a candidate observed at `time_us`. Returns the confirmed placement
    /// once two agree; a disagreeing candidate replaces the pending one, so a
    /// single outlier can delay a placement but never produce one.
    pub fn offer(&mut self, candidate: &Pose, time_us: i64, cfg: &RelocConfig) -> Option<Pose> {
        if let Some((pending, at)) = &self.pending {
            let gap_ok = time_us.saturating_sub(*at) >= cfg.confirm_min_gap_us;
            if gap_ok && poses_agree(pending, candidate, cfg.confirm_pos_m, cfg.confirm_yaw_deg) {
                let confirmed = candidate.clone();
                self.pending = None;
                return Some(confirmed);
            }
            if !gap_ok && poses_agree(pending, candidate, cfg.confirm_pos_m, cfg.confirm_yaw_deg)
            {
                // Too soon: the same frames' evidence is not independent. Keep
                // the older one so the gap keeps counting from it.
                return None;
            }
        }
        self.pending = Some((candidate.clone(), time_us));
        None
    }
}

/// Folds a provisional submap (built in a session's own odometry frame before
/// it was placed) into the scene through the placement it finally got.
pub fn merge_submap(
    scene: &mut OccupancyGrid,
    submap: &OccupancyGrid,
    placement: &Pose,
    time_us: i64,
) -> FrameDelta {
    let stamp: FrameStamp = scene.begin_frame(time_us);
    let mut delta = FrameDelta::default();
    for key in submap.chunk_keys() {
        let Some(chunk) = submap.chunk(key) else {
            continue;
        };
        for cell in chunk.persisted_cells() {
            let local = submap.cell_of_id(crate::occupancy::CellId {
                chunk: key,
                index: cell.index,
            });
            let p = submap.cell_center(local);
            let q = placement.0 * Point3::new(p[0] as f64, p[1] as f64, p[2] as f64);
            let target = scene.cell_of([q.x as f32, q.y as f32, q.z as f32]);
            let was_stable = cell.flags & crate::occupancy::flags::STABLE != 0;
            if let Some(id) = scene.merge_cell(stamp, target, cell.hits, cell.log_odds, was_stable)
            {
                delta.add_stable(id);
            }
        }
    }
    delta
}

// ---------------------------------------------------------------------------
// Branch and bound
// ---------------------------------------------------------------------------

/// A 2-D occupancy slice of the map plus its max-pooled pyramid. Level `k`
/// stores, at every cell, the maximum over the `2^k × 2^k` block starting
/// there — so the score of a level-`k` node is an upper bound on every
/// translation it covers, which is what makes pruning exact.
struct SliceGrid {
    origin: [f32; 2],
    res: f32,
    w: i32,
    h: i32,
    levels: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

#[derive(Debug, Clone, Copy)]
struct Leaf {
    /// Translation in grid cells and the yaw it was scored at.
    x: f32,
    y: f32,
    yaw: f32,
    score: f32,
}

struct SearchResult {
    best: Option<Leaf>,
    /// Every leaf that scored at least `ambiguity_ratio × best` — the only ones
    /// the ambiguity decision needs.
    contenders: Vec<Leaf>,
    #[cfg_attr(not(test), allow(dead_code))]
    leaves_scored: usize,
}

impl SliceGrid {
    fn build(points: &[[f32; 2]], res: f32, margin_m: f32) -> Self {
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in points {
            for a in 0..2 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        // Margin so that a scan hanging off the edge of the map still has cells
        // to score (as zeros) instead of being clipped into a false match.
        let margin = margin_m.min(5.0);
        let origin = [min[0] - margin, min[1] - margin];
        let w = ((max[0] - origin[0] + margin) / res).ceil() as i32 + 1;
        let h = ((max[1] - origin[1] + margin) / res).ceil() as i32 + 1;
        let mut base = vec![0u8; (w * h) as usize];
        for p in points {
            let cx = ((p[0] - origin[0]) / res).floor() as i32;
            let cy = ((p[1] - origin[1]) / res).floor() as i32;
            if (0..w).contains(&cx) && (0..h).contains(&cy) {
                base[(cy * w + cx) as usize] = 1;
            }
        }
        let depth = (w.max(h) as f32).log2().ceil() as usize + 1;
        let mut levels = vec![base];
        for k in 1..depth {
            let step = 1i32 << (k - 1);
            let prev = &levels[k - 1];
            let mut next = vec![0u8; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let mut m = prev[(y * w + x) as usize];
                    for (dx, dy) in [(step, 0), (0, step), (step, step)] {
                        let (xx, yy) = (x + dx, y + dy);
                        if xx < w && yy < h {
                            m = m.max(prev[(yy * w + xx) as usize]);
                        }
                    }
                    next[(y * w + x) as usize] = m;
                }
            }
            levels.push(next);
        }
        Self {
            origin,
            res,
            w,
            h,
            levels,
        }
    }

    /// Translations within `radius_m` of a prior translation. Translations are
    /// counted in cells of `scene = R·odom + t`, so the grid origin plays no part.
    fn window_around(&self, centre: [f32; 2], radius_m: f32) -> Window {
        let cx = (centre[0] / self.res).floor() as i32;
        let cy = (centre[1] / self.res).floor() as i32;
        let r = (radius_m / self.res).ceil() as i32;
        Window {
            x0: cx - r,
            y0: cy - r,
            x1: cx + r,
            y1: cy + r,
        }
    }

    /// Every translation that can put the scan's centroid on the map under any
    /// rotation. The rotated centroid lies on a circle of radius |c| around the
    /// odometry origin, so the window is the map widened by that radius.
    fn whole_window(&self, scan: &[[f32; 2]]) -> Window {
        let n = scan.len().max(1) as f32;
        let c = scan
            .iter()
            .fold([0.0f32; 2], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
        let r = (c[0] * c[0] + c[1] * c[1]).sqrt();
        let to_cells = |m: f32| (m / self.res).floor() as i32;
        Window {
            x0: to_cells(self.origin[0] - r),
            y0: to_cells(self.origin[1] - r),
            x1: to_cells(self.origin[0] + r) + self.w,
            y1: to_cells(self.origin[1] + r) + self.h,
        }
    }

    /// Pyramid value with the block clipped to the grid. A block that starts
    /// left of the grid but reaches into it must still bound the cells it
    /// covers, so it is read at the edge: that block is a superset of the
    /// clipped one, which keeps the value an upper bound — returning 0 there
    /// would prune the true optimum whenever a scan hangs off the map.
    #[inline]
    fn value(&self, level: usize, mut x: i32, mut y: i32) -> u8 {
        let span = 1i32 << level;
        if x >= self.w || y >= self.h || x + span <= 0 || y + span <= 0 {
            return 0;
        }
        x = x.max(0);
        y = y.max(0);
        self.levels[level][(y * self.w + x) as usize]
    }

    /// Score of a node: points whose cell (after translation) is occupied at
    /// this level, as a fraction of all points.
    fn score(&self, level: usize, cells: &[(i32, i32)], dx: i32, dy: i32) -> f32 {
        let hits: u32 = cells
            .iter()
            .map(|(x, y)| u32::from(self.value(level, x + dx, y + dy)))
            .sum();
        hits as f32 / cells.len().max(1) as f32
    }
}

fn branch_and_bound(
    grid: &SliceGrid,
    scan: &[[f32; 2]],
    window: Window,
    cfg: &RelocConfig,
) -> SearchResult {
    let yaws = yaw_candidates(cfg.yaw_step_deg);
    let top = grid.levels.len() - 1;
    let top_step = 1i32 << top;

    // Node: (yaw index, x, y, level, bound).
    let mut stack: Vec<(usize, i32, i32, usize, f32)> = Vec::new();
    let rotated: Vec<Vec<(i32, i32)>> = yaws.iter().map(|y| scan_cells(grid, scan, *y)).collect();
    let mut roots = Vec::new();
    for (yi, cells) in rotated.iter().enumerate() {
        let mut y = window.y0;
        while y <= window.y1 {
            let mut x = window.x0;
            while x <= window.x1 {
                roots.push((yi, x, y, top, grid.score(top, cells, x, y)));
                x += top_step;
            }
            y += top_step;
        }
    }
    roots.sort_by(|a, b| a.4.total_cmp(&b.4));
    stack.extend(roots);

    let mut best: Option<Leaf> = None;
    let mut contenders: Vec<Leaf> = Vec::new();
    let mut leaves_scored = 0usize;
    while let Some((yi, x, y, level, bound)) = stack.pop() {
        let threshold = best.map(|b| b.score * cfg.ambiguity_ratio).unwrap_or(0.0);
        // Nothing below the ambiguity threshold can either win or make the
        // result ambiguous, so it is safe to drop — and a zero bound never is
        // an answer.
        if bound <= 0.0 || bound < threshold {
            continue;
        }
        if level == 0 {
            leaves_scored += 1;
            let leaf = Leaf {
                x: x as f32,
                y: y as f32,
                yaw: yaws[yi],
                score: bound,
            };
            if best.is_none_or(|b| leaf.score > b.score) {
                best = Some(leaf);
                let floor = leaf.score * cfg.ambiguity_ratio;
                contenders.retain(|c| c.score >= floor);
            }
            contenders.push(leaf);
            continue;
        }
        let step = 1i32 << (level - 1);
        let mut children = Vec::with_capacity(4);
        for (dx, dy) in [(0, 0), (step, 0), (0, step), (step, step)] {
            let (cx, cy) = (x + dx, y + dy);
            if cx > window.x1 || cy > window.y1 {
                continue;
            }
            children.push((yi, cx, cy, level - 1, grid.score(level - 1, &rotated[yi], cx, cy)));
        }
        // Ascending so the most promising child is popped first: a good best
        // early is what makes the rest of the tree prunable.
        children.sort_by(|a, b| a.4.total_cmp(&b.4));
        stack.extend(children);
    }
    SearchResult {
        best,
        contenders,
        leaves_scored,
    }
}

/// Scan points rotated by `yaw` and expressed as grid cells relative to the map
/// origin, for a zero translation. An integer translation then shifts every
/// cell by the same amount, so the rotation is paid once per yaw.
fn scan_cells(grid: &SliceGrid, scan: &[[f32; 2]], yaw: f32) -> Vec<(i32, i32)> {
    let (s, c) = yaw.sin_cos();
    scan.iter()
        .map(|p| {
            let rx = c * p[0] - s * p[1];
            let ry = s * p[0] + c * p[1];
            (
                ((rx - grid.origin[0]) / grid.res).floor() as i32,
                ((ry - grid.origin[1]) / grid.res).floor() as i32,
            )
        })
        .collect()
}

fn yaw_candidates(step_deg: f32) -> Vec<f32> {
    let n = (360.0 / step_deg).round().max(1.0) as usize;
    (0..n)
        .map(|i| (i as f32 * step_deg).to_radians())
        .collect()
}

fn same_answer(a: &Leaf, b: &Leaf, cfg: &RelocConfig) -> bool {
    let dist = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() * cfg.base_res_m;
    let dyaw = angle_diff(a.yaw, b.yaw).to_degrees().abs();
    dist < cfg.distinct_m && dyaw < cfg.distinct_yaw_deg
}

/// Leaf → metric pose. The scan's cells already had the grid origin
/// subtracted (`scan_cells`), and a map cell holds points at the same offset, so
/// a match at a translation of `k` cells means `scene = R·odom + k·res`.
fn pose_2d(leaf: &Leaf, res: f32, z: f32) -> Pose {
    let half = leaf.yaw as f64 / 2.0;
    Pose::from_parts(
        [
            f64::from(leaf.x * res),
            f64::from(leaf.y * res),
            f64::from(z),
        ],
        [0.0, 0.0, half.sin(), half.cos()],
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct Refined {
    pose: Pose,
    inlier_ratio: f32,
    residual_m: f32,
}

fn refine(map_points: &[[f32; 3]], scan_odom: &[[f32; 3]], init: &Pose, res: f32) -> Refined {
    let mut map = VoxelMap::new(0.1, 16);
    map.add_points(map_points.iter().map(|p| Point3::new(p[0], p[1], p[2])));
    // A session buffer holds seconds of dense scans; at the search resolution
    // the fit is the same and the cost is a fraction.
    let raw: Vec<Point3<f32>> = scan_odom
        .iter()
        .map(|p| Point3::new(p[0], p[1], p[2]))
        .collect();
    let source = voxel_downsample(&raw, res);
    let cfg = IcpConfig {
        max_iters: 40,
        max_corr_dist: 0.3,
        ..IcpConfig::default()
    };
    let result = register(&source, &map, init.clone(), &cfg);
    Refined {
        pose: result.pose,
        inlier_ratio: result.inliers as f32 / source.len().max(1) as f32,
        residual_m: result.mean_residual as f32,
    }
}

/// Height of the floor: the 5th percentile of z. A percentile rather than the
/// minimum so a single point under the floor (a reflection, a stair) does not
/// shift the whole slice.
fn floor_height(points: &[[f32; 3]]) -> Option<f32> {
    if points.len() < 20 {
        return None;
    }
    let mut z: Vec<f32> = points.iter().map(|p| p[2]).collect();
    z.sort_by(|a, b| a.total_cmp(b));
    Some(z[z.len() / 20])
}

fn slice(points: &[[f32; 3]], floor: f32, cfg: &RelocConfig) -> Vec<[f32; 2]> {
    points
        .iter()
        .filter(|p| p[2] >= floor + cfg.band_min_m && p[2] <= floor + cfg.band_max_m)
        .map(|p| [p[0], p[1]])
        .collect()
}

/// One point per grid cell: a wall seen from close up would otherwise outvote
/// the rest of the room.
fn dedup_2d(points: &[[f32; 2]], res: f32) -> Vec<[f32; 2]> {
    let mut seen = std::collections::HashSet::new();
    points
        .iter()
        .filter(|p| seen.insert(((p[0] / res).floor() as i32, (p[1] / res).floor() as i32)))
        .copied()
        .collect()
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let mut d = a - b;
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    d
}

fn poses_agree(a: &Pose, b: &Pose, pos_m: f32, yaw_deg: f32) -> bool {
    let (ta, tb) = (a.translation(), b.translation());
    let dist = ((ta[0] - tb[0]).powi(2) + (ta[1] - tb[1]).powi(2) + (ta[2] - tb[2]).powi(2)).sqrt();
    let rel: UnitQuaternion<f64> = a.0.rotation.inverse() * b.0.rotation;
    dist <= f64::from(pos_m) && rel.angle().to_degrees() <= f64::from(yaw_deg)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RES: f32 = 0.05;

    /// Points of a vertical wall from `a` to `b` (XY), floor to `height`.
    fn wall(a: [f32; 2], b: [f32; 2], height: f32, step: f32) -> Vec<[f32; 3]> {
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let n = (len / step).ceil() as usize;
        let mut out = Vec::new();
        for i in 0..=n {
            let t = i as f32 / n.max(1) as f32;
            let (x, y) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
            let mut z = 0.0;
            while z <= height {
                out.push([x, y, z]);
                z += step;
            }
        }
        out
    }

    /// A floor patch, so each cloud's floor estimate has something to find.
    fn floor(x: [f32; 2], y: [f32; 2], step: f32) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        let mut px = x[0];
        while px <= x[1] {
            let mut py = y[0];
            while py <= y[1] {
                out.push([px, py, 0.0]);
                py += step;
            }
            px += step;
        }
        out
    }

    /// An L-shaped room (8 × 6 m with a 3 × 3 m corner missing) plus a box.
    /// The shape itself breaks the symmetry: a rectangle is its own 180°
    /// rotation, and the first version of this fixture — a rectangle with a
    /// short inner wall — was rightly reported ambiguous (runner-up at 86 % of
    /// the best), which is the behaviour a real rectangular room must get.
    fn room(step: f32) -> Vec<[f32; 3]> {
        let mut p = Vec::new();
        p.extend(wall([0.0, 0.0], [8.0, 0.0], 2.2, step));
        p.extend(wall([8.0, 0.0], [8.0, 3.0], 2.2, step));
        p.extend(wall([8.0, 3.0], [5.0, 3.0], 2.2, step));
        p.extend(wall([5.0, 3.0], [5.0, 6.0], 2.2, step));
        p.extend(wall([5.0, 6.0], [0.0, 6.0], 2.2, step));
        p.extend(wall([0.0, 6.0], [0.0, 0.0], 2.2, step));
        p.extend(wall([1.5, 1.0], [2.7, 1.0], 1.0, step));
        p.extend(wall([2.7, 1.0], [2.7, 1.7], 1.0, step));
        p.extend(floor([0.2, 7.8], [0.2, 2.8], 0.2));
        p.extend(floor([0.2, 4.8], [3.0, 5.8], 0.2));
        p
    }

    fn scene_of(points: &[[f32; 3]]) -> OccupancyGrid {
        let mut g = OccupancyGrid::new(RES, 10_000_000);
        let stamp = g.begin_frame(0);
        for p in points {
            let cell = g.cell_of(*p);
            g.merge_cell(stamp, cell, 5, 100, true);
        }
        g
    }

    fn yaw_pose(t: [f64; 3], yaw_deg: f64) -> Pose {
        let half = yaw_deg.to_radians() / 2.0;
        Pose::from_parts(t, [0.0, 0.0, half.sin(), half.cos()])
    }

    /// Scene points seen from a session whose odometry frame is `placement`
    /// away from the scene: `odom = placement⁻¹ · scene`.
    fn as_odom(points: &[[f32; 3]], placement: &Pose) -> Vec<[f32; 3]> {
        let inv = placement.0.inverse();
        points
            .iter()
            .map(|p| {
                let q = inv * Point3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                [q.x as f32, q.y as f32, q.z as f32]
            })
            .collect()
    }

    fn yaw_of(p: &Pose) -> f64 {
        p.0.rotation.euler_angles().2.to_degrees()
    }

    /// The robot woke up 1.3 m / -0.7 m / 37° away from where the map was
    /// started: search + ICP must put it back within 5 cm and 1°.
    #[test]
    fn a_scan_is_placed_in_a_known_room_to_within_5cm() {
        let geometry = room(RES);
        let scene = scene_of(&geometry);
        let truth = yaw_pose([1.3, -0.7, 0.0], 37.0);
        let scan = as_odom(&room(0.1), &truth);

        let outcome = relocalize(&scene, &scan, None, &RelocConfig::default());
        let RelocOutcome::Candidate(c) = outcome else {
            panic!("expected a candidate, got {outcome:?}");
        };
        let (got, want) = (c.placement.translation(), truth.translation());
        let err = ((got[0] - want[0]).powi(2) + (got[1] - want[1]).powi(2)).sqrt();
        assert!(err < 0.05, "translation error {err:.3} m");
        let yaw_err = (yaw_of(&c.placement) - 37.0).abs();
        assert!(yaw_err < 1.0, "yaw error {yaw_err:.2}°");
        assert!(c.inlier_ratio >= 0.6, "inlier ratio {}", c.inlier_ratio);
    }

    /// Branch and bound must not lose the optimum: on the same discrete grid
    /// it returns the score an exhaustive search finds, while scoring far
    /// fewer leaves.
    #[test]
    fn branch_and_bound_finds_the_exhaustive_optimum() {
        let cfg = RelocConfig {
            yaw_step_deg: 10.0,
            ..RelocConfig::default()
        };
        let map = slice(&room(RES), 0.0, &cfg);
        let truth = yaw_pose([0.6, 0.4, 0.0], 20.0);
        let scan3 = as_odom(&room(0.1), &truth);
        let scan = dedup_2d(&slice(&scan3, 0.0, &cfg), cfg.base_res_m);
        let grid = SliceGrid::build(&map, cfg.base_res_m, 2.0);
        let window = Window {
            x0: -20,
            y0: -20,
            x1: 20,
            y1: 20,
        };

        let bnb = branch_and_bound(&grid, &scan, window, &cfg);
        let mut exhaustive = 0.0f32;
        let mut leaves = 0usize;
        for yaw in yaw_candidates(cfg.yaw_step_deg) {
            let cells = scan_cells(&grid, &scan, yaw);
            for y in window.y0..=window.y1 {
                for x in window.x0..=window.x1 {
                    exhaustive = exhaustive.max(grid.score(0, &cells, x, y));
                    leaves += 1;
                }
            }
        }
        let best = bnb.best.expect("a best leaf");
        assert!(
            (best.score - exhaustive).abs() < 1e-6,
            "bnb {} vs exhaustive {}",
            best.score,
            exhaustive
        );
        assert!(
            bnb.leaves_scored * 10 < leaves,
            "bnb scored {} of {} leaves — it is not pruning",
            bnb.leaves_scored,
            leaves
        );
    }

    /// A long corridor looks the same a metre further on. The answer is
    /// "ambiguous", never a confident wrong placement.
    #[test]
    fn a_featureless_corridor_is_ambiguous() {
        let mut corridor = Vec::new();
        corridor.extend(wall([0.0, 0.0], [30.0, 0.0], 2.2, RES));
        corridor.extend(wall([0.0, 2.0], [30.0, 2.0], 2.2, RES));
        corridor.extend(floor([0.0, 30.0], [0.2, 1.8], 0.2));
        let scene = scene_of(&corridor);

        let mut piece = Vec::new();
        piece.extend(wall([10.0, 0.0], [16.0, 0.0], 2.2, 0.1));
        piece.extend(wall([10.0, 2.0], [16.0, 2.0], 2.2, 0.1));
        piece.extend(floor([10.0, 16.0], [0.2, 1.8], 0.2));
        let scan = as_odom(&piece, &yaw_pose([3.0, 0.0, 0.0], 0.0));

        let outcome = relocalize(&scene, &scan, None, &RelocConfig::default());
        assert!(
            matches!(outcome, RelocOutcome::Ambiguous { .. }),
            "expected ambiguous, got {outcome:?}"
        );
    }

    /// One good answer is not a placement: a second one, at least a second
    /// later and in agreement, is. A disagreeing one resets the wait.
    #[test]
    fn a_placement_needs_an_agreeing_second_answer_a_second_later() {
        let cfg = RelocConfig::default();
        let mut gate = ConfirmationGate::default();
        let a = yaw_pose([2.0, 1.0, 0.0], 30.0);
        let close = yaw_pose([2.05, 1.02, 0.0], 31.0);
        let far = yaw_pose([4.0, 1.0, 0.0], 30.0);

        assert!(gate.offer(&a, 0, &cfg).is_none(), "one answer is never enough");
        assert!(
            gate.offer(&close, 300_000, &cfg).is_none(),
            "too soon to be independent evidence"
        );
        assert!(
            gate.offer(&far, 1_500_000, &cfg).is_none(),
            "a disagreeing answer must not confirm"
        );
        assert!(gate.offer(&close, 2_000_000, &cfg).is_none(), "the wait restarted at `far`");
        let confirmed = gate
            .offer(&close, 3_200_000, &cfg)
            .expect("two agreeing answers a second apart");
        assert!(poses_agree(&confirmed, &close, 0.01, 0.1));
    }

    /// A session that started outside the map built its own submap; once it is
    /// placed, that geometry lands in the scene where the placement says, and
    /// what was already visible stays visible.
    #[test]
    fn a_placed_submap_merges_into_the_scene() {
        let mut scene = OccupancyGrid::new(RES, 1_000_000);
        let submap = scene_of(&wall([0.0, 0.0], [1.0, 0.0], 0.5, RES));
        let placement = yaw_pose([3.0, 2.0, 0.0], 90.0);

        let delta = merge_submap(&mut scene, &submap, &placement, 5_000_000);
        assert!(!delta.is_empty());
        // The submap cell centred at (0.525, 0.025, 0.275) → rotated 90° and
        // shifted → (2.975, 2.525, 0.275).
        assert!(scene.is_stable(scene.cell_of([2.975, 2.525, 0.275])));
        // Where it would be without the rotation: nothing.
        assert!(!scene.is_stable(scene.cell_of([3.525, 2.025, 0.275])));
        assert_eq!(
            scene.materialized_cells(),
            submap.materialized_cells(),
            "every submap cell lands exactly once"
        );
    }

    #[test]
    fn an_empty_scene_rejects_instead_of_guessing() {
        let scene = OccupancyGrid::new(RES, 1_000);
        let outcome = relocalize(&scene, &room(0.1), None, &RelocConfig::default());
        assert!(matches!(outcome, RelocOutcome::Rejected(RejectReason::MapEmpty)));
    }
}
