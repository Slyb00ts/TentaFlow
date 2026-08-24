// ===== File: motion.rs — lighting-robust directional motion in a zone =====
//
// Algorithmic core of the zone-based, motion-triggered event recorder. The YOLO
// vehicle detector routinely misses tankers that fill the frame (no context,
// out-of-training-scale, nano model), so recording cannot rely on detections.
// This estimator produces an independent motion signal that fires on any large
// moving object AND stays robust to weather / lighting flicker — the exact
// failure mode of naive background subtraction.
//
// Illumination-invariance method (chosen): per-block ZERO-MEAN NORMALISED
// CROSS-CORRELATION (ZNCC) on the downsampled luma. For any affine brightness
// change I' = a*I + b (a > 0), ZNCC(I, a*I+b) == 1.0 exactly: subtracting each
// block's own mean removes the additive term `b`, and dividing by each block's
// own norm removes the multiplicative term `a`. That makes the correlation peak
// stay at zero displacement under both a global brightness STEP (additive
// flicker) and a global brightness SCALE (multiplicative flicker), so neither
// looks like motion. A single measure covering both additive and multiplicative
// invariance is why ZNCC is preferred here over plain zero-mean SAD (additive
// only) or a raw-luma metric (neither). Real translation of a textured object
// still moves the ZNCC peak off zero, coherently and with a consistent sign.

/// One frame's motion verdict inside the active detection zone(s).
#[derive(Debug, Clone, Copy, Default)]
pub struct MotionSignal {
    /// Coherent directional motion is present in the zone this frame.
    pub moving: bool,
    /// Net horizontal direction of the motion, -1.0..=1.0 (positive = rightward,
    /// negative = leftward). 0.0 when not moving.
    pub dir_x: f32,
    /// Motion strength, 0.0..=1.0 (median coherent block displacement, normalised).
    pub magnitude: f32,
    /// Normalised x-position (0.0..=1.0, full-frame coords) of the moving mass —
    /// used to tell "entered from the left" vs "exited on the right".
    pub centroid_x: f32,
    /// Fraction of evaluated blocks that agree on the dominant direction, 0.0..=1.0.
    pub coherence: f32,
}

// --- Fixed, resolution-independent working grid ------------------------------
// The zone bounding box is resampled onto a fixed DS_W x DS_H luma grid, so the
// per-frame cost is bounded regardless of a 4K input. Block matching then runs
// on that grid.
const DS_W: usize = 120; // downsampled region width  (cells)
const DS_H: usize = 48; //  downsampled region height (cells)
const SUB: usize = 3; //    sub-samples per axis per cell (bounds source reads)
const PW: usize = 6; //     block width  in downsampled cells (DS_W % PW == 0)
const PH: usize = 4; //     block height in downsampled cells (DS_H % PH == 0)
const BX: usize = DS_W / PW; // blocks across (20)
const BY: usize = DS_H / PH; // blocks down   (12)
const SEARCH: i32 = 8; //   horizontal search range +/- cells (~1/15 of DS_W)

// Robustness thresholds.
const MATCH_MIN: f32 = 0.55; //   min ZNCC peak to trust a block's displacement.
                             //                                A rigidly translating textured object scores
                             //                                ~1.0; random noise's best-of-many-shifts peak
                             //                                sits well below this, so raising the gate here
                             //                                rejects incoherent noise without losing motion.
const TEX_FLOOR: f32 = 4.0; //    min block variance to be a texture (evaluated)
const DISP_THRESH: i32 = 1; //    |displacement| >= this counts as "moving"
const COHERENCE_MIN: f32 = 0.20; // fraction of evaluated blocks that must agree
const MAG_FLOOR: f32 = 0.06; //   min normalised magnitude to call it motion
const FLICKER_MEAN_JUMP: f32 = 12.0; // region-mean jump that flags global flicker

/// Integer pixel rectangle of the evaluated region within the frame, plus the
/// frame size it was derived from. Used to detect zone/resolution changes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geom {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    w: usize,
    h: usize,
}

pub struct MotionEstimator {
    /// Previous frame's downsampled region luma (DS_W * DS_H), if any.
    prev: Option<Vec<f32>>,
    /// Region mean of the previous downsampled grid (for flicker detection).
    prev_mean: f32,
    /// Geometry the previous grid was sampled from; a mismatch resets state.
    geom: Option<Geom>,
}

impl Default for MotionEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl MotionEstimator {
    pub fn new() -> Self {
        MotionEstimator {
            prev: None,
            prev_mean: 0.0,
            geom: None,
        }
    }

    /// Estimate zone motion for the current frame. `luma` is the NV12 Y plane:
    /// pixel (col,row) = `luma[y_offset as usize + row*y_stride as usize + col]`,
    /// valid for col in 0..w, row in 0..h. `zones` are normalised polygons
    /// (`Vec<Vec<(f32,f32)>>`, coords 0.0..=1.0); an EMPTY slice means "whole frame".
    /// Returns a non-moving default on the first frame (no previous) and whenever a
    /// global illumination change (flicker) is detected rather than real motion.
    pub fn estimate(
        &mut self,
        luma: &[u8],
        w: u32,
        h: u32,
        y_stride: u32,
        y_offset: u32,
        zones: &[Vec<(f32, f32)>],
    ) -> MotionSignal {
        let default = MotionSignal::default();

        // --- Input validation: never panic on malformed input. ---------------
        let (wz, hz) = (w as usize, h as usize);
        let stride = y_stride as usize;
        let offset = y_offset as usize;
        if luma.is_empty() || wz == 0 || hz == 0 || stride < wz {
            self.reset();
            return default;
        }
        // Largest index we will ever touch: (h-1)*stride + (w-1) + offset.
        let max_idx = match offset
            .checked_add((hz - 1).checked_mul(stride).unwrap_or(usize::MAX))
            .and_then(|v| v.checked_add(wz - 1))
        {
            Some(v) => v,
            None => {
                self.reset();
                return default;
            }
        };
        if max_idx >= luma.len() {
            self.reset();
            return default;
        }

        // --- Resolve the evaluated region (union of zone bboxes). ------------
        let geom = match region_bbox(zones, wz, hz) {
            Some(g) => g,
            None => {
                self.reset();
                return default;
            }
        };

        // --- Resample region onto the fixed grid. ----------------------------
        let (cur, cur_mean) = downsample(luma, stride, offset, &geom);

        // Geometry (zone or resolution) changed, or first frame: store & wait.
        let same_geom = self.geom == Some(geom);
        let prev = match (&self.prev, same_geom) {
            (Some(p), true) => p.clone(),
            _ => {
                self.prev = Some(cur);
                self.prev_mean = cur_mean;
                self.geom = Some(geom);
                return default;
            }
        };

        // --- Per-block horizontal displacement via ZNCC block matching. ------
        // `evaluated` = blocks with enough texture to match reliably.
        // `disps`     = signed displacement (positive = rightward) of blocks
        //               whose best ZNCC peak is trustworthy and non-trivial.
        // `centers`   = downsampled x of each moving block (for the centroid).
        let mut evaluated: usize = 0;
        let mut disps: Vec<i32> = Vec::new();
        let mut centers: Vec<f32> = Vec::new();

        for by in 0..BY {
            for bx in 0..BX {
                let c0 = bx * PW;
                let r0 = by * PH;
                let (cur_var, cur_mu) = block_stats(&cur, c0, r0);
                if cur_var < TEX_FLOOR {
                    continue; // flat patch: matching is meaningless — not evaluated
                }
                evaluated += 1;

                // Search the previous frame for this block's content.
                // best_d is the shift applied to the PREV patch that maximises
                // correlation; content displacement = -best_d (see file header).
                let mut best_z = MATCH_MIN;
                let mut best_d: Option<i32> = None;
                for d in -SEARCH..=SEARCH {
                    let sc = c0 as i32 + d;
                    if sc < 0 || sc as usize + PW > DS_W {
                        continue; // shifted patch would fall outside the grid
                    }
                    let sc = sc as usize;
                    let z = zncc(&cur, c0, r0, cur_mu, cur_var, &prev, sc, r0);
                    if z > best_z {
                        best_z = z;
                        best_d = Some(d);
                    }
                }
                if let Some(d) = best_d {
                    let disp = -d; // rightward object motion -> positive
                    if disp.abs() >= DISP_THRESH {
                        disps.push(disp);
                        centers.push(c0 as f32 + PW as f32 * 0.5);
                    }
                }
            }
        }

        // Roll state forward (this frame becomes the reference for the next one).
        let mean_jump = (cur_mean - self.prev_mean).abs();
        self.prev = Some(cur);
        self.prev_mean = cur_mean;
        self.geom = Some(geom);

        if evaluated == 0 || disps.is_empty() {
            return default; // nothing textured, or nothing moved
        }

        // --- Aggregate into a directional verdict. ---------------------------
        let med = median_i32(&mut disps.clone());
        let sign = med.signum();
        if sign == 0 {
            // Balanced +/- displacement => no dominant direction (incoherent).
            return default;
        }
        // Coherent blocks: moving AND agreeing with the median's sign.
        let mut coherent: usize = 0;
        let mut wsum = 0.0f32; // displacement-weighted centroid accumulator
        let mut wtot = 0.0f32;
        for (i, &disp) in disps.iter().enumerate() {
            if disp.signum() == sign {
                coherent += 1;
                let wgt = disp.unsigned_abs() as f32;
                wsum += centers[i] * wgt;
                wtot += wgt;
            }
        }
        let coherence = coherent as f32 / evaluated as f32;
        let magnitude = ((med.unsigned_abs() as f32) / SEARCH as f32).clamp(0.0, 1.0);
        let dir_x = (med as f32 / SEARCH as f32).clamp(-1.0, 1.0);

        // Flicker guard: a large global-mean jump with incoherent flow is a
        // lighting/weather change, not motion. (ZNCC already suppresses the
        // displacement in that case; this is the explicit belt-and-suspenders.)
        if mean_jump > FLICKER_MEAN_JUMP && coherence < COHERENCE_MIN {
            return default;
        }

        let moving = coherence >= COHERENCE_MIN && magnitude >= MAG_FLOOR;
        if !moving {
            return MotionSignal {
                moving: false,
                coherence,
                ..default
            };
        }

        // Centroid in downsampled x -> full-frame normalised x.
        let cx_ds = if wtot > 0.0 {
            wsum / wtot
        } else {
            DS_W as f32 * 0.5
        };
        let frac = (cx_ds / DS_W as f32).clamp(0.0, 1.0);
        let full_col = geom.x0 as f32 + frac * (geom.x1 - geom.x0) as f32;
        let centroid_x = (full_col / wz as f32).clamp(0.0, 1.0);

        MotionSignal {
            moving: true,
            dir_x,
            magnitude,
            centroid_x,
            coherence,
        }
    }

    fn reset(&mut self) {
        self.prev = None;
        self.geom = None;
        self.prev_mean = 0.0;
    }
}

/// Union of the zone polygons' bounding boxes, clamped to the frame, as an
/// integer half-open rectangle. Empty `zones` (or only empty polygons) => whole
/// frame. Returns `None` only for a degenerate (zero-area) region.
fn region_bbox(zones: &[Vec<(f32, f32)>], w: usize, h: usize) -> Option<Geom> {
    let mut have = false;
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in zones {
        for &(px, py) in poly {
            if !px.is_finite() || !py.is_finite() {
                continue;
            }
            let px = px.clamp(0.0, 1.0);
            let py = py.clamp(0.0, 1.0);
            minx = minx.min(px);
            miny = miny.min(py);
            maxx = maxx.max(px);
            maxy = maxy.max(py);
            have = true;
        }
    }

    let (x0, y0, x1, y1) = if have {
        let x0 = (minx * w as f32).floor() as usize;
        let y0 = (miny * h as f32).floor() as usize;
        // ceil the far edge so a thin polygon still spans >= 1 px, then clamp.
        let x1 = ((maxx * w as f32).ceil() as usize)
            .min(w)
            .max(x0 + 1)
            .min(w);
        let y1 = ((maxy * h as f32).ceil() as usize)
            .min(h)
            .max(y0 + 1)
            .min(h);
        (x0.min(w - 1), y0.min(h - 1), x1, y1)
    } else {
        (0, 0, w, h)
    };

    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Geom {
        x0,
        y0,
        x1,
        y1,
        w,
        h,
    })
}

/// Resample the region onto the fixed DS_W x DS_H grid, averaging a bounded
/// SUBxSUB set of source samples per cell (so total reads are independent of
/// input resolution). Returns the grid and its mean.
fn downsample(luma: &[u8], stride: usize, offset: usize, g: &Geom) -> (Vec<f32>, f32) {
    let mut out = vec![0.0f32; DS_W * DS_H];
    let rw = (g.x1 - g.x0) as f32;
    let rh = (g.y1 - g.y0) as f32;
    let step_x = rw / DS_W as f32;
    let step_y = rh / DS_H as f32;
    let xmax = (g.x1 - 1) as f32;
    let ymax = (g.y1 - 1) as f32;
    let x0f = g.x0 as f32;
    let y0f = g.y0 as f32;
    let sub_c = (SUB - 1) as f32 * 0.5;

    let mut sum = 0.0f32;
    for oy in 0..DS_H {
        let cy = y0f + (oy as f32 + 0.5) * step_y;
        for ox in 0..DS_W {
            let cx = x0f + (ox as f32 + 0.5) * step_x;
            let mut acc = 0.0f32;
            for sj in 0..SUB {
                let sy = cy + (sj as f32 - sub_c) * (step_y / SUB as f32);
                let ry = sy.clamp(y0f, ymax) as usize;
                let base = offset + ry * stride;
                for si in 0..SUB {
                    let sx = cx + (si as f32 - sub_c) * (step_x / SUB as f32);
                    let rx = sx.clamp(x0f, xmax) as usize;
                    acc += luma[base + rx] as f32;
                }
            }
            let v = acc / (SUB * SUB) as f32;
            out[oy * DS_W + ox] = v;
            sum += v;
        }
    }
    (out, sum / (DS_W * DS_H) as f32)
}

/// Mean and variance (population) of a PW x PH block at (c0,r0) in a DS grid.
fn block_stats(img: &[f32], c0: usize, r0: usize) -> (f32, f32) {
    let n = (PW * PH) as f32;
    let mut sum = 0.0f32;
    let mut sq = 0.0f32;
    for r in r0..r0 + PH {
        let base = r * DS_W;
        for c in c0..c0 + PW {
            let v = img[base + c];
            sum += v;
            sq += v * v;
        }
    }
    let mean = sum / n;
    let var = (sq / n - mean * mean).max(0.0);
    (var, mean)
}

/// Zero-mean normalised cross-correlation between the current block at
/// (cc,cr) — whose mean `cur_mu` and variance `cur_var` are precomputed — and a
/// previous-frame patch at (pc,pr). Result in [-1, 1]; 0 for a flat prev patch.
fn zncc(
    cur: &[f32],
    cc: usize,
    cr: usize,
    cur_mu: f32,
    cur_var: f32,
    prev: &[f32],
    pc: usize,
    pr: usize,
) -> f32 {
    let n = (PW * PH) as f32;
    // Previous-patch mean.
    let mut psum = 0.0f32;
    for r in 0..PH {
        let pbase = (pr + r) * DS_W + pc;
        for c in 0..PW {
            psum += prev[pbase + c];
        }
    }
    let pmu = psum / n;

    let mut cross = 0.0f32;
    let mut pvarn = 0.0f32; // sum of squared prev deviations
    for r in 0..PH {
        let cbase = (cr + r) * DS_W + cc;
        let pbase = (pr + r) * DS_W + pc;
        for c in 0..PW {
            let cd = cur[cbase + c] - cur_mu;
            let pd = prev[pbase + c] - pmu;
            cross += cd * pd;
            pvarn += pd * pd;
        }
    }
    let cur_norm = (cur_var * n).sqrt(); // sqrt(sum of squared cur deviations)
    let prev_norm = pvarn.sqrt();
    let den = cur_norm * prev_norm;
    if den <= f32::EPSILON {
        return 0.0;
    }
    cross / den
}

/// Median of a slice (mutates it via sort). Empty slice -> 0.
fn median_i32(v: &mut [i32]) -> i32 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    // Build a luma frame (stride == w, offset == 0). A textured, static
    // background plus a large textured object band whose texture translates with
    // `shift` (positive = object shifted rightward). `add`/`mul` apply a global
    // affine brightness change (flicker) to EVERY pixel after compositing, with
    // saturation to [0,255]. `moving_half` restricts the moving object to the
    // right half of the frame (for the zone test).
    fn make_frame(
        w: usize,
        h: usize,
        shift: i32,
        add: f32,
        mul: f32,
        right_half_only: bool,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; w * h];
        let obj_x0 = (w as f32 * 0.20) as i32;
        let obj_x1 = (w as f32 * 0.80) as i32;
        let split = if right_half_only { w as i32 / 2 } else { 0 };
        for y in 0..h {
            for x in 0..w {
                let xf = x as f32;
                let yf = y as f32;
                // Multi-frequency static background (keeps blocks textured).
                let mut v = 60.0
                    + 22.0 * (xf * 0.33).sin()
                    + 18.0 * (yf * 0.51).sin()
                    + 12.0 * ((xf + yf) * 0.19).sin();

                // Object band: present when the (shifted) screen x lies in the
                // object window; its texture is a function of object-local x so
                // it moves rigidly with `shift`.
                let sx = x as i32 - shift;
                let in_band = sx >= obj_x0 && sx < obj_x1 && (x as i32) >= split;
                if in_band {
                    // Broadband, non-periodic object texture (three incommensurate
                    // horizontal frequencies) so its autocorrelation has a single
                    // sharp peak inside the search window — no aliasing / false
                    // block match. Tied to object-local x, so it translates rigidly.
                    let ox = sx as f32;
                    v = 60.0
                        + 20.0 * (ox * 0.61).sin()
                        + 16.0 * (ox * 0.29).sin()
                        + 14.0 * (ox * 0.13).sin()
                        + 16.0 * (yf * 0.43 + PI * 0.25).sin();
                }
                let v = (v * mul + add).clamp(0.0, 255.0);
                buf[y * w + x] = v as u8;
            }
        }
        buf
    }

    fn est(m: &mut MotionEstimator, f: &[u8], w: usize, h: usize) -> MotionSignal {
        m.estimate(f, w as u32, h as u32, w as u32, 0, &[])
    }

    #[test]
    fn first_call_is_default() {
        let (w, h) = (240, 120);
        let f = make_frame(w, h, 0, 0.0, 1.0, false);
        let mut m = MotionEstimator::new();
        let s = est(&mut m, &f, w, h);
        assert!(!s.moving);
        assert_eq!(s.dir_x, 0.0);
        assert_eq!(s.magnitude, 0.0);
    }

    #[test]
    fn object_shifted_right_moves_rightward() {
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, false);
        let cur = make_frame(w, h, 8, 0.0, 1.0, false);
        let mut m = MotionEstimator::new();
        assert!(!est(&mut m, &prev, w, h).moving); // primes prev
        let s = est(&mut m, &cur, w, h);
        assert!(s.moving, "expected motion, got {:?}", s);
        assert!(s.dir_x > 0.0, "expected rightward, got {:?}", s);
        assert!(s.magnitude > 0.0);
        assert!(s.coherence >= COHERENCE_MIN);
        // Centroid should land near the object's mid-band (~0.5).
        assert!(
            (0.25..=0.75).contains(&s.centroid_x),
            "centroid {:?}",
            s.centroid_x
        );
    }

    #[test]
    fn object_shifted_left_moves_leftward() {
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, false);
        let cur = make_frame(w, h, -8, 0.0, 1.0, false);
        let mut m = MotionEstimator::new();
        est(&mut m, &prev, w, h);
        let s = est(&mut m, &cur, w, h);
        assert!(s.moving, "expected motion, got {:?}", s);
        assert!(s.dir_x < 0.0, "expected leftward, got {:?}", s);
    }

    #[test]
    fn global_brightness_step_is_not_motion() {
        // Additive flicker: +45 to every pixel, no motion. MUST be rejected.
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, false);
        let cur = make_frame(w, h, 0, 45.0, 1.0, false);
        let mut m = MotionEstimator::new();
        est(&mut m, &prev, w, h);
        let s = est(&mut m, &cur, w, h);
        assert!(!s.moving, "additive flicker read as motion: {:?}", s);
    }

    #[test]
    fn multiplicative_brightness_is_not_motion() {
        // Multiplicative flicker: scale every pixel by 1.35, no motion.
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, false);
        let cur = make_frame(w, h, 0, 0.0, 1.35, false);
        let mut m = MotionEstimator::new();
        est(&mut m, &prev, w, h);
        let s = est(&mut m, &cur, w, h);
        assert!(!s.moving, "multiplicative flicker read as motion: {:?}", s);
    }

    #[test]
    fn static_frame_is_not_motion() {
        let (w, h) = (240, 120);
        let f = make_frame(w, h, 0, 0.0, 1.0, false);
        let mut m = MotionEstimator::new();
        est(&mut m, &f, w, h);
        let s = est(&mut m, &f, w, h);
        assert!(!s.moving, "identical frames read as motion: {:?}", s);
    }

    #[test]
    fn incoherent_noise_is_not_motion() {
        // Two independent pseudo-random frames: textured but incoherent.
        let (w, h) = (200, 120);
        let noise = |seed: u64| -> Vec<u8> {
            let mut s = seed;
            (0..w * h)
                .map(|_| {
                    // xorshift64
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    (s & 0xff) as u8
                })
                .collect()
        };
        let prev = noise(0x1234_5678_9abc_def0);
        let cur = noise(0x0fed_cba9_8765_4321);
        let mut m = MotionEstimator::new();
        est(&mut m, &prev, w, h);
        let s = est(&mut m, &cur, w, h);
        assert!(!s.moving, "random noise read as coherent motion: {:?}", s);
    }

    #[test]
    fn zone_restricts_evaluation() {
        // Object moves ONLY in the right half. A left-half zone must see nothing;
        // a right-half zone must see the motion.
        // A narrow zone resamples to DS_W, magnifying the per-frame shift; keep
        // it inside the bounded search window (fast objects in tight zones are an
        // inherent block-matching limit, not what this test exercises).
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, true);
        let cur = make_frame(w, h, 5, 0.0, 1.0, true);

        let left_zone: Vec<Vec<(f32, f32)>> =
            vec![vec![(0.0, 0.0), (0.45, 0.0), (0.45, 1.0), (0.0, 1.0)]];
        let right_zone: Vec<Vec<(f32, f32)>> =
            vec![vec![(0.55, 0.0), (1.0, 0.0), (1.0, 1.0), (0.55, 1.0)]];

        let mut ml = MotionEstimator::new();
        ml.estimate(&prev, w as u32, h as u32, w as u32, 0, &left_zone);
        let sl = ml.estimate(&cur, w as u32, h as u32, w as u32, 0, &left_zone);
        assert!(!sl.moving, "left zone (static) read as motion: {:?}", sl);

        let mut mr = MotionEstimator::new();
        mr.estimate(&prev, w as u32, h as u32, w as u32, 0, &right_zone);
        let sr = mr.estimate(&cur, w as u32, h as u32, w as u32, 0, &right_zone);
        assert!(sr.moving, "right zone (moving) missed motion: {:?}", sr);
        assert!(sr.dir_x > 0.0);
    }

    #[test]
    fn geometry_change_resets_to_default() {
        let (w, h) = (240, 120);
        let prev = make_frame(w, h, 0, 0.0, 1.0, false);
        let cur = make_frame(w, h, 8, 0.0, 1.0, false);
        let mut m = MotionEstimator::new();
        est(&mut m, &prev, w, h); // primes at full-frame geometry
                                  // Switch to a zone: geometry differs -> treated as first frame.
        let zone: Vec<Vec<(f32, f32)>> = vec![vec![(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)]];
        let s = m.estimate(&cur, w as u32, h as u32, w as u32, 0, &zone);
        assert!(!s.moving, "geometry change should default, got {:?}", s);
    }

    #[test]
    fn malformed_input_returns_default() {
        let mut m = MotionEstimator::new();
        // Empty luma.
        assert!(!m.estimate(&[], 100, 100, 100, 0, &[]).moving);
        // Zero dimensions.
        let buf = vec![0u8; 100];
        assert!(!m.estimate(&buf, 0, 10, 10, 0, &[]).moving);
        assert!(!m.estimate(&buf, 10, 0, 10, 0, &[]).moving);
        // Stride smaller than width.
        assert!(!m.estimate(&buf, 10, 10, 4, 0, &[]).moving);
        // Buffer too small for the declared geometry.
        assert!(!m.estimate(&buf, 100, 100, 100, 0, &[]).moving);
    }
}
