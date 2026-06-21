// Integration tests for chunk 0b: LiDAR-inertial odometry (voxel + ICP + tracker).
// Synthetic scene = 3 orthogonal planes (constrains all 6 DoF). We synthesize each
// scan as the scene seen FROM a known sensor pose, then check the tracker/ICP
// recovers that pose.

use nalgebra::Point3;
use tentaflow_slam::{
    register, voxel_downsample, IcpConfig, LioConfig, LioTracker, Pose, VoxelMap,
};

/// Deterministic pseudo-random jitter in [-amp, amp] from an integer seed (no rng
/// dependency). Breaks the regular grid so point-to-point correspondences are
/// unique at the solution — real lidar scans are never perfectly gridded.
fn jitter(seed: u64, amp: f64) -> f64 {
    // splitmix64-ish hash → [0,1) → [-amp, amp].
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = (z >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
    (u * 2.0 - 1.0) * amp
}

/// 3 orthogonal plane patches (floor z=0, wall y=0, wall x=0) with per-point jitter
/// so the cloud constrains all 6 DoF and isn't a degenerate regular grid.
fn scene() -> Vec<[f64; 3]> {
    let mut pts = Vec::new();
    let mut i: u64 = 0;
    let j = |p: [f64; 3], s: u64| [
        p[0] + jitter(s, 0.02),
        p[1] + jitter(s.wrapping_mul(3).wrapping_add(1), 0.02),
        p[2] + jitter(s.wrapping_mul(7).wrapping_add(2), 0.02),
    ];
    let mut a = -1.5;
    while a <= 1.5 + 1e-9 {
        let mut b = -1.5;
        while b <= 1.5 + 1e-9 {
            pts.push(j([a, b, 0.0], i)); // floor
            pts.push(j([a, 0.0, b + 1.6], i + 1_000_000)); // wall y=0
            pts.push(j([0.0, a, b + 1.6], i + 2_000_000)); // wall x=0
            i += 1;
            b += 0.1;
        }
        a += 0.1;
    }
    pts
}

/// Pose with a Z-axis rotation `theta` and translation `t`.
fn pose_tz(t: [f64; 3], theta: f64) -> Pose {
    let (s, c) = (theta / 2.0).sin_cos();
    Pose::from_parts(t, [0.0, 0.0, s, c])
}

/// The scan a sensor at `pose` (sensor→world) observes: world points expressed in
/// the sensor frame = pose⁻¹ · world.
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

/// Assert two poses are within tolerances (translation metres, rotation radians).
fn assert_pose_close(got: &Pose, want: &Pose, t_tol: f64, r_tol: f64) {
    let (dt, da) = got.relative_to(want).magnitude();
    assert!(dt <= t_tol, "translation off by {dt} m (tol {t_tol})");
    assert!(da <= r_tol, "rotation off by {da} rad (tol {r_tol})");
}

#[test]
fn voxel_downsample_collapses_to_one_per_voxel() {
    // 1000 points all inside a single 1.0 m voxel → exactly one output point.
    let pts: Vec<Point3<f32>> = (0..1000)
        .map(|i| Point3::new(0.1 + (i % 7) as f32 * 0.01, 0.2, 0.3))
        .collect();
    let out = voxel_downsample(&pts, 1.0);
    assert_eq!(out.len(), 1);
}

#[test]
fn voxel_map_nearest_finds_closest() {
    let mut m = VoxelMap::new(0.5, 20);
    m.add_point(Point3::new(0.0, 0.0, 0.0));
    m.add_point(Point3::new(5.0, 0.0, 0.0));
    let nn = m.nearest(&nalgebra::Point3::new(4.9, 0.0, 0.0), 1.0).unwrap();
    assert!((nn.x - 5.0).abs() < 1e-6);
    // Nothing within range → None.
    assert!(m.nearest(&nalgebra::Point3::new(50.0, 0.0, 0.0), 1.0).is_none());
}

#[test]
fn icp_recovers_known_rigid_transform() {
    let world = scene();
    let mut map = VoxelMap::new(0.5, 50);
    for w in &world {
        map.add_point(Point3::new(w[0] as f32, w[1] as f32, w[2] as f32));
    }
    // Ground-truth G; source = G⁻¹·world so that the optimal T == G (T·source≈map).
    let g = pose_tz([0.12, -0.08, 0.05], 0.06);
    let g_inv = g.inverse();
    let source: Vec<Point3<f32>> = world
        .iter()
        .map(|w| {
            let p = g_inv.transform_point(*w);
            Point3::new(p[0] as f32, p[1] as f32, p[2] as f32)
        })
        .collect();

    let cfg = IcpConfig { max_corr_dist: 1.0, ..IcpConfig::default() };
    let res = register(&source, &map, Pose::identity(), &cfg);
    assert!(res.inliers > 100, "should match most points, got {}", res.inliers);
    assert_pose_close(&res.pose, &g, 0.01, 0.01);
}

#[test]
fn tracker_follows_synthetic_trajectory() {
    let world = scene();
    let cfg = LioConfig {
        scan_voxel_size: 0.1,
        map_voxel_size: 0.5,
        max_points_per_voxel: 50,
        icp: IcpConfig { max_corr_dist: 1.0, max_iters: 40, ..IcpConfig::default() },
    };
    let mut tracker = LioTracker::new(cfg);

    // Ground-truth trajectory (starts at origin so frame-0 seeds the map there).
    let traj = [
        pose_tz([0.0, 0.0, 0.0], 0.0),
        pose_tz([0.2, 0.1, 0.0], 0.03),
        pose_tz([0.45, 0.18, -0.05], 0.07),
        pose_tz([0.7, 0.22, -0.05], 0.10),
    ];

    for (i, gt) in traj.iter().enumerate() {
        let scan = scan_from(gt, &world);
        let r = tracker.process_scan(&scan, None);
        if i == 0 {
            assert!(r.icp.is_none(), "frame 0 only seeds the map");
        } else {
            assert!(r.icp.unwrap().inliers > 100);
        }
        assert_pose_close(&tracker.pose(), gt, 0.05, 0.03);
    }
}

#[test]
fn tracker_rejects_overlapless_scan_without_corrupting_map() {
    let world = scene();
    let cfg = LioConfig {
        scan_voxel_size: 0.1,
        map_voxel_size: 0.5,
        max_points_per_voxel: 50,
        icp: IcpConfig { max_corr_dist: 1.0, ..IcpConfig::default() },
    };
    let mut tracker = LioTracker::new(cfg);

    let p0 = Pose::identity();
    tracker.process_scan(&scan_from(&p0, &world), None);
    let map_len_after_seed = tracker.map().len();

    // A garbage scan far from anything the map contains → no correspondences.
    let garbage: Vec<Point3<f32>> =
        (0..500).map(|i| Point3::new(1000.0 + i as f32 * 0.01, 1000.0, 1000.0)).collect();
    let r = tracker.process_scan(&garbage, None);
    assert!(!r.ok, "overlapless scan must be rejected");
    assert_eq!(tracker.map().len(), map_len_after_seed, "map must NOT be corrupted");

    // A subsequent GOOD scan still tracks — the map survived intact.
    let p1 = pose_tz([0.2, 0.1, 0.0], 0.03);
    let r1 = tracker.process_scan(&scan_from(&p1, &world), None);
    assert!(r1.ok);
    assert_pose_close(&tracker.pose(), &p1, 0.05, 0.03);
}

#[test]
fn tracker_uses_imu_prior_when_given() {
    // With an exact prior the registration starts already aligned and stays accurate.
    let world = scene();
    let mut tracker = LioTracker::new(LioConfig {
        scan_voxel_size: 0.1,
        map_voxel_size: 0.5,
        max_points_per_voxel: 50,
        icp: IcpConfig { max_corr_dist: 1.0, ..IcpConfig::default() },
    });
    let p0 = Pose::identity();
    let p1 = pose_tz([0.3, 0.0, 0.0], 0.0);
    tracker.process_scan(&scan_from(&p0, &world), None);
    let r = tracker.process_scan(&scan_from(&p1, &world), Some(p1));
    assert!(r.icp.is_some());
    assert_pose_close(&tracker.pose(), &p1, 0.03, 0.02);
}
