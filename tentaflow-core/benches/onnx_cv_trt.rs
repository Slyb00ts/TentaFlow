// =============================================================================
// File: benches/onnx_cv_trt.rs — ONNX detector: FP32-CPU vs FP16-TensorRT
// =============================================================================
//
// Measures raw `Session::run` latency of the RF-DETR detector graph at batch
// 1/8/16 on two EP stacks: plain FP32 CPU and FP16 TensorRT with the explicit
// batch shape profile from `vision::ort_common`. Criterion throughput is set
// to Elements(n), so the report shows images/s next to the per-batch time.
// Input tensors are synthetic and prebuilt per batch size — the numbers are
// pure inference, no preprocessing.
//
// Skips gracefully (prints why + returns) when the model file or the ORT
// dylib is missing, and skips the TRT half when the loaded runtime has no
// TensorRT EP — CI machines without a GPU never fail.
//
// B300 run:
//   cargo bench --features inference-supertonic --bench onnx_cv_trt
// Model: `<models_root>/vision/rfdetr-base.onnx` (deploy the detector first).
// ORT_DYLIB_PATH is auto-detected from `native-libs/<platform>/lib-dynamic/`
// (that build carries the TensorRT + CUDA providers); override via env to
// point at another runtime. The FIRST TRT run builds one engine for the whole
// 1..16 batch profile — expect a minutes-scale one-off stall, cached under
// `trt-cache-bench/`; warm runs load the serialized plan. Delete
// `trt-cache-bench/` after changing the profile, the model, or the GPU.

#![cfg(feature = "inference-supertonic")]

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use tentaflow_core::vision::ort_common::{self, TrtShapeProfile};

/// Square input resolution of the exported RF-DETR graph.
const RESOLUTION: usize = 560;
const BATCHES: [usize; 3] = [1, 8, 16];

/// Builds one deterministic `[n,3,560,560]` input value (content is irrelevant
/// to inference cost; non-constant values avoid any degenerate all-zero paths).
fn synthetic_input(n: usize) -> ort::value::Value {
    let len = n * 3 * RESOLUTION * RESOLUTION;
    let data: Vec<f32> = (0..len).map(|i| ((i % 255) as f32 / 255.0) - 0.5).collect();
    let array = ndarray::Array4::from_shape_vec((n, 3, RESOLUTION, RESOLUTION), data)
        .expect("shape matches data length");
    ort::value::Value::from_array(array)
        .expect("Value::from_array")
        .into_dyn()
}

/// Benches `session.run` for every batch size in `BATCHES` under `label`.
fn bench_session(c: &mut Criterion, label: &str, session: &mut ort::session::Session) {
    let input_name = session
        .inputs()
        .first()
        .map(|i| i.name().to_string())
        .expect("detector graph has one input");
    let mut group = c.benchmark_group(label);
    // TRT engine builds happen inside the first `run` of each session, not at
    // session creation — one untimed warmup per batch size keeps the one-off
    // engine build (and CUDA graph capture) out of the measured samples.
    for &n in &BATCHES {
        let value = synthetic_input(n);
        session
            .run(ort::inputs! { input_name.as_str() => &value })
            .expect("warmup run");
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("batch_{n}"), |b| {
            b.iter(|| {
                // Bind instead of returning: `SessionOutputs` borrows the
                // session, and that borrow must end inside the FnMut body.
                let outputs = session
                    .run(ort::inputs! { input_name.as_str() => &value })
                    .expect("session.run");
                drop(outputs);
            });
        });
    }
    group.finish();
}

fn bench_cpu_vs_trt(c: &mut Criterion) {
    let model_path = tentaflow_core::paths::vision_models_dir().join("rfdetr-base.onnx");
    if !model_path.exists() {
        eprintln!(
            "onnx_cv_trt: skipping — model not available at {}",
            model_path.display()
        );
        return;
    }
    ort_common::ensure_ort_dylib();
    // With `load-dynamic`, a missing dylib PANICS inside ort on first API use —
    // check the resolved path ourselves so a GPU-less CI box skips instead.
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if !std::path::Path::new(&dylib).is_file() {
        eprintln!("onnx_cv_trt: skipping — ORT dylib not found at '{dylib}'");
        return;
    }

    // FP32 CPU baseline: default session builder = CPU EP only.
    match ort::session::Session::builder()
        .and_then(|mut b| b.commit_from_file(&model_path))
    {
        Ok(mut session) => bench_session(c, "rfdetr_fp32_cpu", &mut session),
        Err(e) => {
            eprintln!("onnx_cv_trt: skipping CPU bench — session build failed: {e}");
            return;
        }
    }

    use ort::ep::ExecutionProvider;
    if !ort::ep::TensorRT::default().is_available().unwrap_or(false) {
        eprintln!("onnx_cv_trt: skipping TRT bench — TensorRT EP not available in this runtime");
        return;
    }
    // FP16 TensorRT with the explicit 1..16 batch profile. Separate cache dir:
    // max_batch=16 differs from the production 1..8 profile, and mixing the
    // two would invalidate the deployed engine plans.
    let trt_cache = tentaflow_core::paths::vision_models_dir().join("trt-cache-bench");
    let profile = TrtShapeProfile {
        input_name: "input".to_string(),
        min_batch: 1,
        opt_batch: 8,
        max_batch: 16,
        channels: 3,
        height: RESOLUTION as u32,
        width: RESOLUTION as u32,
    };
    match ort_common::build_ort_session(&model_path, &trt_cache, Some(&profile)) {
        Ok(mut session) => bench_session(c, "rfdetr_fp16_trt", &mut session),
        Err(e) => eprintln!("onnx_cv_trt: skipping TRT bench — session build failed: {e}"),
    }
}

criterion_group!(benches, bench_cpu_vs_trt);
criterion_main!(benches);
