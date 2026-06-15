// =============================================================================
// File: vision/ort_session.rs — shared ONNX Runtime session builder
// =============================================================================
//
// Single place that builds every vision `ort::Session` with a GPU-first
// execution-provider chain: TensorRT → CUDA → CPU. Registration is best-effort
// — when the loaded onnxruntime build lacks a GPU provider (e.g. the CPU-only
// `download-binaries` package on a dev box) ort logs and silently falls back to
// the next provider, ending on CPU. This keeps a single code path that "just
// uses the GPU" the moment a CUDA/TensorRT onnxruntime is deployed (DGX B300),
// with zero changes to the runners.

#![cfg(feature = "inference-vision-gpu")]

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use ort::execution_providers::{CUDAExecutionProvider, TensorRTExecutionProvider};
use ort::session::Session;
use tracing::info;

/// Builds a session for `model_path` with the TensorRT → CUDA → CPU EP chain.
/// EP registration is non-fatal: a missing GPU provider degrades to the next,
/// so this always succeeds on a CPU-only onnxruntime.
pub fn build_session(model_path: &Path) -> Result<Session> {
    info!(
        "[vision] building ort session for {} (EP: tensorrt>cuda>cpu, best-effort)",
        model_path.display()
    );
    Session::builder()
        .context("Session::builder")?
        .with_execution_providers([
            TensorRTExecutionProvider::default().build(),
            CUDAExecutionProvider::default().build(),
        ])
        .map_err(|e| anyhow!("with_execution_providers: {e}"))?
        .commit_from_file(model_path)
        .with_context(|| format!("commit ONNX {}", model_path.display()))
}
