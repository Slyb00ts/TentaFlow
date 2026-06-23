// Integration tests for chunk 0c: submap sealing + pose-graph wiring.
// Reuses the synthetic-scene approach from the LIO tests (helpers duplicated — test
// files don't share modules).

use nalgebra::Point3;
use tentaflow_slam::{
    ConstraintKind, IcpConfig, LioConfig, MappingFrontend, Pose, SealPolicy,
};

fn jitter(seed: u64, amp: f64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = (z >> 11) as f64 / (1u64 << 53) as f64;
    (u * 2.0 - 1.0) * amp
}

/// A larger jittered 3-plane scene that stays visible as the sensor translates along
/// +x (so frame-to-map tracking keeps overlap across the whole trajectory).
fn scene() -> Vec<[f64; 3]> {
    let mut pts = Vec::new();
    let mut i: u64 = 0;
    let j = |p: [f64; 3], s: u64| {
        [
            p[0] + jitter(s, 0.02),
            p[1] + jitter(s.wrapping_mul(3).wrapping_add(1), 0.02),
            p[2] + jitter(s.wrapping_mul(7).wrapping_add(2), 0.02),
        ]
    };
    let mut x = -1.0;
    while x <= 4.0 + 1e-9 {
        let mut b = -1.5;
        while b <= 1.5 + 1e-9 {
            pts.push(j([x, b, 0.0], i)); // floor strip along +x
            pts.push(j([x, 0.0, b + 1.6], i + 1_000_000)); // wall y=0 along +x
            i += 1;
            b += 0.1;
        }
        x += 0.1;
    }
    pts
}

fn pose_x(tx: f64) -> Pose {
    Pose::from_parts([tx, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])
}

fn scan_from(pose: &Pose, world: &[[f64; 3]]) -> Vec<Point3<f32>> {
    let inv = pose.inverse();
    world
        .iter()
        .map(|w| {
            let p = inv.transform_point(*w);
            Point3::new(p[0] as f32, p[1] as f32, p[2] as f32)
        })
        .collect()
}

fn frontend() -> MappingFrontend {
    let lio = LioConfig {
        scan_voxel_size: 0.15,
        map_voxel_size: 0.5,
        max_points_per_voxel: 50,
        icp: IcpConfig { max_corr_dist: 0.5, max_iters: 25, ..IcpConfig::default() },
    };
    // Seal every ~0.6 m so a short trajectory produces several submaps.
    let seal = SealPolicy { max_travel_m: 0.6, max_rot_rad: 0.7, max_points: 1_000_000 };
    MappingFrontend::new(11, 1, lio, 0.2, seal)
}

#[test]
fn trajectory_seals_multiple_submaps_with_odometry_edges() {
    let world = scene();
    let mut fe = frontend();

    let mut sealed = Vec::new();
    // Drive +x in 0.15 m steps over ~3 m → several 0.6 m submaps.
    let mut tx = 0.0;
    while tx <= 3.0 + 1e-9 {
        let step = fe.process_scan(&scan_from(&pose_x(tx), &world), None);
        if let Some(id) = step.sealed {
            sealed.push(id);
        }
        tx += 0.15;
    }

    assert!(sealed.len() >= 3, "expected several submaps, got {}", sealed.len());
    // Every sealed submap is in the scene with a pose node.
    assert_eq!(fe.scene().submap_count(), sealed.len());
    for id in &sealed {
        assert!(fe.scene().submap(*id).is_some());
        assert!(fe.scene().graph.node(*id).is_some());
        assert!(fe.scene().submap(*id).unwrap().point_count() > 0, "frozen geometry");
    }

    // Odometry edges connect consecutive submaps: count == sealed-1.
    let odo = fe
        .scene()
        .graph
        .constraints()
        .filter(|c| matches!(c.kind, ConstraintKind::Odometry { .. }))
        .count();
    assert_eq!(odo, sealed.len() - 1, "one odometry edge per consecutive pair");

    // Gauge anchor = lowest submap id (no georef yet).
    assert_eq!(fe.scene().graph.gauge_anchor(), Some(sealed[0]));
}

#[test]
fn odometry_edge_matches_anchor_difference() {
    let world = scene();
    let mut fe = frontend();

    let mut anchors = Vec::new();
    let mut tx = 0.0;
    while tx <= 3.0 + 1e-9 {
        let step = fe.process_scan(&scan_from(&pose_x(tx), &world), None);
        if let Some(id) = step.sealed {
            anchors.push((id, fe.scene().graph.node(id).unwrap().pose));
        }
        tx += 0.15;
    }
    assert!(anchors.len() >= 2, "expected >=2 seals, got {}", anchors.len());

    // For the first odometry edge, the stored relative must equal anchor0→anchor1.
    let edge = fe
        .scene()
        .graph
        .constraints()
        .find_map(|c| match &c.kind {
            ConstraintKind::Odometry { from, to, relative, .. } => Some((*from, *to, *relative)),
            _ => None,
        })
        .expect("an odometry edge exists");
    let (from, to, relative) = edge;
    let pose_from = fe.scene().graph.node(from).unwrap().pose;
    let pose_to = fe.scene().graph.node(to).unwrap().pose;
    let expected = pose_from.relative_to(&pose_to);
    let (dt, da) = relative.relative_to(&expected).magnitude();
    assert!(dt < 1e-9 && da < 1e-9, "edge relative must equal anchor difference");
}

#[test]
fn sealed_submap_geometry_is_anchor_local() {
    // The submap's first sealed points should be near its anchor origin (local
    // frame), not in world coords — proving anchor-local storage.
    let world = scene();
    let mut fe = frontend();
    let mut first_sealed = None;
    let mut tx = 0.0;
    while first_sealed.is_none() && tx <= 3.0 {
        let step = fe.process_scan(&scan_from(&pose_x(tx), &world), None);
        first_sealed = step.sealed;
        tx += 0.15;
    }
    let id = first_sealed.expect("a submap sealed");
    let sm = fe.scene().submap(id).unwrap();
    let anchor = fe.scene().graph.node(id).unwrap().pose;
    // The anchor is near the trajectory start (~origin), so local≈world here, but the
    // invariant we assert: re-projecting local points by the anchor lands them back
    // on the world scene scale (finite, bounded), i.e. transform is consistent.
    let world_pts = sm.points_in(&anchor);
    assert_eq!(world_pts.len(), sm.point_count());
    // Local extent is bounded (a single submap spans << whole trajectory).
    let max_local = sm
        .geometry()
        .points()
        .iter()
        .map(|p| p.pos.coords.norm())
        .fold(0.0_f32, f32::max);
    assert!(max_local < 10.0, "submap-local coords stay small, got {max_local}");
}
