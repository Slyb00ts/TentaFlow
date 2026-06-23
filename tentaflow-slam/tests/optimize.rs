// Integration tests for chunk 0d: pose-graph optimization + loop-closure gating.
// The optimizer tests use a pure pose graph (no lidar → fast). The loop-closure
// tests build submaps manually with a known overlap.

use nalgebra::{Matrix6, Point3};
use tentaflow_slam::{
    optimize, verify_loop_closure, Constraint, ConstraintId, ConstraintKind, ConstraintSource,
    ConstraintStatus, LoopGate, OptConfig, Pose, PoseGraph, Scene, SubmapBuilder, SubmapId,
};

fn tx(x: f64, y: f64, z: f64) -> Pose {
    Pose::from_parts([x, y, z], [0.0, 0.0, 0.0, 1.0])
}

fn odom(id: u32, from: SubmapId, to: SubmapId, rel: Pose) -> Constraint {
    Constraint {
        id: ConstraintId::new(0, id),
        kind: ConstraintKind::Odometry { from, to, relative: rel, information: Matrix6::identity() },
        status: ConstraintStatus::Confirmed,
        source: ConstraintSource::LidarOdometry,
    }
}

fn close(a: &Pose, b: &Pose, t_tol: f64, r_tol: f64, msg: &str) {
    let (dt, da) = a.relative_to(b).magnitude();
    assert!(dt <= t_tol && da <= r_tol, "{msg}: off by {dt} m / {da} rad");
}

#[test]
fn optimizer_recovers_consistent_chain() {
    // 3 nodes all initialized at identity (wrong). Exact odometry says B is 1 m and
    // C is 2 m along +x from A. Gauge (A, lowest id) is fixed at identity = truth.
    // Optimization must move B→(1,0,0), C→(2,0,0).
    let a = SubmapId::new(0, 0);
    let b = SubmapId::new(0, 1);
    let c = SubmapId::new(0, 2);
    let mut g = PoseGraph::new();
    for id in [a, b, c] {
        g.ensure_node(id);
        g.set_node(id, Pose::identity(), Matrix6::identity());
    }
    g.add_constraint(odom(0, a, b, tx(1.0, 0.0, 0.0)));
    g.add_constraint(odom(1, b, c, tx(1.0, 0.0, 0.0)));

    let report = optimize(&mut g, &OptConfig::default());
    assert!(report.converged, "should converge");
    assert!(report.final_cost < 1e-9, "consistent graph → ~0 cost, got {}", report.final_cost);
    close(&g.node(a).unwrap().pose, &Pose::identity(), 1e-6, 1e-6, "A fixed (gauge)");
    close(&g.node(b).unwrap().pose, &tx(1.0, 0.0, 0.0), 1e-4, 1e-4, "B");
    close(&g.node(c).unwrap().pose, &tx(2.0, 0.0, 0.0), 1e-4, 1e-4, "C");
}

#[test]
fn anchor_moves_node_to_anchor_pose() {
    // A single submap anchored at a non-identity pose, initialized at identity. The
    // absolute anchor must pull it to the target (gauge is NOT fixed to it).
    let s = SubmapId::new(0, 0);
    let mut g = PoseGraph::new();
    g.ensure_node(s);
    g.set_node(s, Pose::identity(), Matrix6::identity());
    let target = tx(2.0, -1.0, 0.5);
    g.add_constraint(Constraint {
        id: ConstraintId::new(0, 1),
        kind: ConstraintKind::Anchor {
            submap: s,
            submap_pose: target,
            information: Matrix6::identity() * 10.0,
        },
        status: ConstraintStatus::Confirmed,
        source: ConstraintSource::Anchor,
    });
    let report = optimize(&mut g, &OptConfig::default());
    assert!(report.final_cost < 1e-6, "anchor should be satisfied, cost {}", report.final_cost);
    close(&g.node(s).unwrap().pose, &target, 1e-4, 1e-4, "anchored node moved to target");
}

#[test]
fn loop_closure_distributes_drift() {
    // A→B→C→D odometry forms an L; a loop closure D→A pins the loop. Initialize
    // nodes by integrating odometry (so D is wherever the chain lands); the loop
    // closure says D→A's TRUE relative, which should be satisfied after optimization
    // (final cost drops to ~0 because all edges are mutually consistent here).
    let ids: Vec<SubmapId> = (0..4).map(|s| SubmapId::new(0, s)).collect();
    let mut g = PoseGraph::new();

    // True square: A(0,0) B(1,0) C(1,1) D(0,1).
    let truth = [tx(0.0, 0.0, 0.0), tx(1.0, 0.0, 0.0), tx(1.0, 1.0, 0.0), tx(0.0, 1.0, 0.0)];
    for (i, id) in ids.iter().enumerate() {
        g.ensure_node(*id);
        // Initialize off-truth (except gauge) to give the optimizer work.
        let init = if i == 0 { truth[0] } else { Pose::identity() };
        g.set_node(*id, init, Matrix6::identity());
    }
    // Odometry edges = true relatives.
    for i in 0..3 {
        let rel = truth[i].relative_to(&truth[i + 1]);
        g.add_constraint(odom(i as u32, ids[i], ids[i + 1], rel));
    }
    // Loop closure D→A = true relative.
    let loop_rel = truth[3].relative_to(&truth[0]);
    g.add_constraint(Constraint {
        id: ConstraintId::new(0, 100),
        kind: ConstraintKind::LoopClosure {
            from: ids[3],
            to: ids[0],
            relative: loop_rel,
            information: Matrix6::identity() * 10.0,
        },
        status: ConstraintStatus::Confirmed,
        source: ConstraintSource::LidarLoopClosure,
    });

    let report = optimize(&mut g, &OptConfig { max_iters: 50, ..OptConfig::default() });
    assert!(report.final_cost < 1e-6, "consistent loop → ~0 cost, got {}", report.final_cost);
    for (i, id) in ids.iter().enumerate() {
        close(&g.node(*id).unwrap().pose, &truth[i], 1e-3, 1e-3, "node recovered to truth");
    }
}

// ---- loop closure verification + gating ----

fn jitter(seed: u64, amp: f64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = (z >> 11) as f64 / (1u64 << 53) as f64;
    (u * 2.0 - 1.0) * amp
}

/// A jittered cube of points in a local frame.
fn cloud() -> Vec<Point3<f32>> {
    let mut v = Vec::new();
    let mut i = 0u64;
    let mut a = -1.0;
    while a <= 1.0 + 1e-9 {
        let mut b = -1.0;
        while b <= 1.0 + 1e-9 {
            v.push(Point3::new(
                (a + jitter(i, 0.03)) as f32,
                (b + jitter(i + 7, 0.03)) as f32,
                (jitter(i + 19, 0.03)) as f32,
            ));
            v.push(Point3::new(
                (a + jitter(i + 101, 0.03)) as f32,
                (jitter(i + 53, 0.03)) as f32,
                (b + jitter(i + 31, 0.03)) as f32,
            ));
            i += 1;
            b += 0.1;
        }
        a += 0.1;
    }
    v
}

/// Build a scene with two overlapping submaps related by `g_from_to` (from-local →
/// to-local), nodes set to a (possibly off) estimate.
fn two_overlapping_submaps(g_from_to: Pose, from_node: Pose, to_node: Pose) -> (Scene, SubmapId, SubmapId) {
    let to_pts = cloud();
    let g_inv = g_from_to.inverse();
    let from_id = SubmapId::new(0, 0);
    let to_id = SubmapId::new(0, 1);

    let mut from_b = SubmapBuilder::new(from_id);
    for p in &to_pts {
        // from-local = (from→to)⁻¹ · to-local
        let l = g_inv.transform_point([p.x as f64, p.y as f64, p.z as f64]);
        from_b.push(tentaflow_slam::LocalPoint::xyz(l[0] as f32, l[1] as f32, l[2] as f32));
    }
    let mut to_b = SubmapBuilder::new(to_id);
    for p in &to_pts {
        to_b.push(tentaflow_slam::LocalPoint::xyz(p.x, p.y, p.z));
    }

    let mut scene = Scene::new(1);
    scene.insert_submap(from_b.seal());
    scene.insert_submap(to_b.seal());
    scene.graph.set_node(from_id, from_node, Matrix6::identity());
    scene.graph.set_node(to_id, to_node, Matrix6::identity());
    (scene, from_id, to_id)
}

#[test]
fn loop_closure_verify_accepts_overlap_and_recovers_relative() {
    let g_from_to = tx(0.15, -0.1, 0.05);
    // Node estimate is slightly off the truth → ICP must refine + gate must accept.
    let (scene, from, to) =
        two_overlapping_submaps(g_from_to, tx(0.1, -0.05, 0.0), Pose::identity());
    let v = verify_loop_closure(&scene, from, to, ConstraintId::new(0, 9), &LoopGate::default())
        .expect("submaps present");
    assert!(v.accepted, "overlapping pair must be accepted (ratio {})", v.inlier_ratio);
    if let ConstraintKind::LoopClosure { relative, .. } = v.constraint.kind {
        // Stored in optimizer convention z = T_from⁻¹·T_to = (geometry R_ft)⁻¹.
        close(&relative, &g_from_to.inverse(), 0.03, 0.03, "recovered loop relative");
    } else {
        panic!("expected a LoopClosure constraint");
    }
    assert_eq!(v.constraint.status, ConstraintStatus::Confirmed);
}

#[test]
fn loop_closure_verify_rejects_nonoverlapping_pair() {
    // `from` geometry shoved 1000 m away → no correspondences → reject.
    let to_pts = cloud();
    let from_id = SubmapId::new(0, 0);
    let to_id = SubmapId::new(0, 1);
    let mut from_b = SubmapBuilder::new(from_id);
    for p in &to_pts {
        from_b.push(tentaflow_slam::LocalPoint::xyz(p.x + 1000.0, p.y, p.z));
    }
    let mut to_b = SubmapBuilder::new(to_id);
    for p in &to_pts {
        to_b.push(tentaflow_slam::LocalPoint::xyz(p.x, p.y, p.z));
    }
    let mut scene = Scene::new(1);
    scene.insert_submap(from_b.seal());
    scene.insert_submap(to_b.seal());
    scene.graph.set_node(from_id, Pose::identity(), Matrix6::identity());
    scene.graph.set_node(to_id, Pose::identity(), Matrix6::identity());

    let v = verify_loop_closure(&scene, from_id, to_id, ConstraintId::new(0, 9), &LoopGate::default())
        .expect("submaps present");
    assert!(!v.accepted, "non-overlapping pair must be rejected");
    assert_eq!(v.constraint.status, ConstraintStatus::Rejected);
}
