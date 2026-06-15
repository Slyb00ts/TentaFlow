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
use std::sync::Once;

use anyhow::{anyhow, Context, Result};
use ort::execution_providers::{CUDAExecutionProvider, TensorRTExecutionProvider};
use ort::session::Session;
use tracing::{info, warn};

/// `load-dynamic` links no onnxruntime at build time — it loads the shared lib
/// at runtime. We use the *system* onnxruntime (built against whatever CUDA the
/// host has), which is what makes the GPU path version-agnostic. ort reads
/// `ORT_DYLIB_PATH`; when unset we probe the usual install locations once so a
/// normal deploy needs no env wiring.
static DYLIB_INIT: Once = Once::new();

fn ensure_dylib_path() {
    DYLIB_INIT.call_once(|| {
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }

        // Platform-specific shared-library file name.
        let libname = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else {
            "libonnxruntime.so"
        };

        // First look next to the executable (+ a `lib/` subdir) so an installer
        // can drop the runtime into the install prefix and it works on any
        // distro with zero env wiring; then fall back to system locations.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join(libname));
                candidates.push(dir.join("lib").join(libname));
            }
        }
        for base in [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/opt/onnxruntime/lib",
            "/usr/lib/x86_64-linux-gnu",
        ] {
            candidates.push(Path::new(base).join(libname));
        }

        if let Some(found) = candidates.iter().find(|p| p.exists()) {
            std::env::set_var("ORT_DYLIB_PATH", found);
            info!("[vision] ORT_DYLIB_PATH -> {}", found.display());
        } else {
            warn!(
                "[vision] no system libonnxruntime.so found; set ORT_DYLIB_PATH \
                 or install onnxruntime (GPU build for CUDA/TensorRT)"
            );
        }
    });
}

/// Builds a session for `model_path` with the TensorRT → CUDA → CPU EP chain.
/// EP registration is non-fatal: a missing GPU provider degrades to the next,
/// so this always succeeds on a CPU-only onnxruntime.
pub fn build_session(model_path: &Path) -> Result<Session> {
    ensure_dylib_path();
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
