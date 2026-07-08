// =============================================================================
// File: vision/ocr_prep.rs — OCR crop pre-processing: perspective deskew + dumps
// =============================================================================
//
// Cameras watch trucks/tankers from the side, so license plates reach the OCR
// model perspective-skewed. The detector box is axis-aligned and often tight,
// so the raw crop both clips characters and feeds a keystoned plate into a model
// that expects an upright, frontal rectangle. This module rectifies the crop
// BEFORE the model:
//   1. threshold the (padded) crop to a bright plate mask (PL plates are a white
//      rectangle on a darker vehicle),
//   2. take the largest near-central component and read its four corners with the
//      document-scanner sum/diff trick (TL=min(x+y), BR=max(x+y), TR=max(x-y),
//      BL=min(x-y)),
//   3. validate the quad (aspect / area / min-side) so truck background never
//      produces a bogus warp, then
//   4. axis-aligned crop when near-frontal (no interpolation softening) or a
//      perspective warp when actually skewed.
// If no confident plate quad is found we return `None` and the caller keeps the
// current bilinear-stretch path — deskew is a strictly fallback-protected
// enhancement, never worse than today.
//
// Toggles (zero cost when unset):
//   * `TENTAFLOW_OCR_DESKEW=0` disables deskew (A/B against the stretch path).
//   * `TENTAFLOW_OCR_DUMP_DIR=<dir>` dumps, per OCR call, the raw crop, the
//     rectified crop (when any) and the exact model-input tensor as PNGs named
//     with the read result + score, so a human can SEE what the model saw.

#![cfg(feature = "inference-vision-gpu")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// `TENTAFLOW_OCR_DESKEW` — default ON. `0`/`false`/`off`/`no` disable it.
pub fn deskew_enabled() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| match std::env::var("TENTAFLOW_OCR_DESKEW") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// `TENTAFLOW_OCR_DUMP_DIR` — when set to an existing/creatable dir, OCR calls
/// dump their crops there. `None` (unset) means every dump call is a no-op.
pub fn dump_dir() -> Option<&'static Path> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let raw = std::env::var("TENTAFLOW_OCR_DUMP_DIR").ok()?;
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            let dir = PathBuf::from(raw);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("[ocr_prep] TENTAFLOW_OCR_DUMP_DIR {} unusable: {e}", dir.display());
                return None;
            }
            tracing::info!("[ocr_prep] OCR crop dump enabled → {}", dir.display());
            Some(dir)
        })
        .as_deref()
}

/// A 2D point in crop-pixel coordinates.
#[derive(Clone, Copy, Debug)]
struct Pt {
    x: f32,
    y: f32,
}

fn dist(a: Pt, b: Pt) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

/// Perspective-rectify a plate crop (RGB24, `w*h*3`). Returns the rectified RGB
/// crop + its dims, or `None` when no confident plate quad is found (caller then
/// keeps the current stretch path). Aspect bounds target PL plates; the same
/// classical pipeline works for any bright rectangular plate.
pub fn deskew_plate_rgb(rgb: &[u8], w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    deskew_rect_rgb(rgb, w, h, 1.8, 9.0)
}

/// Core deskew usable for any near-rectangular bright sign. `min_aspect`/
/// `max_aspect` gate the detected quad's width/height ratio so a wrong (square /
/// absurd) component never triggers a warp.
pub fn deskew_rect_rgb(
    rgb: &[u8],
    w: u32,
    h: u32,
    min_aspect: f32,
    max_aspect: f32,
) -> Option<(Vec<u8>, u32, u32)> {
    let (wu, hu) = (w as usize, h as usize);
    if wu < 16 || hu < 8 || rgb.len() < wu * hu * 3 {
        return None;
    }
    let gray = luma(rgb, wu, hu);
    let thr = otsu(&gray);
    let mask = bright_mask(&gray, thr);
    let comp = largest_central_component(&mask, wu, hu)?;
    let quad = quad_from_component(&comp.labels, comp.label, wu, hu)?;
    validate_quad(&quad, wu, hu, min_aspect, max_aspect)?;

    if is_near_frontal(&quad, wu, hu) {
        axis_aligned_crop(rgb, wu, hu, &quad)
    } else {
        perspective_warp(rgb, wu, hu, &quad)
    }
}

/// BT.601 luma of an RGB24 buffer → one byte per pixel (matches the OCR grayscale).
fn luma(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut g = Vec::with_capacity(w * h);
    for px in rgb[..w * h * 3].chunks_exact(3) {
        let l = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
        g.push(l.round().clamp(0.0, 255.0) as u8);
    }
    g
}

/// Otsu's threshold on a grayscale histogram (0..=255).
fn otsu(gray: &[u8]) -> u8 {
    let mut hist = [0u32; 256];
    for &v in gray {
        hist[v as usize] += 1;
    }
    let total = gray.len() as f64;
    if total == 0.0 {
        return 128;
    }
    let sum: f64 = (0..256).map(|i| i as f64 * hist[i] as f64).sum();
    let (mut sum_b, mut w_b, mut max_var, mut thr) = (0.0f64, 0.0f64, -1.0f64, 128usize);
    for i in 0..256 {
        w_b += hist[i] as f64;
        if w_b == 0.0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0.0 {
            break;
        }
        sum_b += i as f64 * hist[i] as f64;
        let m_b = sum_b / w_b;
        let m_f = (sum - sum_b) / w_f;
        let var = w_b * w_f * (m_b - m_f) * (m_b - m_f);
        if var > max_var {
            max_var = var;
            thr = i;
        }
    }
    thr as u8
}

/// Bright-pixel mask (plate background is the brightest large region). Otsu's
/// convention puts the foreground class STRICTLY above the threshold, so a clean
/// bimodal crop (bright plate on a dark vehicle) is not swallowed whole when the
/// threshold lands on the dark class value.
fn bright_mask(gray: &[u8], thr: u8) -> Vec<bool> {
    gray.iter().map(|&v| v > thr).collect()
}

/// Chosen connected component: its label id and the full label buffer.
struct Component {
    labels: Vec<u32>,
    label: u32,
}

/// 4-connected components over the bright mask; returns the component that best
/// looks like a centered plate: area within [8%, 98%] of the crop, largest such,
/// preferring ones whose centroid is near the crop center (the plate sits in the
/// middle of a padded crop). `None` when nothing qualifies.
fn largest_central_component(mask: &[bool], w: usize, h: usize) -> Option<Component> {
    let n = w * h;
    let mut labels = vec![0u32; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next: u32 = 0;
    // Per-component running stats: area + centroid accumulators.
    let mut stats: Vec<(u64, u64, u64)> = Vec::new(); // (area, sum_x, sum_y)
    for start in 0..n {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        next += 1;
        let lbl = next;
        let (mut area, mut sx, mut sy) = (0u64, 0u64, 0u64);
        labels[start] = lbl;
        stack.push(start);
        while let Some(idx) = stack.pop() {
            let (x, y) = (idx % w, idx / w);
            area += 1;
            sx += x as u64;
            sy += y as u64;
            // 4-neighbourhood
            if x > 0 && mask[idx - 1] && labels[idx - 1] == 0 {
                labels[idx - 1] = lbl;
                stack.push(idx - 1);
            }
            if x + 1 < w && mask[idx + 1] && labels[idx + 1] == 0 {
                labels[idx + 1] = lbl;
                stack.push(idx + 1);
            }
            if y > 0 && mask[idx - w] && labels[idx - w] == 0 {
                labels[idx - w] = lbl;
                stack.push(idx - w);
            }
            if y + 1 < h && mask[idx + w] && labels[idx + w] == 0 {
                labels[idx + w] = lbl;
                stack.push(idx + w);
            }
        }
        stats.push((area, sx, sy));
    }
    if next == 0 {
        return None;
    }
    let total = (w * h) as f64;
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let diag = ((w * w + h * h) as f64).sqrt();
    let mut best: Option<(f64, u32)> = None; // (score, label)
    for (i, &(area, sx, sy)) in stats.iter().enumerate() {
        let frac = area as f64 / total;
        if !(0.08..=0.98).contains(&frac) {
            continue;
        }
        let (ccx, ccy) = (sx as f64 / area as f64, sy as f64 / area as f64);
        let center_pen = (((ccx - cx).powi(2) + (ccy - cy).powi(2)).sqrt()) / diag;
        // Prefer large + centered: area fraction minus a centering penalty.
        let score = frac - center_pen;
        if best.map(|b| score > b.0).unwrap_or(true) {
            best = Some((score, i as u32 + 1));
        }
    }
    best.map(|(_, label)| Component { labels, label })
}

/// Four corners of a labelled component via the sum/diff extrema trick, ordered
/// TL, TR, BR, BL.
fn quad_from_component(labels: &[u32], label: u32, w: usize, h: usize) -> Option<[Pt; 4]> {
    let (mut tl, mut tr, mut br, mut bl) = (
        (f32::MAX, Pt { x: 0.0, y: 0.0 }),
        (f32::MIN, Pt { x: 0.0, y: 0.0 }),
        (f32::MIN, Pt { x: 0.0, y: 0.0 }),
        (f32::MAX, Pt { x: 0.0, y: 0.0 }),
    );
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            if labels[y * w + x] != label {
                continue;
            }
            found = true;
            let (xf, yf) = (x as f32, y as f32);
            let sum = xf + yf;
            let diff = xf - yf;
            if sum < tl.0 {
                tl = (sum, Pt { x: xf, y: yf });
            }
            if sum > br.0 {
                br = (sum, Pt { x: xf, y: yf });
            }
            if diff > tr.0 {
                tr = (diff, Pt { x: xf, y: yf });
            }
            if diff < bl.0 {
                bl = (diff, Pt { x: xf, y: yf });
            }
        }
    }
    if !found {
        return None;
    }
    Some([tl.1, tr.1, br.1, bl.1])
}

/// Reject implausible quads (wrong aspect, degenerate side, tiny area) so noisy
/// truck background never warps a good frontal read into garbage.
fn validate_quad(
    q: &[Pt; 4],
    w: usize,
    h: usize,
    min_aspect: f32,
    max_aspect: f32,
) -> Option<()> {
    let top = dist(q[0], q[1]);
    let bottom = dist(q[3], q[2]);
    let left = dist(q[0], q[3]);
    let right = dist(q[1], q[2]);
    let avg_w = (top + bottom) / 2.0;
    let avg_h = (left + right) / 2.0;
    if avg_w < 12.0 || avg_h < 6.0 {
        return None;
    }
    let aspect = avg_w / avg_h;
    if !(min_aspect..=max_aspect).contains(&aspect) {
        return None;
    }
    // Opposite sides should be comparable — a wildly trapezoidal blob is not a plate.
    if (top / bottom).max(bottom / top) > 2.5 || (left / right).max(right / left) > 2.5 {
        return None;
    }
    // Quad area (shoelace) must cover a real fraction of the crop.
    let area = shoelace(q);
    let frac = area / (w as f32 * h as f32);
    if !(0.06..=0.99).contains(&frac) {
        return None;
    }
    Some(())
}

fn shoelace(q: &[Pt; 4]) -> f32 {
    let mut a = 0.0f32;
    for i in 0..4 {
        let p = q[i];
        let n = q[(i + 1) % 4];
        a += p.x * n.y - n.x * p.y;
    }
    (a / 2.0).abs()
}

/// A quad is "near frontal" when every corner sits close to its axis-aligned
/// bounding-box corner (≤6% of the larger side). Such plates get a plain crop —
/// no perspective interpolation — so frontal reads never regress from softening.
fn is_near_frontal(q: &[Pt; 4], w: usize, h: usize) -> bool {
    let (minx, maxx, miny, maxy) = bbox(q, w, h);
    let bw = (maxx - minx).max(1.0);
    let bh = (maxy - miny).max(1.0);
    let tol = 0.06 * bw.max(bh);
    let corners = [
        Pt { x: minx, y: miny },
        Pt { x: maxx, y: miny },
        Pt { x: maxx, y: maxy },
        Pt { x: minx, y: maxy },
    ];
    q.iter().zip(corners.iter()).all(|(a, b)| dist(*a, *b) <= tol)
}

fn bbox(q: &[Pt; 4], w: usize, h: usize) -> (f32, f32, f32, f32) {
    let mut minx = f32::MAX;
    let mut maxx = f32::MIN;
    let mut miny = f32::MAX;
    let mut maxy = f32::MIN;
    for p in q {
        minx = minx.min(p.x);
        maxx = maxx.max(p.x);
        miny = miny.min(p.y);
        maxy = maxy.max(p.y);
    }
    (
        minx.clamp(0.0, w as f32 - 1.0),
        maxx.clamp(0.0, w as f32 - 1.0),
        miny.clamp(0.0, h as f32 - 1.0),
        maxy.clamp(0.0, h as f32 - 1.0),
    )
}

/// Axis-aligned crop to the quad's bounding box (near-frontal plates). Removes
/// the extra detector/deskew padding without any resampling.
fn axis_aligned_crop(rgb: &[u8], w: usize, h: usize, q: &[Pt; 4]) -> Option<(Vec<u8>, u32, u32)> {
    let (minx, maxx, miny, maxy) = bbox(q, w, h);
    let x0 = minx.floor() as usize;
    let y0 = miny.floor() as usize;
    let x1 = (maxx.ceil() as usize).min(w - 1);
    let y1 = (maxy.ceil() as usize).min(h - 1);
    if x1 <= x0 + 4 || y1 <= y0 + 2 {
        return None;
    }
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = Vec::with_capacity(cw * ch * 3);
    for y in y0..=y1 {
        let row = (y * w + x0) * 3;
        out.extend_from_slice(&rgb[row..row + cw * 3]);
    }
    Some((out, cw as u32, ch as u32))
}

/// Perspective-warp the source quad (TL,TR,BR,BL) to an upright rectangle sized
/// to the plate's estimated width/height, bilinear-sampled. Inverse mapping:
/// solve a homography from the OUTPUT rectangle to the SOURCE quad, then for each
/// output pixel sample the source directly.
fn perspective_warp(rgb: &[u8], w: usize, h: usize, q: &[Pt; 4]) -> Option<(Vec<u8>, u32, u32)> {
    let top = dist(q[0], q[1]);
    let bottom = dist(q[3], q[2]);
    let left = dist(q[0], q[3]);
    let right = dist(q[1], q[2]);
    let ow = (top.max(bottom).round() as usize).clamp(16, 640);
    let oh = (left.max(right).round() as usize).clamp(8, 256);

    let dst = [
        Pt { x: 0.0, y: 0.0 },
        Pt { x: ow as f32 - 1.0, y: 0.0 },
        Pt { x: ow as f32 - 1.0, y: oh as f32 - 1.0 },
        Pt { x: 0.0, y: oh as f32 - 1.0 },
    ];
    // Homography mapping dst → src (so each output pixel maps back to a source pt).
    let hm = homography(&dst, q)?;

    let mut out = vec![0u8; ow * oh * 3];
    for oy in 0..oh {
        for ox in 0..ow {
            let (sx, sy) = apply_h(&hm, ox as f32, oy as f32);
            let px = sample_bilinear(rgb, w, h, sx, sy);
            let o = (oy * ow + ox) * 3;
            out[o] = px[0];
            out[o + 1] = px[1];
            out[o + 2] = px[2];
        }
    }
    Some((out, ow as u32, oh as u32))
}

/// Bilinear RGB sample with edge clamping.
fn sample_bilinear(rgb: &[u8], w: usize, h: usize, x: f32, y: f32) -> [u8; 3] {
    let x = x.clamp(0.0, w as f32 - 1.0);
    let y = y.clamp(0.0, h as f32 - 1.0);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut out = [0u8; 3];
    for c in 0..3 {
        let p00 = rgb[(y0 * w + x0) * 3 + c] as f32;
        let p10 = rgb[(y0 * w + x1) * 3 + c] as f32;
        let p01 = rgb[(y1 * w + x0) * 3 + c] as f32;
        let p11 = rgb[(y1 * w + x1) * 3 + c] as f32;
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        out[c] = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Solve the 3×3 homography `H` mapping `src[i] → dst[i]` (h33 = 1) from four
/// point correspondences via an 8×8 linear system with partial-pivot Gaussian
/// elimination. Returns the 9 coefficients row-major, or `None` if degenerate.
fn homography(src: &[Pt; 4], dst: &[Pt; 4]) -> Option<[f32; 9]> {
    // Build A h = b, unknowns h = [h0..h7].
    let mut a = [[0.0f64; 8]; 8];
    let mut b = [0.0f64; 8];
    for i in 0..4 {
        let (x, y) = (src[i].x as f64, src[i].y as f64);
        let (u, v) = (dst[i].x as f64, dst[i].y as f64);
        let r0 = 2 * i;
        let r1 = 2 * i + 1;
        a[r0] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u];
        b[r0] = u;
        a[r1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v];
        b[r1] = v;
    }
    let h = solve8(&mut a, &mut b)?;
    Some([
        h[0] as f32,
        h[1] as f32,
        h[2] as f32,
        h[3] as f32,
        h[4] as f32,
        h[5] as f32,
        h[6] as f32,
        h[7] as f32,
        1.0,
    ])
}

/// Apply a 3×3 homography to a 2D point (perspective divide).
fn apply_h(h: &[f32; 9], x: f32, y: f32) -> (f32, f32) {
    let d = h[6] * x + h[7] * y + h[8];
    if d.abs() < 1e-6 {
        return (x, y);
    }
    let sx = (h[0] * x + h[1] * y + h[2]) / d;
    let sy = (h[3] * x + h[4] * y + h[5]) / d;
    (sx, sy)
}

/// Gaussian elimination with partial pivoting for an 8×8 system. Returns the
/// solution vector, or `None` when the matrix is (near-)singular.
fn solve8(a: &mut [[f64; 8]; 8], b: &mut [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        // Partial pivot.
        let mut piv = col;
        let mut best = a[col][col].abs();
        for r in (col + 1)..8 {
            if a[r][col].abs() > best {
                best = a[r][col].abs();
                piv = r;
            }
        }
        if best < 1e-9 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let pivot = a[col][col];
        for r in (col + 1)..8 {
            let f = a[r][col] / pivot;
            if f == 0.0 {
                continue;
            }
            for c in col..8 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0f64; 8];
    for i in (0..8).rev() {
        let mut s = b[i];
        for c in (i + 1)..8 {
            s -= a[i][c] * x[c];
        }
        x[i] = s / a[i][i];
    }
    Some(x)
}

/// Dump one OCR sample when `TENTAFLOW_OCR_DUMP_DIR` is set. Writes the raw crop,
/// the rectified crop (when deskew produced one) and the exact model-input tensor
/// (grayscale, `gw×gh`) as PNGs whose names carry the read result + score, so a
/// human can inspect whether the plate was clipped, skewed, too small or stretched.
/// No-op (and no allocation on the caller side is required) when dumps are off.
#[allow(clippy::too_many_arguments)]
pub fn dump_ocr_sample(
    tag: &str,
    raw_rgb: &[u8],
    raw_w: u32,
    raw_h: u32,
    deskewed: Option<(&[u8], u32, u32)>,
    gray: &[u8],
    gw: u32,
    gh: u32,
    read: Option<&str>,
    score: f32,
) {
    let Some(dir) = dump_dir() else {
        return;
    };
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let label = sanitize(read.unwrap_or("none"));
    let base = format!("{tag}_{seq:08}_{label}_s{:03}", (score * 100.0) as i32);

    save_rgb(dir, &format!("{base}_raw"), raw_rgb, raw_w, raw_h);
    if let Some((d, dw, dh)) = deskewed {
        save_rgb(dir, &format!("{base}_deskew"), d, dw, dh);
    }
    save_gray(dir, &format!("{base}_tensor"), gray, gw, gh);
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    if t.is_empty() {
        "none".to_string()
    } else {
        t
    }
}

fn save_rgb(dir: &Path, name: &str, rgb: &[u8], w: u32, h: u32) {
    let need = w as usize * h as usize * 3;
    if w == 0 || h == 0 || rgb.len() < need {
        return;
    }
    if let Some(img) = image::RgbImage::from_raw(w, h, rgb[..need].to_vec()) {
        let path = dir.join(format!("{name}.png"));
        if let Err(e) = img.save(&path) {
            tracing::warn!("[ocr_prep] dump {}: {e}", path.display());
        }
    }
}

fn save_gray(dir: &Path, name: &str, gray: &[u8], w: u32, h: u32) {
    let need = w as usize * h as usize;
    if w == 0 || h == 0 || gray.len() < need {
        return;
    }
    let mut rgb = Vec::with_capacity(need * 3);
    for &g in &gray[..need] {
        rgb.extend_from_slice(&[g, g, g]);
    }
    save_rgb(dir, name, &rgb, w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic axis-aligned bright plate must be found and treated as
    /// near-frontal → axis-aligned crop (no perspective warp).
    #[test]
    fn frontal_plate_is_cropped_not_warped() {
        let (w, h) = (120usize, 60usize);
        let mut rgb = vec![30u8; w * h * 3]; // dark background
        // Bright plate rectangle in the centre.
        for y in 12..48 {
            for x in 20..100 {
                let o = (y * w + x) * 3;
                rgb[o] = 235;
                rgb[o + 1] = 235;
                rgb[o + 2] = 235;
            }
        }
        let out = deskew_rect_rgb(&rgb, w as u32, h as u32, 1.2, 9.0);
        let (_, ow, oh) = out.expect("plate found");
        // Cropped to ~plate size (80×36), not the full crop.
        assert!(ow >= 70 && ow <= 90, "ow={ow}");
        assert!(oh >= 28 && oh <= 44, "oh={oh}");
    }

    /// No bright rectangle → no quad → fall back to the stretch path.
    #[test]
    fn no_plate_returns_none() {
        let (w, h) = (100usize, 50usize);
        let rgb = vec![40u8; w * h * 3];
        assert!(deskew_rect_rgb(&rgb, w as u32, h as u32, 1.8, 9.0).is_none());
    }

    #[test]
    fn homography_identity_maps_points() {
        let sq = [
            Pt { x: 0.0, y: 0.0 },
            Pt { x: 10.0, y: 0.0 },
            Pt { x: 10.0, y: 5.0 },
            Pt { x: 0.0, y: 5.0 },
        ];
        let h = homography(&sq, &sq).expect("solvable");
        let (x, y) = apply_h(&h, 4.0, 3.0);
        assert!((x - 4.0).abs() < 1e-3 && (y - 3.0).abs() < 1e-3);
    }
}
