// =============================================================================
// File: lib.rs — tentaflow-slam crate root (unified SLAM core).
// Purpose: the shared data model for spatial + positioning as ONE SLAM loop
// (docs/UNIFIED_SLAM_ARCHITECTURE.md). Chunk 0a: frozen submaps + pose graph +
// scene, with the correctness invariants that later chunks (LIO, TSDF, backend,
// mesh) build on. Pure Rust, no GPU, off-device testable.
// =============================================================================

#![forbid(unsafe_code)]

pub mod graph;
pub mod lidar;
pub mod mapping;
pub mod pose;
pub mod submap;

pub use graph::{
    Constraint, ConstraintId, ConstraintKind, ConstraintSource, ConstraintStatus, PoseGraph,
    PoseNode, Scene,
};
pub use mapping::{MapStep, MappingFrontend, SealPolicy};
pub use lidar::{
    register, voxel_downsample, IcpConfig, IcpResult, LioConfig, LioTracker, TrackResult, VoxelMap,
};
pub use pose::{rotation_pose, translation_pose, Pose};
pub use submap::{LocalPoint, Submap, SubmapBuilder, SubmapGeometry, SubmapId};

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use nalgebra::{Matrix3, Matrix6};
    use std::f64::consts::FRAC_PI_2;

    // ---- pose algebra ----

    #[test]
    fn pose_relative_then_round_trips() {
        let a = translation_pose([1.0, 2.0, 3.0]);
        let b = rotation_pose([0.0, 0.0, 1.0], FRAC_PI_2).compose(&translation_pose([5.0, 0.0, 0.0]));
        // a.compose(a→b) == b
        let rel = a.relative_to(&b);
        let recomposed = a.compose(&rel);
        let (dt, da) = recomposed.relative_to(&b).magnitude();
        assert_relative_eq!(dt, 0.0, epsilon = 1e-9);
        assert_relative_eq!(da, 0.0, epsilon = 1e-9);
    }

    #[test]
    fn pose_inverse_is_identity() {
        let p = rotation_pose([1.0, 1.0, 0.0], 0.7).compose(&translation_pose([2.0, -1.0, 4.0]));
        let id = p.compose(&p.inverse());
        let (dt, da) = id.magnitude();
        assert_relative_eq!(dt, 0.0, epsilon = 1e-9);
        assert_relative_eq!(da, 0.0, epsilon = 1e-9);
    }

    #[test]
    fn pose_transform_point_matches_translation() {
        let p = translation_pose([10.0, 0.0, -5.0]);
        assert_eq!(p.transform_point([1.0, 2.0, 3.0]), [11.0, 2.0, -2.0]);
    }

    // ---- frozen submaps ----

    #[test]
    fn submap_seals_geometry_and_projects() {
        let mut b = SubmapBuilder::new(SubmapId::new(1, 0));
        b.push(LocalPoint::xyz(1.0, 0.0, 0.0));
        b.push(LocalPoint::xyz_rgb(0.0, 2.0, 0.0, [255, 0, 0]));
        assert_eq!(b.len(), 2);
        let sm = b.seal();
        // Sealed geometry is intact and immutable (no &mut API exists on Submap).
        assert_eq!(sm.point_count(), 2);
        assert_eq!(sm.geometry().points()[1].color, Some([255, 0, 0]));
        // points_in applies the submap pose without mutating the frozen geometry.
        let world = sm.points_in(&translation_pose([100.0, 0.0, 0.0]));
        assert_eq!(world[0], [101.0, 0.0, 0.0]);
        assert_eq!(sm.point_count(), 2, "projection must not consume geometry");
    }

    #[test]
    fn submap_clone_shares_frozen_geometry() {
        let mut b = SubmapBuilder::new(SubmapId::new(7, 3));
        b.extend((0..100).map(|i| LocalPoint::xyz(i as f32, 0.0, 0.0)));
        let sm = b.seal();
        let sm2 = sm.clone();
        // Arc-shared: cloning is O(1) and both see the same frozen bytes.
        assert!(std::sync::Arc::ptr_eq(sm.geometry(), sm2.geometry()));
    }

    // ---- constraints: add-only, idempotent, gated ----

    fn odom(id: u32, from: SubmapId, to: SubmapId) -> Constraint {
        Constraint {
            id: ConstraintId::new(0, id),
            kind: ConstraintKind::Odometry {
                from,
                to,
                relative: translation_pose([1.0, 0.0, 0.0]),
                information: Matrix6::identity(),
            },
            status: ConstraintStatus::Confirmed,
            source: ConstraintSource::LidarOdometry,
        }
    }

    #[test]
    fn constraints_are_add_only_idempotent() {
        let mut g = PoseGraph::new();
        let a = SubmapId::new(0, 0);
        let b = SubmapId::new(0, 1);
        g.ensure_node(a);
        g.ensure_node(b);
        assert!(g.add_constraint(odom(0, a, b)));
        // Same id again → rejected as a duplicate (mesh-merge safety).
        assert!(!g.add_constraint(odom(0, a, b)));
        assert_eq!(g.constraint_count(), 1);
    }

    #[test]
    fn loop_closure_gating_excludes_until_confirmed() {
        let mut g = PoseGraph::new();
        let a = SubmapId::new(0, 0);
        let b = SubmapId::new(0, 5);
        g.ensure_node(a);
        g.ensure_node(b);
        g.add_constraint(odom(0, a, b)); // confirmed odometry
        let lc = Constraint {
            id: ConstraintId::new(0, 99),
            kind: ConstraintKind::LoopClosure {
                from: b,
                to: a,
                relative: Pose::identity(),
                information: Matrix6::identity(),
            },
            status: ConstraintStatus::Candidate,
            source: ConstraintSource::LidarLoopClosure,
        };
        g.add_constraint(lc);
        // Candidate loop closure is NOT active (gating §5).
        assert_eq!(g.active_constraints().count(), 1);
        // Promote → now active.
        assert!(g.set_status(ConstraintId::new(0, 99), ConstraintStatus::Confirmed));
        assert_eq!(g.active_constraints().count(), 2);
        // Reject → excluded again, geometry never involved.
        assert!(g.set_status(ConstraintId::new(0, 99), ConstraintStatus::Rejected));
        assert_eq!(g.active_constraints().count(), 1);
    }

    // ---- deterministic gauge (mesh convergence backstop) ----

    #[test]
    fn gauge_prefers_georeferenced_submap_else_lowest_id() {
        let mut g = PoseGraph::new();
        let lo = SubmapId::new(1, 0);
        let hi = SubmapId::new(9, 0);
        g.ensure_node(hi);
        g.ensure_node(lo);
        // No anchor yet → lowest id is the gauge.
        assert_eq!(g.gauge_anchor(), Some(lo));
        // Add a CONFIRMED georef on the higher-id submap → it becomes the gauge.
        g.add_constraint(Constraint {
            id: ConstraintId::new(0, 1),
            kind: ConstraintKind::Georef {
                submap: hi,
                submap_to_ecef: Pose::identity(),
                information: Matrix6::identity(),
            },
            status: ConstraintStatus::Confirmed,
            source: ConstraintSource::Georef,
        });
        assert_eq!(g.gauge_anchor(), Some(hi));
    }

    #[test]
    fn gauge_is_insertion_order_independent() {
        // Two nodes build the same scene with submaps inserted in OPPOSITE orders;
        // the deterministic gauge + node set must be identical (mesh convergence).
        let ids = [SubmapId::new(3, 1), SubmapId::new(1, 4), SubmapId::new(2, 2)];
        let mut a = Scene::new(42);
        let mut b = Scene::new(42);
        for &id in ids.iter() {
            a.insert_submap(SubmapBuilder::new(id).seal());
        }
        for &id in ids.iter().rev() {
            b.insert_submap(SubmapBuilder::new(id).seal());
        }
        assert_eq!(a.graph.gauge_anchor(), b.graph.gauge_anchor());
        assert_eq!(a.graph.gauge_anchor(), Some(SubmapId::new(1, 4)));
        let an: Vec<_> = a.graph.nodes().map(|(k, _)| *k).collect();
        let bn: Vec<_> = b.graph.nodes().map(|(k, _)| *k).collect();
        assert_eq!(an, bn, "node order is deterministic regardless of insertion");
    }

    #[test]
    fn gnss_constraint_carries_3x3_information() {
        let mut g = PoseGraph::new();
        let s = SubmapId::new(0, 0);
        g.ensure_node(s);
        g.add_constraint(Constraint {
            id: ConstraintId::new(0, 7),
            kind: ConstraintKind::Gnss {
                submap: s,
                position_ecef: [3_875_000.0, 447_000.0, 5_010_000.0],
                information: Matrix3::identity() * 0.25,
            },
            status: ConstraintStatus::Confirmed,
            source: ConstraintSource::Gnss,
        });
        assert!(g.constraint(ConstraintId::new(0, 7)).unwrap().is_global_anchor());
        assert_eq!(g.gauge_anchor(), Some(s));
    }

    #[test]
    fn scene_insert_is_idempotent_on_nodes() {
        let mut s = Scene::new(1);
        let id = SubmapId::new(5, 5);
        s.insert_submap(SubmapBuilder::new(id).seal());
        s.insert_submap(SubmapBuilder::new(id).seal());
        assert_eq!(s.submap_count(), 1);
        assert_eq!(s.graph.node_count(), 1);
    }

    #[test]
    fn rejected_odometry_is_excluded_from_active() {
        let mut g = PoseGraph::new();
        let a = SubmapId::new(0, 0);
        let b = SubmapId::new(0, 1);
        g.ensure_node(a);
        g.ensure_node(b);
        g.add_constraint(odom(0, a, b));
        assert_eq!(g.active_constraints().count(), 1);
        // A bad sequential edge, rejected after review, must NOT stay active.
        assert!(g.set_status(ConstraintId::new(0, 0), ConstraintStatus::Rejected));
        assert_eq!(g.active_constraints().count(), 0);
    }

    #[test]
    fn duplicate_submap_insert_is_keep_first() {
        // The local invariant: a second insert for the same id NEVER overwrites the
        // frozen geometry already there. (Same id is meant to imply same content —
        // the mesh ingest layer hash-verifies that; here we only guarantee that the
        // sealed map is immutable against a later insert, regardless of its payload.)
        let id = SubmapId::new(2, 9);
        let mut first = SubmapBuilder::new(id);
        first.extend((0..3).map(|i| LocalPoint::xyz(i as f32, 0.0, 0.0)));
        let mut second = SubmapBuilder::new(id);
        second.extend((0..50).map(|i| LocalPoint::xyz(0.0, i as f32, 0.0)));

        let mut s = Scene::new(1);
        s.insert_submap(first.seal());
        s.insert_submap(second.seal()); // ignored — first stays frozen
        assert_eq!(s.submap_count(), 1);
        assert_eq!(s.submap(id).unwrap().point_count(), 3, "second insert ignored");
    }
}
