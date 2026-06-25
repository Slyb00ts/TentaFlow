// =============================================================================
// File: examples/depth_calib.rs — offline depth↔lidar extrinsic calibration
// =============================================================================
//
// Loads a one-shot capture written by `depth_mapping::maybe_dump_calibration`
// (`TENTAFLOW_CALIB_DUMP=1` on a live run) from `/tmp/tf_calib/{depth,lidar}.bin`
// and finds the camera extrinsics (FOV, scale, and a full yaw/pitch/roll mount
// rotation) that best align the back-projected depth cloud onto the lidar cloud
// (ground truth). The metric is a trimmed-mean nearest-neighbour distance from
// depth points to lidar points, with lidar indexed in a voxel hash for speed.
//
// Run:  cargo run --example depth_calib --release
// The capture is reused every run, so calibration iterates fully offline.

use std::collections::HashMap;
use std::io::Read;

use tentaflow_slam::Pose;

const CALIB_DIR: &str = "/tmp/tf_calib";

struct Capture {
    width: usize,
    height: usize,
    cap_fov: f32,
    cap_pitch: f32,
    cap_scale: f32,
    pose: Pose,
    depth: Vec<f32>,
    lidar: Vec<[f32; 3]>,
}

fn read_file(name: &str) -> Vec<u8> {
    let path = format!("{CALIB_DIR}/{name}");
    let mut f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut b = Vec::new();
    f.read_to_end(&mut b).expect("read");
    b
}

fn rd_u32(b: &[u8], o: &mut usize) -> u32 {
    let v = u32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
    *o += 4;
    v
}
fn rd_f32(b: &[u8], o: &mut usize) -> f32 {
    let v = f32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
    *o += 4;
    v
}
fn rd_f64(b: &[u8], o: &mut usize) -> f64 {
    let v = f64::from_le_bytes(b[*o..*o + 8].try_into().unwrap());
    *o += 8;
    v
}

fn load_capture() -> Capture {
    let d = read_file("depth.bin");
    let mut o = 0usize;
    assert_eq!(rd_u32(&d, &mut o), 0x4445_5054, "depth.bin magic");
    let width = rd_u32(&d, &mut o) as usize;
    let height = rd_u32(&d, &mut o) as usize;
    let cap_fov = rd_f32(&d, &mut o);
    let cap_pitch = rd_f32(&d, &mut o);
    let cap_scale = rd_f32(&d, &mut o);
    let t = [rd_f64(&d, &mut o), rd_f64(&d, &mut o), rd_f64(&d, &mut o)];
    let q = [
        rd_f64(&d, &mut o),
        rd_f64(&d, &mut o),
        rd_f64(&d, &mut o),
        rd_f64(&d, &mut o),
    ];
    let mut depth = Vec::with_capacity(width * height);
    for _ in 0..width * height {
        depth.push(rd_f32(&d, &mut o));
    }

    let l = read_file("lidar.bin");
    let mut o = 0usize;
    assert_eq!(rd_u32(&l, &mut o), 0x4C49_4441, "lidar.bin magic");
    let n = rd_u32(&l, &mut o) as usize;
    let mut lidar = Vec::with_capacity(n);
    for _ in 0..n {
        lidar.push([rd_f32(&l, &mut o), rd_f32(&l, &mut o), rd_f32(&l, &mut o)]);
    }

    Capture {
        width,
        height,
        cap_fov,
        cap_pitch,
        cap_scale,
        pose: Pose::from_parts(t, q),
        depth,
        lidar,
    }
}

/// Calibration parameters being optimised. `pitch/yaw/roll` are the camera mount
/// rotation in the body frame (extends production, which only has pitch).
#[derive(Clone, Copy, Debug)]
struct Params {
    fov: f32,
    /// Vertical FOV, decoupled from horizontal because the depth model runs on a
    /// 518² square STRETCHED from the camera's native (≈4:3) frame, so fx≠fy.
    fov_v: f32,
    scale: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
}

/// Body-frame rotation R = Rz(yaw)·Ry(pitch)·Rx(roll), applied to the optical→body
/// point before the scene pose. Production applies only Ry(pitch) about +Y(left).
fn rotate_body(p: [f32; 3], yaw: f32, pitch: f32, roll: f32) -> [f32; 3] {
    let (sr, cr) = roll.to_radians().sin_cos();
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sy, cy) = yaw.to_radians().sin_cos();
    // Rx
    let x1 = p[0];
    let y1 = p[1] * cr - p[2] * sr;
    let z1 = p[1] * sr + p[2] * cr;
    // Ry
    let x2 = x1 * cp + z1 * sp;
    let y2 = y1;
    let z2 = -x1 * sp + z1 * cp;
    // Rz
    let x3 = x2 * cy - y2 * sy;
    let y3 = x2 * sy + y2 * cy;
    let z3 = z2;
    [x3, y3, z3]
}

/// Back-project the depth map to world points with the given params. `stride`
/// subsamples for speed during optimisation.
fn backproject(cap: &Capture, p: &Params, stride: usize) -> Vec<[f32; 3]> {
    let (w, h) = (cap.width, cap.height);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let fx = (w as f32 / 2.0) / (p.fov.to_radians() / 2.0).tan();
    let fy = (h as f32 / 2.0) / (p.fov_v.to_radians() / 2.0).tan();
    let mut out = Vec::new();
    let mut v = 0usize;
    while v < h {
        let row = v * w;
        let mut u = 0usize;
        while u < w {
            let d = cap.depth[row + u] * p.scale;
            // Cut far monocular depth: it is unreliable AND the Go2's wide/fisheye
            // lens makes a pinhole back-projection over-spread edge pixels at range.
            if d.is_finite() && d > 0.05 && d <= 6.0 {
                let x_opt = (u as f32 - cx) * d / fx;
                let y_opt = (v as f32 - cy) * d / fy;
                let z_opt = d;
                // optical → body (FLU, Z-up), then mount rotation
                let body = rotate_body([z_opt, -x_opt, -y_opt], p.yaw, p.pitch, p.roll);
                let world =
                    cap.pose.transform_point([body[0] as f64, body[1] as f64, body[2] as f64]);
                out.push([world[0] as f32, world[1] as f32, world[2] as f32]);
            }
            u += stride;
        }
        v += stride;
    }
    out
}

/// Voxel hash over the lidar cloud for O(1) nearest-neighbour queries.
struct Grid {
    cell: f32,
    map: HashMap<(i32, i32, i32), Vec<[f32; 3]>>,
}
impl Grid {
    fn build(pts: &[[f32; 3]], cell: f32) -> Self {
        let mut map: HashMap<(i32, i32, i32), Vec<[f32; 3]>> = HashMap::new();
        for &p in pts {
            map.entry(Self::key(p, cell)).or_default().push(p);
        }
        Grid { cell, map }
    }
    fn key(p: [f32; 3], cell: f32) -> (i32, i32, i32) {
        (
            (p[0] / cell).floor() as i32,
            (p[1] / cell).floor() as i32,
            (p[2] / cell).floor() as i32,
        )
    }
    /// Squared distance to the nearest lidar point, searching the 27 cells around
    /// the query. Returns `None` if no lidar point is within ~`cell` of the query.
    fn nearest_sq(&self, q: [f32; 3]) -> Option<f32> {
        let (kx, ky, kz) = Self::key(q, self.cell);
        let mut best = f32::INFINITY;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(v) = self.map.get(&(kx + dx, ky + dy, kz + dz)) {
                        for &p in v {
                            let d = (p[0] - q[0]).powi(2)
                                + (p[1] - q[1]).powi(2)
                                + (p[2] - q[2]).powi(2);
                            if d < best {
                                best = d;
                            }
                        }
                    }
                }
            }
        }
        if best.is_finite() {
            Some(best)
        } else {
            None
        }
    }
}

/// Trimmed mean (keep best `frac`) of nearest-neighbour distances from `src` to the
/// `dst` grid. Points with no neighbour in the searched cells count as `miss_dist`.
fn trimmed_nn(src: &[[f32; 3]], dst: &Grid, frac: f32, miss_dist: f32) -> f64 {
    if src.is_empty() {
        return f64::INFINITY;
    }
    let mut dists: Vec<f32> = src
        .iter()
        .map(|&q| dst.nearest_sq(q).map(|d2| d2.sqrt()).unwrap_or(miss_dist))
        .collect();
    dists.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let keep = ((dists.len() as f32 * frac).ceil() as usize).max(1);
    dists[..keep].iter().map(|&d| d as f64).sum::<f64>() / keep as f64
}

/// Bidirectional (chamfer) alignment cost.
/// * forward  — depth→lidar, keep best 70% (rejects the far monocular-depth spray).
/// * reverse  — lidar→depth, keep best 50%: this anchors against the degenerate
///   "shrink the cloud into one lidar cell" solution, and the 50% trim discards the
///   ~half of the 360° lidar that sits behind the robot where the camera can't see.
/// * a soft prior keeps the monocular scale near 1.0.
fn cost(depth_pts: &[[f32; 3]], lidar_grid: &Grid, lidar_pts: &[[f32; 3]], scale: f32) -> f64 {
    if depth_pts.len() < 50 {
        return f64::INFINITY;
    }
    let fwd = trimmed_nn(depth_pts, lidar_grid, 0.70, 2.0);
    let depth_grid = Grid::build(depth_pts, lidar_grid.cell);
    let rev = trimmed_nn(lidar_pts, &depth_grid, 0.50, 2.0);
    let scale_reg = 0.25 * ((scale - 1.0) as f64).abs();
    fwd + rev + scale_reg
}

fn extents(pts: &[[f32; 3]]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    let mut c = [0f64; 3];
    for &p in pts {
        for i in 0..3 {
            mn[i] = mn[i].min(p[i]);
            mx[i] = mx[i].max(p[i]);
            c[i] += p[i] as f64;
        }
    }
    let n = pts.len().max(1) as f64;
    ([c[0] as f32 / n as f32, c[1] as f32 / n as f32, c[2] as f32 / n as f32], mn, mx)
}

/// Top-down (x-y plane) overlay: lidar green, depth magenta. A quick visual check
/// of whether the back-projected depth lands on the lidar geometry.
/// Project onto axes (`ah` horizontal, `av` vertical) — `(0,1)`=top-down x-y,
/// `(0,2)`=side x-z (reveals vertical/height misalignment the top-down hides).
fn render_view(lidar: &[[f32; 3]], depth: &[[f32; 3]], path: &str, ah: usize, av: usize) {
    let all: Vec<[f32; 3]> = lidar.iter().chain(depth.iter()).copied().collect();
    if all.is_empty() {
        return;
    }
    let (_, mn, mx) = extents(&all);
    let (w, h) = (760u32, 760u32);
    let pad = 24.0f32;
    let s = ((w as f32 - 2.0 * pad) / (mx[ah] - mn[ah]).max(0.1))
        .min((h as f32 - 2.0 * pad) / (mx[av] - mn[av]).max(0.1));
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([12, 12, 20]));
    let mut plot = |img: &mut image::RgbImage, p: [f32; 3], c: [u8; 3]| {
        let px = (pad + (p[ah] - mn[ah]) * s) as i32;
        let py = (h as f32 - pad - (p[av] - mn[av]) * s) as i32; // chosen axis up
        for dx in -1..=1 {
            for dy in -1..=1 {
                let (x, y) = (px + dx, py + dy);
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    img.put_pixel(x as u32, y as u32, image::Rgb(c));
                }
            }
        }
    };
    for &p in lidar {
        plot(&mut img, p, [0, 210, 60]);
    }
    for &p in depth {
        plot(&mut img, p, [230, 30, 220]);
    }
    let _ = img.save(path);
}

fn main() {
    let cap = load_capture();
    let grid = Grid::build(&cap.lidar, 0.20);
    // Subsampled lidar for the reverse (lidar→depth) chamfer term.
    let lstep = (cap.lidar.len() / 4000).max(1);
    let lidar_sub: Vec<[f32; 3]> = cap.lidar.iter().step_by(lstep).copied().collect();

    let (lc, lmn, lmx) = extents(&cap.lidar);
    // Hardware truth (Go2 streaming FOV): horizontal 100°, vertical 56° (native 720p
    // 16:9, stretched to the model's 518² square). These are FIXED — fitting them from
    // a floor-dominated scene is under-constrained, so only scale + mount rotation move.
    let init = Params {
        fov: 100.0,
        fov_v: 56.0,
        scale: cap.cap_scale,
        pitch: cap.cap_pitch,
        yaw: 0.0,
        roll: 0.0,
    };
    // Depth value distribution — a quick check of the monocular metric scale.
    let mut dv: Vec<f32> = cap.depth.iter().copied().filter(|&d| d.is_finite() && d > 0.05).collect();
    dv.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !dv.is_empty() {
        let pct = |f: f32| dv[((dv.len() as f32 * f) as usize).min(dv.len() - 1)];
        println!(
            "  depth values (m): p10={:.2} p50={:.2} p90={:.2} max={:.2}",
            pct(0.1),
            pct(0.5),
            pct(0.9),
            dv[dv.len() - 1]
        );
    }
    let dp0 = backproject(&cap, &init, 6);
    let (dc, dmn, dmx) = extents(&dp0);
    println!("capture: {}x{} depth, {} lidar pts", cap.width, cap.height, cap.lidar.len());
    println!(
        "  current params: fov={:.1} pitch={:.1} scale={:.2}",
        cap.cap_fov, cap.cap_pitch, cap.cap_scale
    );
    println!("  lidar  centroid={lc:?} min={lmn:?} max={lmx:?}");
    println!("  depth  centroid={dc:?} min={dmn:?} max={dmx:?}");
    println!("  start cost = {:.3} m", cost(&dp0, &grid, &lidar_sub, init.scale));

    // Pattern search (Hooke-Jeeves) over the 5 params, multi-start over a few scale
    // seeds since the monocular metric scale is the most uncertain DOF.
    let lidar_span = ((lmx[0] - lmn[0]).powi(2) + (lmx[1] - lmn[1]).powi(2)).sqrt();
    let depth_span = ((dmx[0] - dmn[0]).powi(2) + (dmx[1] - dmn[1]).powi(2)).sqrt();
    let scale_hint = if depth_span > 0.1 {
        (init.scale * lidar_span / depth_span).clamp(0.1, 10.0)
    } else {
        init.scale
    };

    let _ = scale_hint;
    let eval = |p: &Params| cost(&backproject(&cap, p, 8), &grid, &lidar_sub, p.scale);
    let mut best = init;
    let mut best_cost = eval(&init);
    // PHYSICAL constraints (the depth values are metric-correct, p50≈1.9 m): scale is
    // pinned near 1.0 so the optimiser can't "win" by collapsing the cloud, and the
    // camera is body-centred so yaw/roll stay small. The dominant unknown is the mount
    // PITCH (the Go2 camera angles down), so multi-start over pitch seeds.
    for &pitch0 in &[-10.0f32, -25.0, -40.0, -55.0] {
        for &y0 in &[0.0f32, -20.0, 20.0] {
            let mut p = Params { scale: 1.0, yaw: y0, pitch: pitch0, roll: 0.0, ..init };
            // step sizes: fov_h, fov_v, scale, pitch, yaw, roll (FOV fixed → skipped)
            let mut step = [0.0f32, 0.0, 0.05, 12.0, 10.0, 10.0];
            let mut c = eval(&p);
            for _ in 0..300 {
                let mut improved = false;
                for axis in 2..6 {
                    for &dir in &[1.0f32, -1.0] {
                        let mut q = p;
                        match axis {
                            0 => q.fov = (q.fov + dir * step[0]).clamp(50.0, 150.0),
                            1 => q.fov_v = (q.fov_v + dir * step[1]).clamp(20.0, 150.0),
                            2 => q.scale = (q.scale + dir * step[2]).clamp(0.85, 1.15),
                            3 => q.pitch = (q.pitch + dir * step[3]).clamp(-89.0, 89.0),
                            4 => q.yaw = (q.yaw + dir * step[4]).clamp(-40.0, 40.0),
                            _ => q.roll = (q.roll + dir * step[5]).clamp(-40.0, 40.0),
                        }
                        let qc = eval(&q);
                        if qc < c {
                            c = qc;
                            p = q;
                            improved = true;
                        }
                    }
                }
                if !improved {
                    for s in step.iter_mut() {
                        *s *= 0.5;
                    }
                    if step.iter().all(|&s| s < 0.02) {
                        break;
                    }
                }
            }
            if c < best_cost {
                best_cost = c;
                best = p;
            }
        }
    }

    // Final high-resolution cost on the full cloud.
    let final_cost = cost(&backproject(&cap, &best, 3), &grid, &lidar_sub, best.scale);
    println!("\n=== best fit ===");
    println!(
        "  fov_h={:.1} fov_v={:.1}  scale={:.3}  pitch={:.1}  yaw={:.1}  roll={:.1}",
        best.fov, best.fov_v, best.scale, best.pitch, best.yaw, best.roll
    );
    println!("  cost: {:.3} m  ->  {:.3} m", best_cost.max(final_cost), final_cost);
    let dp_before = backproject(&cap, &init, 4);
    render_view(&cap.lidar, &dp_before, "/tmp/tf_calib/overlay_before.png", 0, 1); // top-down x-y
    render_view(&cap.lidar, &dp_before, "/tmp/tf_calib/side_before.png", 0, 2); // side x-z (height)
    render_view(&cap.lidar, &backproject(&cap, &best, 4), "/tmp/tf_calib/overlay_after.png", 0, 1);
    println!("  wrote /tmp/tf_calib/overlay_{{before,after}}.png (lidar=green depth=magenta)");
    if best.yaw.abs() < 2.0 && best.roll.abs() < 2.0 {
        println!(
            "  yaw/roll ~0 → production (pitch-only) is sufficient. Set in DB:\n    depth_camera_fov_deg={:.1}, depth_camera_pitch_deg={:.1}, depth_scale={:.2}",
            best.fov, best.pitch, best.scale
        );
    } else {
        println!(
            "  NON-trivial yaw/roll → production must be extended to a full mount rotation.\n    yaw={:.1} pitch={:.1} roll={:.1}",
            best.yaw, best.pitch, best.roll
        );
    }
}
