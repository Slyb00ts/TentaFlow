// =============================================================================
// File: benches/camera_inference_batch.rs — RF-DETR cross-camera batch scaling
// =============================================================================
//
// Measures the fleet throughput lever: one detector `Session::run` over a batch
// of N camera frames vs N=1. Reports total latency + images/s per batch size so
// the inflection (when batching wins) is visible. Requires the deployed model
// at `vision_models_dir()/rfdetr-base.onnx`; skips cleanly when absent.
//
// Run: cargo bench --features inference-vision-gpu --bench camera_inference_batch

#![cfg(feature = "inference-vision-gpu")]

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use tentaflow_core::vision::detector_rfdetr::RfDetrDetector;

fn bench_batch(c: &mut Criterion) {
    let detector = match RfDetrDetector::load() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("camera_inference_batch: skipping — model not available: {e:#}");
            return;
        }
    };

    // Synthetic 1080p-ish frame; content is irrelevant to inference cost.
    let (w, h) = (1600u32, 1200u32);
    let frame = vec![128u8; (w * h * 3) as usize];

    let mut group = c.benchmark_group("rfdetr_detect_batch");
    for &n in &[1usize, 4, 8, 16] {
        let frames: Vec<(&[u8], u32, u32)> = (0..n).map(|_| (frame.as_slice(), w, h)).collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("batch_{n}"), |b| {
            b.iter(|| detector.detect_batch(&frames, None).expect("detect_batch"));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_batch);
criterion_main!(benches);
