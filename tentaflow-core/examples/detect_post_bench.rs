// =============================================================================
// File: examples/detect_post_bench.rs — RF-DETR host fill + decode micro-bench
// =============================================================================
//
// CPU-only micro-bench for the two CPU-side stages of the detector hot path,
// on a synthetic batch of 8 560×560 frames with the REAL head dims
// (queries=300 from the exported graph, label_dim=18 = 17 classes +
// background):
//   (a) host tensor fill — `fill_frame` ×8 into the flat NCHW buffer, plus the
//       ort `Value::from_array` construction cost on top,
//   (b) `decode_detr_batch` — shared by the host-tensor AND device-tensor
//       (NVDEC) paths, i.e. the only per-batch CPU decode cost in production.
//
// Each stage is timed against a BASELINE copy of the pre-optimization code and
// ASSERTED bit-identical to it (buffer f32 bits / full Detection fields), so
// the speedup numbers and the equivalence proof come from one run. No GPU or
// model needed; the ort runtime dylib is only required for the Value section.
//
// Run:
//   cargo run -p tentaflow-core --release \
//     --features inference-vision-gpu,inference-supertonic \
//     --example detect_post_bench

use std::time::Instant;

use tentaflow_core::services::detection_bus::Detection;
use tentaflow_core::vision::detector_rfdetr::{decode_detr_batch, fill_frame, RESOLUTION};
use tentaflow_core::vision::rfdetr_post::sigmoid;

const BATCH: usize = 8;
const QUERIES: usize = 300;
const LABEL_DIM: usize = 18; // 17 real classes + background slot
const NUM_CLASSES: usize = 17;

/// Per-channel ImageNet normalization — must mirror `detector_rfdetr::{MEAN,STD}`
/// (private there) for the baseline copy below.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// xorshift64* — deterministic synthetic data without pulling in `rand`.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn f32(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// BASELINE fill: verbatim copy of the pre-optimization `fill_frame` fast path
/// + resize fallback (per-pixel loop, strided per-channel writes, inline math).
fn baseline_fill_frame(data: &mut [f32], bi: usize, rgb: &[u8], w: u32, h: u32) {
    let res = RESOLUTION as usize;
    let plane = res * res;
    let base = bi * 3 * plane;
    if w == RESOLUTION && h == RESOLUTION && rgb.len() == plane * 3 {
        for y in 0..res {
            for x in 0..res {
                let p = (y * res + x) * 3;
                for c in 0..3 {
                    let v = rgb[p + c] as f32 / 255.0;
                    data[base + c * plane + y * res + x] = (v - MEAN[c]) / STD[c];
                }
            }
        }
        return;
    }
    let resized =
        tentaflow_core::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION).unwrap();
    for y in 0..res {
        for x in 0..res {
            let p = (y * res + x) * 3;
            for c in 0..3 {
                let v = resized[p + c] as f32 / 255.0;
                data[base + c * plane + y * res + x] = (v - MEAN[c]) / STD[c];
            }
        }
    }
}

/// BASELINE decode: verbatim copy of the pre-optimization
/// `rfdetr_post::postprocess_image` (per-query argmax + unconditional sigmoid),
/// applied per batch slot exactly like `decode_detr_batch`.
fn baseline_decode(
    dets: &[f32],
    labels: &[f32],
    n: usize,
    classes: &[String],
    threshold: Option<f32>,
) -> Vec<Vec<Detection>> {
    let score_threshold = threshold.unwrap_or(0.5);
    let mut results = Vec::with_capacity(n);
    for bi in 0..n {
        let d = &dets[bi * QUERIES * 4..(bi + 1) * QUERIES * 4];
        let l = &labels[bi * QUERIES * LABEL_DIM..(bi + 1) * QUERIES * LABEL_DIM];
        let mut items = Vec::new();
        for q in 0..QUERIES {
            let logits = &l[q * LABEL_DIM..q * LABEL_DIM + LABEL_DIM];
            let mut best_idx = 0usize;
            let mut best_logit = f32::NEG_INFINITY;
            for (idx, &lg) in logits.iter().take(NUM_CLASSES).enumerate() {
                if lg > best_logit {
                    best_logit = lg;
                    best_idx = idx;
                }
            }
            let score = sigmoid(best_logit);
            if score <= score_threshold {
                continue;
            }
            let base = q * 4;
            let cx = d[base];
            let cy = d[base + 1];
            let bw = d[base + 2];
            let bh = d[base + 3];
            let x1 = (cx - bw / 2.0).clamp(0.0, 1.0);
            let y1 = (cy - bh / 2.0).clamp(0.0, 1.0);
            let x2 = (cx + bw / 2.0).clamp(0.0, 1.0);
            let y2 = (cy + bh / 2.0).clamp(0.0, 1.0);
            items.push(Detection {
                klasa: classes[best_idx].clone(),
                bbox: [x1, y1, x2 - x1, y2 - y1],
                score,
                stan: Vec::new(),
                tekst: None,
                track_id: 0,
                vx: 0.,
                vy: 0.,
            });
        }
        results.push(items);
    }
    results
}

fn assert_detections_bit_identical(a: &[Vec<Detection>], b: &[Vec<Detection>], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: batch length");
    for (bi, (ia, ib)) in a.iter().zip(b).enumerate() {
        assert_eq!(ia.len(), ib.len(), "{what}: slot {bi} detection count");
        for (da, db) in ia.iter().zip(ib) {
            assert_eq!(da.klasa, db.klasa, "{what}: slot {bi} class");
            assert_eq!(
                da.score.to_bits(),
                db.score.to_bits(),
                "{what}: slot {bi} score bits"
            );
            for c in 0..4 {
                assert_eq!(
                    da.bbox[c].to_bits(),
                    db.bbox[c].to_bits(),
                    "{what}: slot {bi} bbox[{c}] bits"
                );
            }
        }
    }
}

/// min/avg over `iters` timed runs of `f` (warmup runs excluded).
fn time_ms(iters: usize, warmup: usize, mut f: impl FnMut()) -> (f64, f64) {
    for _ in 0..warmup {
        f();
    }
    let mut min = f64::MAX;
    let mut total = 0.0;
    for _ in 0..iters {
        let t = Instant::now();
        f();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        min = min.min(ms);
        total += ms;
    }
    (min, total / iters as f64)
}

fn main() {
    let res = RESOLUTION as usize;
    let plane = res * res;
    let buf_len = BATCH * 3 * plane;
    let mut rng = Rng(0x5EED_CAFE_F00D_D00D);

    // ---- synthetic inputs -------------------------------------------------
    // 8 pre-scaled 560×560 RGB frames (the fill fast path — production shape).
    let frames: Vec<Vec<u8>> = (0..BATCH)
        .map(|_| (0..plane * 3).map(|_| (rng.next() >> 56) as u8).collect())
        .collect();
    // One 1280×720 frame to exercise the resize-fallback normalize too.
    let big = (0..1280usize * 720 * 3)
        .map(|_| (rng.next() >> 56) as u8)
        .collect::<Vec<u8>>();

    // RF-DETR-like head output: background-dominated logits in [-8,-2], ~5% of
    // queries get one class boosted into [0,6] so a realistic handful pass 0.5.
    let mut labels = vec![0f32; BATCH * QUERIES * LABEL_DIM];
    for q in 0..BATCH * QUERIES {
        for c in 0..LABEL_DIM {
            labels[q * LABEL_DIM + c] = -8.0 + 6.0 * rng.f32();
        }
        if rng.f32() < 0.05 {
            let c = (rng.next() as usize) % NUM_CLASSES;
            labels[q * LABEL_DIM + c] = 6.0 * rng.f32();
        }
    }
    let dets: Vec<f32> = (0..BATCH * QUERIES * 4).map(|_| rng.f32()).collect();
    let classes: Vec<String> = (0..NUM_CLASSES).map(|i| format!("class-{i}")).collect();

    // ---- correctness gates ------------------------------------------------
    let mut buf_base = vec![0f32; buf_len];
    let mut buf_new = vec![0f32; buf_len];
    for (bi, f) in frames.iter().enumerate() {
        baseline_fill_frame(&mut buf_base, bi, f, RESOLUTION, RESOLUTION);
        fill_frame(&mut buf_new, bi, f, RESOLUTION, RESOLUTION).unwrap();
    }
    // Resize fallback parity (slot 0 only).
    let mut rb = vec![0f32; 3 * plane];
    let mut rn = vec![0f32; 3 * plane];
    baseline_fill_frame(&mut rb, 0, &big, 1280, 720);
    fill_frame(&mut rn, 0, &big, 1280, 720).unwrap();
    assert!(
        buf_base
            .iter()
            .zip(&buf_new)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "fill fast path not bit-identical"
    );
    assert!(
        rb.iter().zip(&rn).all(|(a, b)| a.to_bits() == b.to_bits()),
        "fill resize path not bit-identical"
    );

    for thr in [None, Some(0.05f32), Some(0.9f32), Some(0.0f32)] {
        let base = baseline_decode(&dets, &labels, BATCH, &classes, thr);
        let new = decode_detr_batch(&dets, &labels, BATCH, QUERIES, LABEL_DIM, &classes, thr);
        assert_detections_bit_identical(&base, &new, &format!("decode thr={thr:?}"));
    }
    let n_pass = decode_detr_batch(&dets, &labels, BATCH, QUERIES, LABEL_DIM, &classes, None)
        .iter()
        .map(|v| v.len())
        .sum::<usize>();
    println!("correctness: fill + decode bit-identical to baseline ✓ ({n_pass} detections pass 0.5 across the batch)");

    // ---- (a) host tensor fill ----------------------------------------------
    let fill_iters = 60;
    let (min_b, avg_b) = time_ms(fill_iters, 5, || {
        let mut data = vec![0f32; buf_len];
        for (bi, f) in frames.iter().enumerate() {
            baseline_fill_frame(&mut data, bi, f, RESOLUTION, RESOLUTION);
        }
        std::hint::black_box(&data);
    });
    let (min_n, avg_n) = time_ms(fill_iters, 5, || {
        let mut data = vec![0f32; buf_len];
        for (bi, f) in frames.iter().enumerate() {
            fill_frame(&mut data, bi, f, RESOLUTION, RESOLUTION).unwrap();
        }
        std::hint::black_box(&data);
    });
    // Production shape: `detect_batch` fills a reused thread-local scratch (no
    // per-batch alloc/page faults) and lends it to ort as a borrowed tensor.
    let mut scratch = vec![0f32; buf_len];
    let (min_s, avg_s) = time_ms(fill_iters, 5, || {
        for (bi, f) in frames.iter().enumerate() {
            fill_frame(&mut scratch, bi, f, RESOLUTION, RESOLUTION).unwrap();
        }
        std::hint::black_box(&scratch);
    });
    println!(
        "fill  (batch=8, 560×560): baseline alloc+fill min {min_b:.3} ms / avg {avg_b:.3} ms | \
         optimized alloc+fill min {min_n:.3} ms / avg {avg_n:.3} ms | \
         optimized reused-scratch (production) min {min_s:.3} ms / avg {avg_s:.3} ms | speedup ×{:.2}",
        min_b / min_s
    );

    // Value construction on top of the fill buffer: baseline (ndarray Array4 →
    // from_array) vs current ((shape, Vec) → from_array). The Vec clone cost is
    // timed alone first so the construction delta is visible.
    tentaflow_core::vision::ort_common::ensure_ort_dylib();
    let (min_clone, _) = time_ms(40, 3, || {
        std::hint::black_box(buf_new.clone());
    });
    let (min_nd, _) = time_ms(40, 3, || {
        let v = buf_new.clone();
        let arr = ndarray::Array4::from_shape_vec((BATCH, 3, res, res), v).unwrap();
        let val = ort::value::Value::from_array(arr).unwrap();
        std::hint::black_box(&val);
    });
    let (min_tuple, _) = time_ms(40, 3, || {
        let v = buf_new.clone();
        let val = ort::value::Value::from_array(([BATCH, 3, res, res], v)).unwrap();
        std::hint::black_box(&val);
    });
    println!(
        "value (30 MB buffer): clone alone {min_clone:.3} ms | clone+Array4+from_array {min_nd:.3} ms \
         (construct ≈{:.3} ms) | clone+(shape,Vec)+from_array {min_tuple:.3} ms (construct ≈{:.3} ms)",
        min_nd - min_clone,
        min_tuple - min_clone
    );

    // ---- (b) decode ---------------------------------------------------------
    let dec_iters = 3000;
    let (min_db, avg_db) = time_ms(dec_iters, 200, || {
        std::hint::black_box(baseline_decode(&dets, &labels, BATCH, &classes, None));
    });
    let (min_dn, avg_dn) = time_ms(dec_iters, 200, || {
        std::hint::black_box(decode_detr_batch(
            &dets, &labels, BATCH, QUERIES, LABEL_DIM, &classes, None,
        ));
    });
    println!(
        "decode (batch=8, 300 queries × 18 logits): baseline min {:.1} µs / avg {:.1} µs | \
         optimized min {:.1} µs / avg {:.1} µs | speedup ×{:.2}",
        min_db * 1e3,
        avg_db * 1e3,
        min_dn * 1e3,
        avg_dn * 1e3,
        min_db / min_dn
    );
    println!(
        "share: baseline fill {:.3} ms vs baseline decode {:.3} ms per batch → fill dominates ×{:.0}",
        min_b,
        min_db,
        min_b / min_db
    );
}
