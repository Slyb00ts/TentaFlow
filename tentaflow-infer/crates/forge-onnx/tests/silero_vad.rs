// ===== File: silero_vad.rs — numeric gate: forge-onnx GPU output vs onnxruntime =====
//
// Loads the staged Silero VAD ONNX model, runs it on the RTX 4090 through
// forge-onnx's hybrid CPU/GPU interpreter, and asserts the speech probability
// matches the onnxruntime reference (computed offline with the CPU provider on
// the SAME inputs) within a tight tolerance. This is the hard validation gate
// for the loader: parsing without verified numbers is not acceptable.
//
// Model path: $FORGE_SILERO_VAD or the staged .runtime location. The test skips
// (with a clear message) only when neither the model nor a CUDA GPU is present.

use std::collections::HashMap;
use std::sync::Arc;

use forge_hal::{PoolSizes, gpu};
use forge_hal::Device;
use forge_onnx::Tensor;

const DEFAULT_MODEL: &str =
    "/home/critix/repos/rust/TentaFlow/.runtime/models/audio/silero_vad.onnx";

/// onnxruntime CPU-provider references (opset 16 Silero VAD), state = zeros
/// [2,1,128], sr = 16000, 512-sample frame. Generated with:
///   onnxruntime.InferenceSession(silero_vad.onnx).run(...)
/// See the crate report for the exact generator.
const SINE_PROB_REF: f32 = 0.298_752_37; // x[i] = sin(i*0.1)*0.1
const ZERO_PROB_REF: f32 = 0.044_262_707; // x[i] = 0
const TOL: f32 = 1e-3;

fn model_path() -> Option<String> {
    let p = std::env::var("FORGE_SILERO_VAD").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    if std::path::Path::new(&p).exists() {
        Some(p)
    } else {
        None
    }
}

fn device() -> Option<Arc<dyn Device>> {
    // Small pools: Silero is 2.3 MB and its activations are a handful of KB.
    let dev = gpu::open(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 4 << 20,
            activations: 256 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .ok()?;
    Some(dev)
}

fn run_frame(path: &str, dev: Arc<dyn Device>, frame: Vec<f32>) -> f32 {
    let session = forge_onnx::load_session(dev, path).expect("load silero_vad");
    let mut inputs = HashMap::new();
    inputs.insert("input".to_string(), Tensor::from_f32(vec![1, 512], frame));
    inputs.insert(
        "state".to_string(),
        Tensor::from_f32(vec![2, 1, 128], vec![0.0; 2 * 128]),
    );
    inputs.insert("sr".to_string(), Tensor::scalar_i64(16000));
    let out = session.run(inputs).expect("run silero_vad");
    let prob = out.get("output").expect("output tensor");
    let v = prob.to_f32_vec().expect("output f32");
    assert_eq!(v.len(), 1, "VAD output is a single probability, got {v:?}");
    v[0]
}

#[test]
fn op_histogram_lists_silero_ops() {
    let Some(path) = model_path() else {
        eprintln!("SKIP: silero_vad.onnx not found");
        return;
    };
    let model = forge_onnx::load_model(&path).expect("parse silero_vad");
    let hist = forge_onnx::op_histogram(&model);
    // The parser must recover the real op set, including the ops inside the
    // sample-rate / state-init If subgraphs (Conv, LSTM, …).
    for op in [
        "Conv",
        "LSTM",
        "Sigmoid",
        "Relu",
        "ReduceMean",
        "If",
        "Slice",
    ] {
        assert!(hist.contains_key(op), "missing op {op} in {hist:?}");
    }
    eprintln!("silero_vad op histogram: {hist:?}");
}

#[test]
fn silero_vad_matches_onnxruntime() {
    let Some(path) = model_path() else {
        eprintln!("SKIP: silero_vad.onnx not found (set FORGE_SILERO_VAD)");
        return;
    };
    let Some(dev) = device() else {
        eprintln!("SKIP: no CUDA device");
        return;
    };

    // Deterministic sine frame reproducing the reference generator exactly.
    let sine: Vec<f32> = (0..512).map(|i| (i as f32 * 0.1).sin() * 0.1).collect();
    let sine_prob = run_frame(&path, dev.clone(), sine);
    eprintln!("sine : forge={sine_prob:.10}  onnxruntime={SINE_PROB_REF:.10}");
    assert!(
        (sine_prob - SINE_PROB_REF).abs() < TOL,
        "sine VAD prob {sine_prob} vs ref {SINE_PROB_REF} (tol {TOL})"
    );

    let zero_prob = run_frame(&path, dev, vec![0.0; 512]);
    eprintln!("zero : forge={zero_prob:.10}  onnxruntime={ZERO_PROB_REF:.10}");
    assert!(
        (zero_prob - ZERO_PROB_REF).abs() < TOL,
        "zero VAD prob {zero_prob} vs ref {ZERO_PROB_REF} (tol {TOL})"
    );
}
