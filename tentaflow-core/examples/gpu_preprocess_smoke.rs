// =============================================================================
// File: examples/gpu_preprocess_smoke.rs — GPU-resident preprocess correctness+speed gate
// =============================================================================
//
// Loads the real state classifier, builds K synthetic RGB crops of varied sizes,
// then runs BOTH paths on the same crops:
//   * `classify_batch`     — CPU preprocess → ORT (host input tensor),
//   * `classify_batch_gpu` — fused CUDA resize+normalize → ORT device tensor
//                            (zero host→device copy).
// It ASSERTS the GPU labels match the CPU labels (the bit-parity correctness
// gate — if the kernel's bilinear/normalize drifts, logits drift and argmax
// flips), then times N iterations of each and prints CPU vs GPU-resident
// ms/batch + speedup. Exits non-zero on a correctness mismatch.
//
// Run (needs a CUDA GPU + a provisioned model_stan.onnx + stan-classes.json in
// the vision models dir):
//   cargo run -p tentaflow-core --release \
//     --features inference-vision-gpu,inference-supertonic \
//     --example gpu_preprocess_smoke

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tentaflow_core::vision::classifier_stan::StateClassifier;

/// Deterministic smooth gradient (not pure noise) so bilinear resampling on both
/// paths reads meaningful low-frequency structure.
fn synth_crop(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let (wu, hu) = (w as usize, h as usize);
    let mut v = vec![0u8; wu * hu * 3];
    for y in 0..hu {
        for x in 0..wu {
            let o = (y * wu + x) * 3;
            v[o] = ((x * 255) / wu) as u8;
            v[o + 1] = ((y * 255) / hu) as u8;
            v[o + 2] = (((x + y).wrapping_add(seed as usize) * 255) / (wu + hu)) as u8;
        }
    }
    v
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().try_init().ok();

    let clf = StateClassifier::load()?;

    // Varied crop sizes: upscale + downscale, odd + even, extreme aspect ratios.
    let sizes: [(u32, u32); 8] = [
        (64, 48),
        (200, 200),
        (33, 120),
        (512, 288),
        (150, 150),
        (17, 240),
        (400, 90),
        (96, 96),
    ];

    let cpu_crops: Vec<(Arc<[u8]>, u32, u32)> = sizes
        .iter()
        .enumerate()
        .map(|(i, &(w, h))| (Arc::from(synth_crop(w, h, i as u64 * 7 + 1)), w, h))
        .collect();
    let gpu_crops: Vec<(&[u8], u32, u32)> = cpu_crops
        .iter()
        .map(|(b, w, h)| (b.as_ref(), *w, *h))
        .collect();

    let cpu_labels = clf.classify_batch(&cpu_crops)?;
    let gpu_labels = clf.classify_batch_gpu(&gpu_crops)?;

    // Correctness gate: identical argmax labels per crop.
    let mut mismatches = 0usize;
    for (i, (c, g)) in cpu_labels.iter().zip(gpu_labels.iter()).enumerate() {
        if c != g {
            mismatches += 1;
            eprintln!(
                "  crop {i} ({}x{}): CPU={c:?} GPU={g:?}",
                sizes[i].0, sizes[i].1
            );
        }
    }
    let correct =
        mismatches == 0 && cpu_labels.len() == gpu_labels.len() && cpu_labels.len() == sizes.len();

    // Timing: warm both paths, then N timed iterations each.
    let iters = 50u32;
    for _ in 0..3 {
        let _ = clf.classify_batch(&cpu_crops)?;
        let _ = clf.classify_batch_gpu(&gpu_crops)?;
    }
    let t = Instant::now();
    for _ in 0..iters {
        let _ = clf.classify_batch(&cpu_crops)?;
    }
    let cpu_ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;

    let t = Instant::now();
    for _ in 0..iters {
        let _ = clf.classify_batch_gpu(&gpu_crops)?;
    }
    let gpu_ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;

    println!("batch = {} crops", sizes.len());
    println!("CPU-preprocess    : {cpu_ms:.3} ms/batch");
    println!("GPU-resident      : {gpu_ms:.3} ms/batch");
    println!("speedup (CPU/GPU) : {:.2}x", cpu_ms / gpu_ms.max(1e-9));

    if correct {
        println!("CORRECTNESS: PASS ({} crops, labels match)", cpu_labels.len());
        Ok(())
    } else {
        println!(
            "CORRECTNESS: FAIL ({mismatches} mismatched of {})",
            cpu_labels.len()
        );
        std::process::exit(1);
    }
}
