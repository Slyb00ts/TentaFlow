// =============================================================================
// File: carve.rs — folding one device frame into the scene's occupancy.
// Purpose: turn "the sensor saw this" into evidence about the building
// (docs/SHARED_MAP_PLAN.md §3.2). Two halves that must stay together: the
// points say where a surface IS, and the space the beam crossed to get there
// says where a surface is NOT. Without the second half nothing ever leaves the
// map and a box carried out of the room stays in it forever.
//
// Where the points already live decides what a pose is for:
//   * `Odom` (Go2's `voxel_map_compressed`): the points arrive in the robot's
//     own odometry frame, so the placement alone puts them in the scene. The
//     pose is needed only to know WHERE the robot was, i.e. to carve and to draw
//     its marker — so a frame without a fresh pose still contributes surfaces,
//     it just cannot take anything away.
//   * `Sensor` (phone, depth camera): the points are in the sensor's frame, so
//     without a pose from the same instant they cannot be placed at all and the
//     frame is refused. Guessing with the last known pose is what smears a
//     corridor across a room.
//
// Exclusion volumes are applied BEFORE the hits and INSIDE the carving: a
// person must neither leave surfaces behind nor punch a hole in the wall they
// walked past, which is what happens when a body's depth error carves along the
// ray it blocked.
// =============================================================================

use std::collections::HashSet;

use crate::occupancy::{Cell, FrameDelta, OccupancyGrid};
use crate::pose::Pose;

/// Which frame a device's points arrive in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointsFrame {
    /// Already in the device's own odometry frame (Go2's fused window).
    Odom,
    /// In the sensor's frame, so `pose_at(timestamp)` is required.
    Sensor,
}

/// What kind of observation the frame is, and the parameters carving needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameKind {
    /// A pre-fused occupancy window around the device (Go2). Free space is not
    /// described by rays: what the window no longer contains is what is gone.
    PrefusedWindow { radius_m: f32 },
    /// Raw depth points. Free space is everything the ray crossed on its way.
    /// `miss_weight` is below 1 for monocular depth, whose range error grows
    /// with distance and must not delete walls at full strength.
    RawDepth { max_range_m: f32, miss_weight: f32 },
}

/// One frame from one device session.
#[derive(Debug, Clone)]
pub struct DeviceFrame {
    pub timestamp_us: i64,
    pub points_frame: PointsFrame,
    /// Sensor position in the frame the points are given in.
    pub origin_frame: [f32; 3],
    pub points: Vec<[f32; 3]>,
    pub kind: FrameKind,
}

/// Volumes whose contents are not scenery: device bodies and the frusta of
/// detected people, animals and vehicles (§4).
#[derive(Debug, Clone, Default)]
pub struct ExclusionSet {
    /// Axis-aligned boxes in the SCENE frame: `(min, max)`.
    pub boxes: Vec<([f32; 3], [f32; 3])>,
    /// `(centre, radius)` in the scene frame.
    pub spheres: Vec<([f32; 3], f32)>,
}

impl ExclusionSet {
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty() && self.spheres.is_empty()
    }

    pub fn contains(&self, p: [f32; 3]) -> bool {
        self.boxes.iter().any(|(min, max)| {
            (0..3).all(|i| p[i] >= min[i] && p[i] <= max[i])
        }) || self.spheres.iter().any(|(c, r)| {
            let (dx, dy, dz) = (p[0] - c[0], p[1] - c[1], p[2] - c[2]);
            dx * dx + dy * dy + dz * dz <= r * r
        })
    }
}

/// Why a frame did not reach the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarveError {
    /// `Sensor` points without a pose from the same moment.
    PoseMissing,
    /// The placement exists but the session was never placed in this scene.
    NotPlaced,
}

/// What a frame did, beyond the delta: the counters the telemetry shows and the
/// reason a map is not growing, when it is not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CarveStats {
    pub points_used: usize,
    pub points_excluded: usize,
    pub cells_hit: usize,
    pub cells_missed: usize,
    /// `Odom` frames that arrived without a usable pose: they added surfaces
    /// but could not carve.
    pub frames_without_pose: u32,
}

/// Per-device carving state that outlives one frame. The window strategy needs
/// the PREVIOUS window to know what disappeared, so this is the memory between
/// two frames of the same session — not scene state, and dropped with the
/// session.
#[derive(Debug, Clone, Default)]
pub struct DeviceCarveState {
    last_window_origin: Option<[f32; 3]>,
    last_window_radius: f32,
}

/// Folds one frame into `grid`.
///
/// `placement` maps the device session's frame into the scene; `pose_at` is the
/// device pose at the frame's own timestamp, in the device's odometry frame
/// (`None` when no pose is close enough in time).
pub fn fold_frame(
    grid: &mut OccupancyGrid,
    state: &mut DeviceCarveState,
    placement: &Pose,
    pose_at: Option<&Pose>,
    frame: &DeviceFrame,
    exclusions: &ExclusionSet,
) -> Result<(FrameDelta, CarveStats), CarveError> {
    let mut stats = CarveStats::default();
    if frame.points_frame == PointsFrame::Sensor && pose_at.is_none() {
        return Err(CarveError::PoseMissing);
    }
    let stamp = grid.begin_frame(frame.timestamp_us);
    let mut delta = FrameDelta::default();

    // Points → scene. For `Sensor` the pose puts them in the odometry frame
    // first; for `Odom` they are already there.
    let to_scene = match frame.points_frame {
        PointsFrame::Odom => placement.0,
        PointsFrame::Sensor => placement.0 * pose_at.expect("checked above").0,
    };
    let origin_scene = transform(&to_scene, frame.origin_frame);

    let mut hit_cells: HashSet<Cell> = HashSet::with_capacity(frame.points.len());
    for p in &frame.points {
        let scene_point = transform(&to_scene, *p);
        if exclusions.contains(scene_point) {
            stats.points_excluded += 1;
            continue;
        }
        stats.points_used += 1;
        let cell = grid.cell_of(scene_point);
        if hit_cells.insert(cell) {
            stats.cells_hit += 1;
        }
        if let Some(id) = grid.hit(stamp, cell, false) {
            delta.add_stable(id);
        }
    }

    let carve_origin = match frame.points_frame {
        // The window's centre is the robot, so its own pose is what says where
        // the free space was observed from. Without it we can add, not remove.
        PointsFrame::Odom => pose_at.map(|p| transform(&(placement.0 * p.0), [0.0, 0.0, 0.0])),
        PointsFrame::Sensor => Some(origin_scene),
    };
    let Some(carve_origin) = carve_origin else {
        stats.frames_without_pose = 1;
        return Ok((delta, stats));
    };

    match frame.kind {
        FrameKind::RawDepth {
            max_range_m,
            miss_weight,
        } => {
            for p in &frame.points {
                let scene_point = transform(&to_scene, *p);
                if exclusions.contains(scene_point) {
                    continue;
                }
                carve_ray(
                    grid,
                    stamp,
                    carve_origin,
                    scene_point,
                    max_range_m,
                    miss_weight,
                    &hit_cells,
                    exclusions,
                    &mut delta,
                    &mut stats,
                );
            }
        }
        FrameKind::PrefusedWindow { radius_m } => {
            carve_window(
                grid,
                stamp,
                carve_origin,
                radius_m,
                state,
                &hit_cells,
                exclusions,
                &mut delta,
                &mut stats,
            );
            state.last_window_origin = Some(carve_origin);
            state.last_window_radius = radius_m;
        }
    }
    Ok((delta, stats))
}

/// Amanatides–Woo traversal from the sensor to one measured point. Only cells
/// that already hold geometry are touched, and the walk stops at the first cell
/// this same frame reported as a surface — beyond that the beam was blocked, so
/// what lies behind was not observed at all.
#[allow(clippy::too_many_arguments)]
fn carve_ray(
    grid: &mut OccupancyGrid,
    stamp: crate::occupancy::FrameStamp,
    origin: [f32; 3],
    target: [f32; 3],
    max_range_m: f32,
    miss_weight: f32,
    hit_cells: &HashSet<Cell>,
    exclusions: &ExclusionSet,
    delta: &mut FrameDelta,
    stats: &mut CarveStats,
) {
    let res = grid.resolution();
    let dir = [
        target[0] - origin[0],
        target[1] - origin[1],
        target[2] - origin[2],
    ];
    let length = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if length <= res || !length.is_finite() {
        return;
    }
    let range = length.min(max_range_m);
    let unit = [dir[0] / length, dir[1] / length, dir[2] / length];

    let mut cell = grid.cell_of(origin);
    let end_cell = grid.cell_of(target);
    let mut t_max = [0.0f32; 3];
    let mut t_delta = [f32::INFINITY; 3];
    let mut step = [0i32; 3];
    for axis in 0..3 {
        if unit[axis] > 0.0 {
            step[axis] = 1;
            let next_boundary = (cell[axis] + 1) as f32 * res;
            t_max[axis] = (next_boundary - origin[axis]) / unit[axis];
            t_delta[axis] = res / unit[axis];
        } else if unit[axis] < 0.0 {
            step[axis] = -1;
            let next_boundary = cell[axis] as f32 * res;
            t_max[axis] = (next_boundary - origin[axis]) / unit[axis];
            t_delta[axis] = res / -unit[axis];
        }
    }

    // A ray of a few metres at 5 cm is ~100 cells; the bound is a guard against
    // a degenerate direction, not a policy.
    let max_steps = (range / res).ceil() as usize + 3;
    for _ in 0..max_steps {
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        if t_max[axis] > range {
            break;
        }
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
        if cell == end_cell || hit_cells.contains(&cell) {
            // The beam stopped here; everything behind is unobserved.
            break;
        }
        if exclusions.contains(grid.cell_center(cell)) {
            // Inside a body: the beam was blocked by something that is not
            // scenery, so it says nothing about the wall behind it.
            continue;
        }
        if let Some(id) = grid.miss(stamp, cell, origin, miss_weight) {
            delta.add_removed(id);
        }
        stats.cells_missed += 1;
    }
}

/// Window-diff carving for a pre-fused source: the device already decided what
/// is occupied inside its window, so a stable cell that lies inside BOTH the
/// previous and the current window and is absent from this frame is gone.
/// Restricting it to the overlap is what keeps a moving robot from deleting the
/// room it just left, which it can no longer see.
#[allow(clippy::too_many_arguments)]
fn carve_window(
    grid: &mut OccupancyGrid,
    stamp: crate::occupancy::FrameStamp,
    origin: [f32; 3],
    radius_m: f32,
    state: &DeviceCarveState,
    hit_cells: &HashSet<Cell>,
    exclusions: &ExclusionSet,
    delta: &mut FrameDelta,
    stats: &mut CarveStats,
) {
    let Some(previous) = state.last_window_origin else {
        // The first window of a session has nothing to compare against.
        return;
    };
    let previous_radius = state.last_window_radius;
    let res = grid.resolution();
    let reach = (radius_m / res).ceil() as i32;
    let centre = grid.cell_of(origin);
    for dz in -reach..=reach {
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let cell = [centre[0] + dx, centre[1] + dy, centre[2] + dz];
                if hit_cells.contains(&cell) || !grid.is_stable(cell) {
                    continue;
                }
                let p = grid.cell_center(cell);
                if distance(p, origin) > radius_m || distance(p, previous) > previous_radius {
                    // Outside the overlap: one of the two windows never looked
                    // here, so their disagreement is not evidence.
                    continue;
                }
                if exclusions.contains(p) {
                    continue;
                }
                if let Some(id) = grid.miss(stamp, cell, origin, 1.0) {
                    delta.add_removed(id);
                }
                stats.cells_missed += 1;
            }
        }
    }
}

fn transform(iso: &nalgebra::Isometry3<f64>, p: [f32; 3]) -> [f32; 3] {
    let v = iso * nalgebra::Point3::new(p[0] as f64, p[1] as f64, p[2] as f64);
    [v.x as f32, v.y as f32, v.z as f32]
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> OccupancyGrid {
        OccupancyGrid::new(0.05, 1_000_000)
    }

    fn depth_frame(time_us: i64, points: Vec<[f32; 3]>) -> DeviceFrame {
        DeviceFrame {
            timestamp_us: time_us,
            points_frame: PointsFrame::Sensor,
            origin_frame: [0.0, 0.0, 0.0],
            points,
            kind: FrameKind::RawDepth {
                max_range_m: 6.0,
                miss_weight: 1.0,
            },
        }
    }

    /// A depth frame taken from `sensor` that measures fixed WORLD points: the
    /// sensor-frame coordinates are the world points relative to it, so moving
    /// the sensor moves the viewpoint and not the wall.
    fn depth_frame_from(time_us: i64, sensor: [f64; 3], world: &[[f32; 3]]) -> DeviceFrame {
        DeviceFrame {
            timestamp_us: time_us,
            points_frame: PointsFrame::Sensor,
            origin_frame: [0.0, 0.0, 0.0],
            points: world
                .iter()
                .map(|p| {
                    [
                        p[0] - sensor[0] as f32,
                        p[1] - sensor[1] as f32,
                        p[2] - sensor[2] as f32,
                    ]
                })
                .collect(),
            kind: FrameKind::RawDepth {
                max_range_m: 6.0,
                miss_weight: 1.0,
            },
        }
    }

    /// Build a wall by looking at it until it is visible.
    fn see_wall(grid: &mut OccupancyGrid, point: [f32; 3], frames: usize) {
        let mut state = DeviceCarveState::default();
        let mut time = 0;
        for _ in 0..frames {
            fold_frame(
                grid,
                &mut state,
                &Pose::identity(),
                Some(&Pose::identity()),
                &depth_frame(time, vec![point]),
                &ExclusionSet::default(),
            )
            .unwrap();
            time += 500_000;
        }
    }

    /// Sensor-frame points without a pose are refused. The alternative — using
    /// the last known pose — is how a corridor gets smeared across a room.
    #[test]
    fn a_depth_frame_without_a_pose_is_refused() {
        let mut g = grid();
        let mut state = DeviceCarveState::default();
        let err = fold_frame(
            &mut g,
            &mut state,
            &Pose::identity(),
            None,
            &depth_frame(0, vec![[1.0, 0.0, 0.0]]),
            &ExclusionSet::default(),
        )
        .unwrap_err();
        assert_eq!(err, CarveError::PoseMissing);
        assert_eq!(g.materialized_cells(), 0);
    }

    /// An odometry-frame window without a pose still contributes surfaces; it
    /// just cannot carve, and says so.
    #[test]
    fn an_odom_frame_without_a_pose_adds_but_does_not_carve() {
        let mut g = grid();
        let mut state = DeviceCarveState::default();
        let frame = DeviceFrame {
            timestamp_us: 0,
            points_frame: PointsFrame::Odom,
            origin_frame: [0.0, 0.0, 0.0],
            points: vec![[1.0, 0.0, 0.0]],
            kind: FrameKind::PrefusedWindow { radius_m: 5.0 },
        };
        let (_, stats) = fold_frame(
            &mut g,
            &mut state,
            &Pose::identity(),
            None,
            &frame,
            &ExclusionSet::default(),
        )
        .unwrap();
        assert_eq!(stats.cells_hit, 1);
        assert_eq!(stats.cells_missed, 0);
        assert_eq!(stats.frames_without_pose, 1);
        assert_eq!(g.materialized_cells(), 1);
    }

    /// The ray stops at the surface it measured: a wall standing behind another
    /// wall is never carved by a beam that could not reach it.
    #[test]
    fn a_ray_stops_at_the_first_surface_and_spares_what_is_behind() {
        let mut g = grid();
        let near = [1.0, 0.0, 0.0];
        let far = [2.0, 0.0, 0.0];
        see_wall(&mut g, near, 4);
        see_wall(&mut g, far, 4);
        assert!(g.is_stable(g.cell_of(near)) && g.is_stable(g.cell_of(far)));
        let far_before = g.log_odds(g.cell_of(far));

        // Now measure only the near wall, many times, from two places.
        let mut state = DeviceCarveState::default();
        let mut time = 10_000_000;
        for i in 0..12 {
            let sensor = [f64::from(i % 2) * -0.5, 0.0, 0.0];
            let pose = Pose::from_parts(sensor, [0.0, 0.0, 0.0, 1.0]);
            fold_frame(
                &mut g,
                &mut state,
                &Pose::identity(),
                Some(&pose),
                &depth_frame_from(time, sensor, &[near]),
                &ExclusionSet::default(),
            )
            .unwrap();
            time += 200_000;
        }
        assert!(g.is_stable(g.cell_of(near)), "the measured wall stays");
        assert_eq!(
            g.log_odds(g.cell_of(far)),
            far_before,
            "the wall behind was never observed, so nothing may be concluded about it"
        );
    }

    /// Free space between the sensor and the surface is carved: a box removed
    /// from the middle of the room disappears from the map.
    #[test]
    fn what_stands_between_the_sensor_and_the_wall_is_carved_away() {
        let mut g = grid();
        let box_point = [1.0, 0.0, 0.0];
        let wall = [3.0, 0.0, 0.0];
        see_wall(&mut g, box_point, 4);
        see_wall(&mut g, wall, 4);
        assert!(g.is_stable(g.cell_of(box_point)));

        // The box is gone; only the wall answers now, from two viewpoints.
        let mut state = DeviceCarveState::default();
        let mut time = 10_000_000;
        let mut removed_reported = false;
        // Two viewpoints on the same sight line, half a metre apart: both look
        // straight through the box's cell, which a sideways step would not.
        for i in 0..14 {
            let sensor = [f64::from(i % 2) * -0.5, 0.0, 0.0];
            let pose = Pose::from_parts(sensor, [0.0, 0.0, 0.0, 1.0]);
            let (delta, _) = fold_frame(
                &mut g,
                &mut state,
                &Pose::identity(),
                Some(&pose),
                &depth_frame_from(time, sensor, &[wall]),
                &ExclusionSet::default(),
            )
            .unwrap();
            removed_reported |= !delta.is_empty()
                && delta
                    .chunks
                    .values()
                    .any(|c| !c.removed.is_empty());
            time += 200_000;
        }
        assert!(!g.is_stable(g.cell_of(box_point)), "the box must be gone");
        assert!(removed_reported, "its removal must reach the delta stream");
        assert!(g.is_stable(g.cell_of(wall)), "the wall must stay");
    }

    /// A person is neither added nor allowed to carve: the volume is excluded
    /// from the points AND from the ray, because the body's own depth error is
    /// what would otherwise eat the wall behind them.
    #[test]
    fn an_excluded_body_neither_adds_nor_carves() {
        let mut g = grid();
        let wall = [3.0, 0.0, 0.0];
        see_wall(&mut g, wall, 4);
        let wall_before = g.log_odds(g.cell_of(wall));

        let body = ExclusionSet {
            spheres: vec![([1.0, 0.0, 0.0], 0.5)],
            ..Default::default()
        };
        let mut state = DeviceCarveState::default();
        let (_, stats) = fold_frame(
            &mut g,
            &mut state,
            &Pose::identity(),
            Some(&Pose::identity()),
            &depth_frame(10_000_000, vec![[1.0, 0.0, 0.0], [1.05, 0.1, 0.0]]),
            &body,
        )
        .unwrap();
        assert_eq!(stats.points_used, 0);
        assert_eq!(stats.points_excluded, 2);
        assert_eq!(g.materialized_cells(), 1, "only the wall is in the map");
        assert_eq!(
            g.log_odds(g.cell_of(wall)),
            wall_before,
            "no ray was cast, so the wall's confidence is untouched"
        );
    }

    /// Window-diff only speaks about the overlap of the two windows: geometry
    /// the robot has driven away from is not deleted because it is out of view.
    #[test]
    fn a_window_only_removes_inside_the_overlap() {
        let mut g = grid();
        // A wall the robot saw from the start position.
        let behind = [-1.0, 0.0, 0.0];
        see_wall(&mut g, behind, 4);
        assert!(g.is_stable(g.cell_of(behind)));

        let mut state = DeviceCarveState::default();
        let mut time = 10_000_000;
        // Windows of radius 1 m, walking away from `behind` in +x.
        for i in 0..14 {
            let x = 1.0 + f64::from(i) * 0.2;
            let pose = Pose::from_parts([x, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
            let frame = DeviceFrame {
                timestamp_us: time,
                points_frame: PointsFrame::Odom,
                origin_frame: [0.0, 0.0, 0.0],
                points: vec![[x as f32 + 0.5, 0.0, 0.0]],
                kind: FrameKind::PrefusedWindow { radius_m: 1.0 },
            };
            fold_frame(
                &mut g,
                &mut state,
                &Pose::identity(),
                Some(&pose),
                &frame,
                &ExclusionSet::default(),
            )
            .unwrap();
            time += 200_000;
        }
        assert!(
            g.is_stable(g.cell_of(behind)),
            "a cell outside the window overlap must not be removed"
        );
    }
}
