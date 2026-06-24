// =============================================================================
// File: vision/burn_backend.rs — compile-time Burn backend selection for vision
// =============================================================================
//
// One backend type for all camera-CV models, chosen by feature so the same
// codegen'd models run natively per platform: CUDA (NVIDIA, NVRTC), Metal
// (Apple), ROCm (AMD), or wgpu (Vulkan/DX12/WebGPU) as the universal fallback.
// The model architectures are vendored in `generated/` (build-time ONNX→Burn
// codegen); only the `.bpk` weights load at runtime.

#![cfg(feature = "vision-burn")]

#[cfg(feature = "vision-cuda")]
pub type VisionBackend = burn::backend::Cuda<f32, i32>;
#[cfg(feature = "vision-cuda")]
pub type VisionDevice = burn::backend::cuda::CudaDevice;

#[cfg(all(feature = "vision-metal", not(feature = "vision-cuda")))]
pub type VisionBackend = burn::backend::Metal<f32, i32>;
#[cfg(all(feature = "vision-metal", not(feature = "vision-cuda")))]
pub type VisionDevice = burn::backend::wgpu::WgpuDevice;

#[cfg(all(
    feature = "vision-rocm",
    not(any(feature = "vision-cuda", feature = "vision-metal"))
))]
pub type VisionBackend = burn::backend::Rocm<f32, i32>;
#[cfg(all(
    feature = "vision-rocm",
    not(any(feature = "vision-cuda", feature = "vision-metal"))
))]
pub type VisionDevice = burn::backend::rocm::RocmDevice;

#[cfg(not(any(feature = "vision-cuda", feature = "vision-metal", feature = "vision-rocm")))]
pub type VisionBackend = burn::backend::wgpu::Wgpu<f32, i32>;
#[cfg(not(any(feature = "vision-cuda", feature = "vision-metal", feature = "vision-rocm")))]
pub type VisionDevice = burn::backend::wgpu::WgpuDevice;

/// Default device for the selected backend.
pub fn device() -> VisionDevice {
    Default::default()
}

type InferJob = Box<dyn FnOnce() + Send + 'static>;

/// The single long-lived thread that runs EVERY vision `forward()`.
///
/// One thread, two reasons:
/// 1. cubecl's wgpu memory manager corrupts under concurrent allocation from many
///    threads (assertion failures + spurious "Out of Memory"); serializing all GPU
///    work onto one thread removes that data race entirely.
/// 2. burn-onnx emits each model as ONE multi-thousand-line `forward()`; unoptimized
///    (debug) builds give every intermediate tensor its own stack slot, overrunning a
///    default thread stack — so this thread reserves 64 MB.
///
/// A panicking job is isolated with `catch_unwind` so one bad forward can't kill the
/// executor (its oneshot sender drops → the caller observes `Err`).
fn infer_executor() -> &'static std::sync::mpsc::Sender<InferJob> {
    static EXEC: std::sync::OnceLock<std::sync::mpsc::Sender<InferJob>> = std::sync::OnceLock::new();
    EXEC.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<InferJob>();
        std::thread::Builder::new()
            .name("vision-infer".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            })
            .expect("spawn vision-infer executor thread");
        tx
    })
}

/// Run a vision closure on the shared inference thread and await its result.
/// `Err` means the job's result was dropped (the closure panicked).
pub async fn run_blocking<T, F>(f: F) -> Result<T, tokio::sync::oneshot::error::RecvError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    let job: InferJob = Box::new(move || {
        let _ = tx.send(f());
    });
    // The receiver lives in a 'static OnceLock, so the channel never closes; send only
    // fails if the executor thread died, which the per-job catch_unwind prevents.
    infer_executor()
        .send(job)
        .expect("vision-infer executor alive");
    rx.await
}

/// Run a model `forward()` under `catch_unwind`, turning a burn/GPU panic into an
/// `Err`. Without this, a panic inside a forward unwinds through the caller's held
/// model `Mutex` — poisoning it so every later `lock().unwrap()` panics too (a
/// self-sustaining spam cascade) — and kills the inference thread.
pub fn guarded_forward<T>(label: &str, f: impl FnOnce() -> T) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        anyhow::anyhow!("{label} forward panicked: {msg}")
    })
}
