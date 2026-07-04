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
// A second group (`rfdetr_concurrent_sessions`) probes the session-pool
// foundation: N independent sessions of the same graph run N forwards in
// parallel (one OS thread each), reporting aggregate images/s at N=1,2,4 and
// asserting identical detections across sessions (determinism = no GPU
// corruption).
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
/// Session-pool sizes probed by the concurrency bench (Workstream-1 Chunk 1):
/// each level runs `n` forwards in parallel, one per independent session.
const POOL_SIZES: [usize; 3] = [1, 2, 4];

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

/// One RF-DETR forward, returning the flat `dets` + `labels` tensors as owned
/// f32 vecs. Comparing these across sessions proves byte-identical output; any
/// divergence would mean cross-session GPU state corruption.
fn run_forward(
    session: &mut ort::session::Session,
    input_name: &str,
    value: &ort::value::Value,
) -> (Vec<f32>, Vec<f32>) {
    let outputs = session
        .run(ort::inputs! { input_name => value })
        .expect("session.run");
    let (_ds, dets) = outputs["dets"]
        .try_extract_tensor::<f32>()
        .expect("extract dets");
    let (_ls, labels) = outputs["labels"]
        .try_extract_tensor::<f32>()
        .expect("extract labels");
    (dets.to_vec(), labels.to_vec())
}

/// Session-pool scaling: builds N INDEPENDENT sessions of the same RF-DETR
/// graph and runs N batch-1 forwards in parallel (one OS thread per session),
/// reporting aggregate images/s at N=1,2,4. Proves N sessions lift throughput
/// beyond a single `&mut`-serialized session, and asserts all sessions return
/// identical boxes for identical input (determinism = no GPU corruption).
///
/// Skips like the latency bench when the model / dylib / TensorRT EP is absent.
fn bench_concurrent_sessions(c: &mut Criterion) {
    let model_path = tentaflow_core::paths::vision_models_dir().join("rfdetr-base.onnx");
    if !model_path.exists() {
        eprintln!(
            "onnx_cv_concurrency: skipping — model not available at {}",
            model_path.display()
        );
        return;
    }
    ort_common::ensure_ort_dylib();
    let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_default();
    if !std::path::Path::new(&dylib).is_file() {
        eprintln!("onnx_cv_concurrency: skipping — ORT dylib not found at '{dylib}'");
        return;
    }
    use ort::ep::ExecutionProvider;
    if !ort::ep::TensorRT::default().is_available().unwrap_or(false) {
        eprintln!(
            "onnx_cv_concurrency: skipping — TensorRT EP not available in this runtime"
        );
        return;
    }

    let model_bytes = match std::fs::read(&model_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("onnx_cv_concurrency: skipping — read model failed: {e}");
            return;
        }
    };
    // Batch-1 profile: threads each run a single-image forward. Separate cache
    // dir from the latency bench (different max_batch → different engine plan).
    let trt_cache = tentaflow_core::paths::vision_models_dir().join("trt-cache-bench-concurrent");
    let input_name = match ort_common::onnx_first_input_name(&model_bytes) {
        Some(name) => name,
        None => {
            eprintln!("onnx_cv_concurrency: skipping — could not read ONNX input name");
            return;
        }
    };
    let profile = TrtShapeProfile {
        input_name: input_name.clone(),
        min_batch: 1,
        opt_batch: 1,
        max_batch: 1,
        channels: 3,
        height: RESOLUTION as u32,
        width: RESOLUTION as u32,
    };
    let build = || -> ort::session::Session {
        ort_common::build_ort_session_from_memory(&model_bytes, &trt_cache, Some(&profile))
            .expect("build session")
    };

    let value = synthetic_input(1);

    // Concurrent determinism gate: build the widest pool, then forward the SAME
    // input on ALL sessions AT THE SAME TIME (a Barrier releases every thread
    // into `run` together) and assert byte-identical dets+labels across
    // sessions. Sequential comparison would miss races that only surface when
    // sessions execute simultaneously — this is the real corruption detector.
    // Repeated for several iterations to catch intermittent races. Runs before
    // timing so a corrupt build aborts loudly instead of publishing numbers.
    let widest = *POOL_SIZES.iter().max().unwrap();
    let mut sessions: Vec<ort::session::Session> = (0..widest).map(|_| build()).collect();
    // Warm each session once so the TRT engine build is out of the comparison
    // and every session is in steady state before the concurrent forwards.
    for s in sessions.iter_mut() {
        let _ = run_forward(s, &input_name, &value);
    }
    let reference = run_forward(&mut sessions[0], &input_name, &value);
    const DET_ITERS: usize = 5;
    let barrier = std::sync::Barrier::new(widest);
    for iter in 0..DET_ITERS {
        let results: Vec<(usize, (Vec<f32>, Vec<f32>))> = std::thread::scope(|scope| {
            let name = input_name.as_str();
            let value_ref = &value;
            let barrier_ref = &barrier;
            let handles: Vec<_> = sessions
                .iter_mut()
                .enumerate()
                .map(|(i, s)| {
                    scope.spawn(move || {
                        // Every session hits `run` at the same instant.
                        barrier_ref.wait();
                        (i, run_forward(s, name, value_ref))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect()
        });
        for (i, out) in &results {
            assert_eq!(
                *out, reference,
                "iter {iter} session {i} diverged under concurrent load — GPU state corruption"
            );
        }
    }
    eprintln!(
        "onnx_cv_concurrency: concurrent determinism OK — {widest} sessions × {DET_ITERS} iters, \
         identical dets ({} floats) + labels ({} floats)",
        reference.0.len(),
        reference.1.len()
    );

    let mut group = c.benchmark_group("rfdetr_concurrent_sessions");
    for &n in &POOL_SIZES {
        // Each level uses its own `n` warmed sessions so the TRT engine build is
        // out of the measured samples and threads never share a session.
        let mut pool: Vec<ort::session::Session> = (0..n).map(|_| build()).collect();
        for s in pool.iter_mut() {
            let out = s
                .run(ort::inputs! { input_name.as_str() => &value })
                .expect("warmup run");
            drop(out);
        }
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("threads_{n}"), |b| {
            b.iter(|| {
                // One scoped thread per session runs its own `&mut` forward; the
                // whole scope joins each iteration, so the sample times the
                // slowest of `n` parallel forwards = aggregate wall time for `n`
                // images. Throughput=Elements(n) turns that into images/s.
                std::thread::scope(|scope| {
                    let name = input_name.as_str();
                    let value_ref = &value;
                    for s in pool.iter_mut() {
                        scope.spawn(move || {
                            let out = s
                                .run(ort::inputs! { name => value_ref })
                                .expect("session.run");
                            drop(out);
                        });
                    }
                });
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_cpu_vs_trt, bench_concurrent_sessions);
criterion_main!(benches);
