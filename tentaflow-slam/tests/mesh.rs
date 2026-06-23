// Integration tests for chunk 0f: 2-node mesh merge → convergent poses.
// Two nodes build disjoint submap chains, share an inter-node loop closure, merge
// each other's facts, and must then derive BYTE-IDENTICAL poses (UNIFIED_SLAM
// _ARCHITECTURE §6). Pure pose graph (empty geometry) → fast.

use nalgebra::Matrix6;
use tentaflow_slam::{
    optimize, Constraint, ConstraintId, ConstraintKind, ConstraintSource, ConstraintStatus,
    OptConfig, Pose, Scene, SubmapBuilder, SubmapId,
};

fn tx(x: f64, y: f64, z: f64) -> Pose {
    Pose::from_parts([x, y, z], [0.0, 0.0, 0.0, 1.0])
}

fn odom(id: ConstraintId, from: SubmapId, to: SubmapId, rel: Pose) -> Constraint {
    Constraint {
        id,
        kind: ConstraintKind::Odometry { from, to, relative: rel, information: Matrix6::identity() },
        status: ConstraintStatus::Confirmed,
        source: ConstraintSource::LidarOdometry,
    }
}

/// A node's scene: two submaps `(node,0)`→`(node,1)` joined by 1 m odometry.
fn build_node(node: u64) -> Scene {
    let s0 = SubmapId::new(node, 0);
    let s1 = SubmapId::new(node, 1);
    let mut sc = Scene::new(1);
    sc.insert_submap(SubmapBuilder::new(s0).seal());
    sc.insert_submap(SubmapBuilder::new(s1).seal());
    // Arbitrary (different per node) initial poses — reinitialize must wash these out.
    sc.graph.set_node(s0, tx(5.0, 5.0, 0.0), Matrix6::identity());
    sc.graph.set_node(s1, tx(9.0, -2.0, 0.0), Matrix6::identity());
    sc.graph.add_constraint(odom(ConstraintId::new(node, 0), s0, s1, tx(1.0, 0.0, 0.0)));
    sc
}

fn interlink() -> Constraint {
    // (1,1) → (2,0): the two nodes' chains are 1 m apart in +y.
    Constraint {
        id: ConstraintId::new(3, 0),
        kind: ConstraintKind::LoopClosure {
            from: SubmapId::new(1, 1),
            to: SubmapId::new(2, 0),
            relative: tx(0.0, 1.0, 0.0),
            information: Matrix6::identity() * 10.0,
        },
        status: ConstraintStatus::Confirmed,
        source: ConstraintSource::InterDevice,
    }
}

fn close(a: &Pose, b: &Pose, msg: &str) {
    let (dt, da) = a.relative_to(b).magnitude();
    assert!(dt <= 1e-9 && da <= 1e-9, "{msg}: off by {dt} m / {da} rad");
}

#[test]
fn two_nodes_converge_to_identical_poses() {
    let mut a = build_node(1);
    let mut b = build_node(2);
    // The inter-node loop closure is a shared fact on both nodes.
    a.graph.add_constraint(interlink());
    b.graph.add_constraint(interlink());

    // Each node merges the other's submaps + constraints (conflict-free union).
    a.merge_from(&b).unwrap();
    b.merge_from(&a).unwrap();

    let cfg = OptConfig { reinitialize: true, max_iters: 50, ..OptConfig::default() };
    optimize(&mut a.graph, &cfg);
    optimize(&mut b.graph, &cfg);

    // Convergence: both nodes derive byte-identical poses for EVERY submap.
    let ids = [
        SubmapId::new(1, 0),
        SubmapId::new(1, 1),
        SubmapId::new(2, 0),
        SubmapId::new(2, 1),
    ];
    for id in ids {
        let pa = a.graph.node(id).unwrap().pose;
        let pb = b.graph.node(id).unwrap().pose;
        close(&pa, &pb, "cross-node pose mismatch");
    }

    // And the converged layout matches the constraints (gauge = lowest id at origin):
    // (1,0)=0; (1,1)=+1x; (2,0)=(1,1)+1y=(1,1,0); (2,1)=(2,0)+1x=(2,1,0).
    close(&a.graph.node(SubmapId::new(1, 0)).unwrap().pose, &tx(0.0, 0.0, 0.0), "gauge");
    close(&a.graph.node(SubmapId::new(1, 1)).unwrap().pose, &tx(1.0, 0.0, 0.0), "A1");
    close(&a.graph.node(SubmapId::new(2, 0)).unwrap().pose, &tx(1.0, 1.0, 0.0), "B0");
    close(&a.graph.node(SubmapId::new(2, 1)).unwrap().pose, &tx(2.0, 1.0, 0.0), "B1");
}

#[test]
fn merge_is_idempotent_and_order_independent() {
    let a0 = build_node(1);
    let b0 = build_node(2);

    // Merge order A←B then re-merge → second merge adds nothing.
    let mut a = a0.clone();
    let (ns, nc) = a.merge_from(&b0).unwrap();
    assert_eq!((ns, nc), (2, 1), "first merge adds B's 2 submaps + 1 constraint");
    let (ns2, nc2) = a.merge_from(&b0).unwrap();
    assert_eq!((ns2, nc2), (0, 0), "re-merging the same facts is a no-op");

    // Opposite order yields the same merged set (same submaps + constraints).
    let mut b = b0.clone();
    b.merge_from(&a0).unwrap();
    assert_eq!(a.submap_count(), b.submap_count());
    assert_eq!(a.graph.constraint_count(), b.graph.constraint_count());
}

#[test]
fn merge_rejects_different_scene() {
    use tentaflow_slam::SceneMergeError;
    let mut a = Scene::new(1);
    let other = Scene::new(2); // different scene id
    assert_eq!(
        a.merge_from(&other),
        Err(SceneMergeError::SceneIdMismatch { expected: 1, got: 2 }),
        "cross-scene merge must be refused, not silently unioned"
    );
}
