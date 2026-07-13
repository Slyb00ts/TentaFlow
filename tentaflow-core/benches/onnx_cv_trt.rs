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
/// Env selecting the session-pool sizes the concurrency bench probes (comma
/// list, e.g. `"1,2,4,8"`). With the per-session TensorRT workspace capped
/// (`[vision] trt_workspace_mib`, default 1 GiB) each RF-DETR session costs only
/// ~2.1 GB VRAM, so 8+ sessions co-reside on a free 24 GB 4090; aggregate
/// throughput plateaus around N=4 (compute-bound: ~214 / 287 / 312 / 310 img/s
/// at N=1/2/4/8). The default `"1,2"` stays conservative so the whole bench
/// (determinism at `widest` + timed levels) fits even on a partly-loaded box.
const CONCURRENCY_ENV: &str = "TENTAFLOW_BENCH_CONCURRENCY";
const DEFAULT_CONCURRENCY: &str = "1,2";

/// Parses [`CONCURRENCY_ENV`] into the ordered, de-duplicated pool sizes to
/// probe. Falls back to [`DEFAULT_CONCURRENCY`] on unset/empty/all-garbage input;
/// every level is `>= 1`.
fn concurrency_levels() -> Vec<usize> {
    let raw = std::env::var(CONCURRENCY_ENV).unwrap_or_default();
    let source = if raw.trim().is_empty() {
        DEFAULT_CONCURRENCY
    } else {
        raw.as_str()
    };
    let mut levels: Vec<usize> = Vec::new();
    for tok in source.split(',') {
        if let Ok(n) = tok.trim().parse::<usize>() {
            if n >= 1 && !levels.contains(&n) {
                levels.push(n);
            }
        }
    }
    if levels.is_empty() {
        vec![1, 2]
    } else {
        levels
    }
}

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
    match ort::session::Session::builder().and_then(|mut b| b.commit_from_file(&model_path)) {
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
    match ort_common::build_ort_session(&model_path, &trt_cache, Some(&profile), 0, true) {
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
        eprintln!("onnx_cv_concurrency: skipping — TensorRT EP not available in this runtime");
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
        ort_common::build_ort_session_from_memory(&model_bytes, &trt_cache, Some(&profile), 0, true)
            .expect("build session")
    };

    let value = synthetic_input(1);

    // ONE pool of `widest` sessions serves BOTH the determinism gate and every
    // timed level. Building fresh sessions per phase (widest for the gate + one
    // set per level) kept every prior phase's sessions resident — many large TRT
    // arenas — and exhausted the 4090's 24 GB (`BFCArena Failed to allocate`).
    // A single `widest`-session pool caps resident VRAM at `widest` model copies
    // and each level times a prefix `&mut pool[..n]`. `widest` is the max of the
    // ACTUALLY-TIMED levels ([`concurrency_levels`], default `1,2`), so the gate
    // only ever allocates the sessions the timed run actually uses.
    let levels = concurrency_levels();
    let widest = *levels.iter().max().unwrap();
    let mut pool: Vec<ort::session::Session> = (0..widest).map(|_| build()).collect();
    // Warm each session once so the TRT engine build is out of every measurement
    // and every session is in steady state before the concurrent forwards.
    for s in pool.iter_mut() {
        let _ = run_forward(s, &input_name, &value);
    }

    // Concurrent determinism gate: forward the SAME input on ALL sessions AT THE
    // SAME TIME (a Barrier releases every thread into `run` together) and assert
    // byte-identical dets+labels across sessions. Sequential comparison would
    // miss races that only surface when sessions execute simultaneously — this
    // is the real corruption detector. Repeated for several iterations to catch
    // intermittent races. Runs before timing so a corrupt build aborts loudly
    // instead of publishing numbers.
    let reference = run_forward(&mut pool[0], &input_name, &value);
    const DET_ITERS: usize = 5;
    let barrier = std::sync::Barrier::new(widest);
    for iter in 0..DET_ITERS {
        let results: Vec<(usize, (Vec<f32>, Vec<f32>))> = std::thread::scope(|scope| {
            let name = input_name.as_str();
            let value_ref = &value;
            let barrier_ref = &barrier;
            let handles: Vec<_> = pool
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
            handles
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .collect()
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
    for &n in &levels {
        // Reuse a prefix of the single warmed pool — no new sessions per level,
        // so resident VRAM never grows across phases.
        let slice = &mut pool[..n];
        let name = input_name.as_str();
        let value_ref = &value;
        // PERSISTENT workers, one per session, spawned ONCE and reused for every
        // criterion iteration. A fresh scoped thread per iteration made ORT's
        // CUDA EP accumulate per-OS-thread device resources (streams/handles it
        // caches by thread id and never frees), so ~1400 short-lived threads grew
        // VRAM until even a 62 MB alloc OOM'd — a bench artifact, not the pool.
        // Two barriers fence one synchronized round: `go` releases all workers
        // into `run` at once, `done` rejoins them; the timed closure just passes
        // through both, so a sample = wall time of the slowest of `n` parallel
        // forwards = aggregate time for `n` images. Throughput=Elements(n) turns
        // that into images/s.
        let go = std::sync::Barrier::new(n + 1);
        let done = std::sync::Barrier::new(n + 1);
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            for s in slice.iter_mut() {
                let go = &go;
                let done = &done;
                let stop = &stop;
                scope.spawn(move || loop {
                    go.wait();
                    if stop.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                    let out = s
                        .run(ort::inputs! { name => value_ref })
                        .expect("session.run");
                    drop(out);
                    done.wait();
                });
            }
            group.throughput(Throughput::Elements(n as u64));
            group.bench_function(format!("threads_{n}"), |b| {
                b.iter(|| {
                    go.wait();
                    done.wait();
                });
            });
            // Release the workers one final time with `stop` set so they break
            // before `done.wait()` and the scope can join them.
            stop.store(true, std::sync::atomic::Ordering::Release);
            go.wait();
        });
    }
    group.finish();
}

criterion_group!(benches, bench_cpu_vs_trt, bench_concurrent_sessions);
criterion_main!(benches);
