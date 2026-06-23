// =============================================================================
// File: mapping.rs — submap sealing + pose-graph wiring (phase-0 chunk 0c).
// Purpose: turn the continuous LiDAR-odometry trajectory into the architecture's
// shared state: as the active submap travels far enough it is SEALED into a frozen
// `Submap`, added to the `Scene` with a pose-graph node (pose = its anchor) and an
// `Odometry` edge from the previous submap. Geometry is stored as frozen points in
// the submap's anchor-local frame; the TSDF representation swaps in behind the same
// sealed API once the SPATIAL voxel store lands (see submap.rs / SPATIAL_3D_PLAN).
// =============================================================================

use nalgebra::Matrix6;

use crate::graph::{
    Constraint, ConstraintId, ConstraintKind, ConstraintSource, ConstraintStatus, Scene,
};
use crate::lidar::{voxel_downsample, LioConfig, LioTracker, TrackResult};
use crate::pose::Pose;
use crate::submap::{LocalPoint, SubmapBuilder, SubmapId};

/// When to seal the active submap into a frozen one. Seal on EITHER enough travel,
/// enough rotation, or enough accumulated points — whichever comes first.
#[derive(Debug, Clone, Copy)]
pub struct SealPolicy {
    pub max_travel_m: f64,
    pub max_rot_rad: f64,
    pub max_points: usize,
}

impl Default for SealPolicy {
    fn default() -> Self {
        SealPolicy { max_travel_m: 3.0, max_rot_rad: 0.7, max_points: 100_000 }
    }
}

/// Result of feeding one scan: the tracker output + the id of a submap that sealed
/// on THIS scan (if any).
#[derive(Debug, Clone, Copy)]
pub struct MapStep {
    pub track: TrackResult,
    pub sealed: Option<SubmapId>,
}

/// Drives odometry → submaps → pose graph for ONE scene on ONE node. `origin_node`
/// stamps the coordination-free ids of the submaps + edges this node produces.
#[derive(Debug, Clone)]
pub struct MappingFrontend {
    tracker: LioTracker,
    scene: Scene,
    origin_node: u64,
    submap_voxel: f32,
    seal: SealPolicy,
    next_submap_seq: u32,
    next_edge_seq: u32,
    active_id: SubmapId,
    active_anchor: Pose,
    active_points: Vec<LocalPoint>,
    prev_submap: Option<SubmapId>,
    started: bool,
    /// Latest pose, set by EITHER the tracker (`process_scan`) or an external
    /// device pose (`ingest_posed`, option B). `current_pose()` returns this.
    current_pose: Pose,
}

impl MappingFrontend {
    pub fn new(
        origin_node: u64,
        scene_id: u64,
        lio: LioConfig,
        submap_voxel: f32,
        seal: SealPolicy,
    ) -> Self {
        MappingFrontend {
            tracker: LioTracker::new(lio),
            scene: Scene::new(scene_id),
            origin_node,
            submap_voxel,
            seal,
            next_submap_seq: 1,
            next_edge_seq: 0,
            active_id: SubmapId::new(origin_node, 0),
            active_anchor: Pose::identity(),
            active_points: Vec::new(),
            prev_submap: None,
            started: false,
            current_pose: Pose::identity(),
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn current_pose(&self) -> Pose {
        self.current_pose
    }

    pub fn active_submap_id(&self) -> SubmapId {
        self.active_id
    }

    pub fn pending_points(&self) -> usize {
        self.active_points.len()
    }

    /// Feed one sensor-frame scan. Tracks, accumulates geometry into the active
    /// submap (anchor-local), and seals when the policy fires.
    pub fn process_scan(&mut self, scan: &[nalgebra::Point3<f32>], prior: Option<Pose>) -> MapStep {
        let track = self.tracker.process_scan(scan, prior);

        if !self.started {
            self.active_anchor = track.pose;
            self.started = true;
        }
        self.current_pose = track.pose;

        // Only accumulate geometry from a SUCCESSFULLY tracked scan — a rejected
        // (degraded) scan must not pollute the frozen submap, mirroring the tracker
        // not folding it into the local map.
        if track.ok {
            self.accumulate(scan, track.pose);
        }

        let sealed = if self.should_seal(&track.pose) {
            Some(self.seal_active(track.pose))
        } else {
            None
        };

        MapStep { track, sealed }
    }

    /// Option B (UNIFIED_SLAM_ARCHITECTURE §15): adopt an EXTERNALLY supplied, trusted
    /// pose (e.g. a Go2's own `sportmodestate` odometry) WITHOUT running ICP — for a
    /// device that already self-localizes against its OWN fused map.
    ///
    /// It does NOT accumulate geometry: the device's `voxel_map_compressed` is its
    /// whole world-frame map re-sent every frame, so per-scan accumulation would
    /// duplicate it (and re-transforming already-world points would corrupt it). That
    /// map is already streamed to the renderer separately; ingesting it into the SLAM
    /// scene only matters for cross-robot fusion, handled in a later phase. Here we
    /// just adopt the pose → `GlobalPose`. Returns `ok` with `icp: None`.
    pub fn ingest_posed(&mut self, pose: Pose) -> MapStep {
        self.started = true;
        let delta = self.current_pose.relative_to(&pose);
        self.current_pose = pose;
        MapStep { track: TrackResult { pose, delta, icp: None, ok: true }, sealed: None }
    }

    /// Fold a sensor-frame scan into the active submap at `pose` (anchor-local).
    fn accumulate(&mut self, scan: &[nalgebra::Point3<f32>], pose: Pose) {
        let down = voxel_downsample(scan, self.submap_voxel);
        let to_local = self.active_anchor.inverse();
        for p in &down {
            let world = pose.transform_point([p.x as f64, p.y as f64, p.z as f64]);
            let loc = to_local.transform_point(world);
            self.active_points
                .push(LocalPoint::xyz(loc[0] as f32, loc[1] as f32, loc[2] as f32));
        }
    }

    fn should_seal(&self, current: &Pose) -> bool {
        if self.active_points.is_empty() {
            return false;
        }
        if self.active_points.len() >= self.seal.max_points {
            return true;
        }
        let (dt, da) = self.active_anchor.relative_to(current).magnitude();
        dt >= self.seal.max_travel_m || da >= self.seal.max_rot_rad
    }

    /// Freeze the active submap, register it + its pose node + an odometry edge from
    /// the previous submap, then open a new active submap anchored at `current`.
    fn seal_active(&mut self, current: Pose) -> SubmapId {
        let sealed_id = self.active_id;

        let mut builder = SubmapBuilder::new(sealed_id);
        builder.extend(self.active_points.drain(..));
        self.scene.insert_submap(builder.seal());
        // Pose node = the submap's anchor (submap→scene). Covariance is a placeholder
        // until the backend (chunk 0d) supplies real marginals.
        self.scene
            .graph
            .set_node(sealed_id, self.active_anchor, Matrix6::identity());

        // Odometry edge from the previous submap's anchor to this one.
        if let Some(prev) = self.prev_submap {
            if let Some(prev_node) = self.scene.graph.node(prev) {
                let relative = prev_node.pose.relative_to(&self.active_anchor);
                let edge = Constraint {
                    id: ConstraintId::new(self.origin_node, self.next_edge_seq),
                    kind: ConstraintKind::Odometry {
                        from: prev,
                        to: sealed_id,
                        relative,
                        information: Matrix6::identity(),
                    },
                    status: ConstraintStatus::Confirmed,
                    source: ConstraintSource::LidarOdometry,
                };
                self.scene.graph.add_constraint(edge);
                self.next_edge_seq += 1;
            }
        }

        // Open the next active submap at the current pose.
        self.prev_submap = Some(sealed_id);
        self.active_id = SubmapId::new(self.origin_node, self.next_submap_seq);
        self.next_submap_seq += 1;
        self.active_anchor = current;
        sealed_id
    }
}
