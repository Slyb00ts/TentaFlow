// Integration tests for chunk 0e: canonical LiDAR frame → SLAM → GlobalPose.
// Encodes synthetic sensor-frame scans as the EXACT wire format the Go2 addon emits
// (tentaflow_sdk_spec::LidarFrameHeader) and feeds them through SlamService.

use nalgebra::Point3;
use tentaflow_slam::{decode_lidar_frame, IcpConfig, LioConfig, Pose, SealPolicy, SlamService};
use tentaflow_sdk_spec::{
    LidarFrameHeader, GLOBAL_POSE_VERSION, LIDAR_FRAME_VERSION, LIDAR_LAYOUT_XYZ,
    LIDAR_LAYOUT_XYZ_I16_PLANAR, POSE_SRC_LIDAR, POSE_STATE_SCENE_LOCAL,
};

fn jitter(seed: u64, amp: f64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = (z >> 11) as f64 / (1u64 << 53) as f64;
    (u * 2.0 - 1.0) * amp
}

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
    let mut a = -1.5;
    while a <= 1.5 + 1e-9 {
        let mut b = -1.5;
        while b <= 1.5 + 1e-9 {
            pts.push(j([a, b, 0.0], i)); // floor z=0
            pts.push(j([a, 0.0, b + 1.6], i + 1_000_000)); // wall y=0 (constrains y)
            pts.push(j([0.0, a, b + 1.6], i + 2_000_000)); // wall x=0 (constrains x)
            i += 1;
            b += 0.1;
        }
        a += 0.1;
    }
    pts
}

/// Translation + a small yaw — well-conditioned motion (pure translation over a
/// gridded floor is weakly observable; rotation breaks that degeneracy).
fn pose_tz(tx: f64, theta: f64) -> Pose {
    let (s, c) = (theta / 2.0).sin_cos();
    Pose::from_parts([tx, 0.0, 0.0], [0.0, 0.0, s, c])
}

/// Sensor-frame scan as f32 points.
fn scan_from(pose: &Pose, world: &[[f64; 3]]) -> Vec<[f32; 3]> {
    let inv = pose.inverse();
    world
        .iter()
        .map(|w| {
            let p = inv.transform_point(*w);
            [p[0] as f32, p[1] as f32, p[2] as f32]
        })
        .collect()
}

/// Encode points as a canonical f32 XYZ LiDAR frame (the wire format).
fn encode_xyz(points: &[[f32; 3]], ts: i64) -> Vec<u8> {
    let h = LidarFrameHeader {
        version: LIDAR_FRAME_VERSION,
        layout: LIDAR_LAYOUT_XYZ,
        flags: 0,
        point_count: points.len() as u32,
        frame_seq: 0,
        timestamp_us: ts,
        host_send_us: 0,
        resolution: 0.05,
        origin: [0.0; 3],
    };
    let mut b = h.encode_header().to_vec();
    for p in points {
        for c in p {
            b.extend_from_slice(&c.to_le_bytes());
        }
    }
    b
}

fn service() -> SlamService {
    let lio = LioConfig {
        scan_voxel_size: 0.1,
        map_voxel_size: 0.5,
        max_points_per_voxel: 50,
        icp: IcpConfig { max_corr_dist: 1.0, max_iters: 40, ..IcpConfig::default() },
    };
    SlamService::new(7, 1, lio, 0.2, SealPolicy::default())
}

#[test]
fn decode_xyz_frame_round_trips_points() {
    let pts = [[1.0f32, 2.0, 3.0], [-4.0, 5.5, 0.0]];
    let frame = encode_xyz(&pts, 123);
    let d = decode_lidar_frame(&frame).expect("decode");
    assert_eq!(d.timestamp_us, 123);
    assert_eq!(d.points.len(), 2);
    assert!((d.points[0] - Point3::new(1.0, 2.0, 3.0)).norm() < 1e-6);
    assert!((d.points[1] - Point3::new(-4.0, 5.5, 0.0)).norm() < 1e-6);
}

#[test]
fn decode_i16_planar_frame_reconstructs_world() {
    // Planar i16 grid: ix=[10,20], iy=[0,5], iz=[3,3], res=0.1 → world idx*res.
    let h = LidarFrameHeader {
        version: LIDAR_FRAME_VERSION,
        layout: LIDAR_LAYOUT_XYZ_I16_PLANAR,
        flags: 0,
        point_count: 2,
        frame_seq: 0,
        timestamp_us: 9,
        host_send_us: 0,
        resolution: 0.1,
        origin: [0.0, 0.0, 0.0],
    };
    let mut b = h.encode_header().to_vec();
    for v in [10i16, 20, 0, 5, 3, 3] {
        // planar: ix,ix, iy,iy, iz,iz
        b.extend_from_slice(&v.to_le_bytes());
    }
    let d = decode_lidar_frame(&b).expect("decode i16");
    assert!((d.points[0] - Point3::new(1.0, 0.0, 0.3)).norm() < 1e-5);
    assert!((d.points[1] - Point3::new(2.0, 0.5, 0.3)).norm() < 1e-5);
}

#[test]
fn service_tracks_trajectory_from_canonical_frames() {
    let world = scene();
    let mut svc = service();
    let traj = [(0.0, 0.0), (0.2, 0.03), (0.45, 0.07), (0.7, 0.10)];
    for (i, &(tx, th)) in traj.iter().enumerate() {
        let gt = pose_tz(tx, th);
        let frame = encode_xyz(&scan_from(&gt, &world), i as i64 * 1000);
        let gp = svc.ingest_lidar_frame(&frame, None).expect("decoded");
        assert_eq!(gp.version, GLOBAL_POSE_VERSION);
        assert_eq!(gp.state, POSE_STATE_SCENE_LOCAL, "metric, not yet georeferenced");
        assert!(gp.source & POSE_SRC_LIDAR != 0);
        assert_eq!(gp.timestamp_us, i as i64 * 1000);
        // Scene-local position tracks the ground-truth translation.
        assert!((gp.position[0] - tx).abs() < 0.05, "x off: {} vs {tx}", gp.position[0]);
    }
}

#[test]
fn service_rejects_undecodable_frame() {
    let mut svc = service();
    assert!(svc.ingest_lidar_frame(&[0u8; 8], None).is_none());
}

#[test]
fn decode_rejects_non_finite_points() {
    let frame = encode_xyz(&[[1.0, f32::NAN, 3.0]], 1);
    assert!(decode_lidar_frame(&frame).is_none(), "NaN coordinate must be rejected");
    let frame = encode_xyz(&[[1.0, 2.0, f32::INFINITY]], 1);
    assert!(decode_lidar_frame(&frame).is_none(), "Inf coordinate must be rejected");
}

#[test]
fn lost_track_reports_huge_covariance() {
    let world = scene();
    let mut svc = service();
    // Seed.
    let seed = encode_xyz(&scan_from(&pose_tz(0.0, 0.0), &world), 0);
    svc.ingest_lidar_frame(&seed, None).unwrap();
    // A finite but overlap-less scan → tracking rejected → Lost + huge covariance.
    let far: Vec<[f32; 3]> = (0..400).map(|i| [1000.0 + i as f32 * 0.01, 1000.0, 1000.0]).collect();
    let gp = svc.ingest_lidar_frame(&encode_xyz(&far, 1), None).unwrap();
    assert_eq!(gp.state, tentaflow_sdk_spec::POSE_STATE_LOST);
    assert!(gp.cov_diag[0] > 1.0e3, "Lost pose must carry huge covariance");
}
