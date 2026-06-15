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
        // Resolve the runtime path: explicit env wins, else probe install-prefix
        // (next to the binary) then system locations so a deploy needs no wiring.
        let resolved: Option<std::path::PathBuf> =
            if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
                Some(std::path::PathBuf::from(p))
            } else {
                let libname = if cfg!(target_os = "macos") {
                    "libonnxruntime.dylib"
                } else if cfg!(target_os = "windows") {
                    "onnxruntime.dll"
                } else {
                    "libonnxruntime.so"
                };
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
                let found = candidates.into_iter().find(|p| p.exists());
                if let Some(ref p) = found {
                    std::env::set_var("ORT_DYLIB_PATH", p);
                    info!("[vision] ORT_DYLIB_PATH -> {}", p.display());
                }
                found
            };

        match resolved {
            Some(p) => preload_deepbind(&p),
            None => warn!(
                "[vision] no system libonnxruntime found; set ORT_DYLIB_PATH \
                 or install onnxruntime (GPU build for CUDA/TensorRT)"
            ),
        }
    });
}

/// Preloads onnxruntime with `RTLD_DEEPBIND` so it resolves its own bundled
/// protobuf/abseil first. Without this, `libzvec_c_api.so` (loaded at startup,
/// exporting protobuf globally) interposes onnxruntime's protobuf symbols and a
/// version-mismatched `RepeatedPtrFieldBase::DestroyProtos` segfaults on the
/// first `Session`. ort later `dlopen`s the same path and gets this handle, so
/// its internal calls stay bound to the right protobuf. No-op off Linux/gnu.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn preload_deepbind(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    // Leak the handle on purpose: the runtime must stay mapped for the process.
    let handle = unsafe {
        libc::dlopen(
            c.as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_DEEPBIND,
        )
    };
    if handle.is_null() {
        warn!("[vision] RTLD_DEEPBIND preload of {} failed", path.display());
    } else {
        info!("[vision] onnxruntime preloaded with RTLD_DEEPBIND");
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn preload_deepbind(_path: &Path) {}

/// Builds a session for `model_path` with the TensorRT → CUDA → CPU EP chain.
/// EP registration is non-fatal: a missing GPU provider degrades to the next,
/// so this always succeeds on a CPU-only onnxruntime.
pub fn build_session(model_path: &Path) -> Result<Session> {
    ensure_dylib_path();
    info!(
        "[vision] building ort session for {} (EP: tensorrt>cuda>cpu, best-effort)",
        model_path.display()
    );
    // Diagnostic / opt-in strictness: with TF_VISION_EP_STRICT=1 a GPU provider
    // that fails to register aborts the build (with the real onnxruntime reason)
    // instead of silently degrading to CPU. Default stays graceful for prod.
    // TensorRT is often absent (no nvinfer) — always graceful. Only CUDA is made
    // strict so its real failure reason surfaces instead of a silent CPU degrade.
    let strict = std::env::var_os("TF_VISION_EP_STRICT").is_some();
    let cuda = if strict {
        CUDAExecutionProvider::default().build().error_on_failure()
    } else {
        CUDAExecutionProvider::default().build()
    };
    Session::builder()
        .context("Session::builder")?
        .with_execution_providers([TensorRTExecutionProvider::default().build(), cuda])
        .map_err(|e| anyhow!("with_execution_providers: {e}"))?
        .commit_from_file(model_path)
        .with_context(|| format!("commit ONNX {}", model_path.display()))
}
