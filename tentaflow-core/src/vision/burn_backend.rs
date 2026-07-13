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

#[cfg(not(any(
    feature = "vision-cuda",
    feature = "vision-metal",
    feature = "vision-rocm"
)))]
pub type VisionBackend = burn::backend::wgpu::Wgpu<f32, i32>;
#[cfg(not(any(
    feature = "vision-cuda",
    feature = "vision-metal",
    feature = "vision-rocm"
)))]
pub type VisionDevice = burn::backend::wgpu::WgpuDevice;

/// Default device for the selected backend.
pub fn device() -> VisionDevice {
    Default::default()
}

/// Adapter ładowania wag: rzutuje stałe tensory logiczne zapisane w `.bpk` jako
/// natywny `Bool(Native)` na `Bool(U32)`.
///
/// Backendy cubecl (wgpu/CUDA/…) NIE obsługują `bool_from_data` dla
/// `Bool(Native)` — panikują z „Unsupported dtype for `bool_from_data`
/// Bool(Native)”. Nowy graf RF-DETR zawiera stałą maskę (`Where`/`mask_fill`)
/// eksportowaną właśnie jako `Bool(Native)`, przez co inferencja na GPU
/// crashuje. Backend wgpu reprezentuje wartości logiczne jako `u32` (WGSL nie
/// zna `u8` — `Bool(U8)` powoduje panic „U8 is not a valid WgpuElement” przy
/// kompilacji shadera), więc konwertujemy do `Bool(U32)`, który jest zgodny
/// zarówno z wgpu, jak i z CUDA. Konwersja jest bezstratna (0/1). Backend
/// NdArray akceptuje wszystkie warianty.
#[derive(Debug, Clone, Default)]
pub struct BoolNativeToU32Adapter;

impl burn_store::ModuleAdapter for BoolNativeToU32Adapter {
    fn adapt(&self, snapshot: &burn_store::TensorSnapshot) -> burn_store::TensorSnapshot {
        use burn::tensor::{BoolStore, DType};

        if snapshot.dtype != DType::Bool(BoolStore::Native) {
            return snapshot.clone();
        }

        let target = DType::Bool(BoolStore::U32);
        let source = snapshot.clone_data_fn();
        let data_fn = std::rc::Rc::new(move || {
            let data = source()?;
            Ok(data.convert_dtype(target))
        });

        burn_store::TensorSnapshot::from_closure(
            data_fn,
            target,
            snapshot.shape.clone(),
            snapshot.path_stack.clone().unwrap_or_default(),
            snapshot.container_stack.clone().unwrap_or_default(),
            snapshot.tensor_id.unwrap_or_default(),
        )
    }

    fn clone_box(&self) -> Box<dyn burn_store::ModuleAdapter> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burn::tensor::{BoolStore, DType, TensorData};
    use burn_store::ModuleAdapter;

    #[test]
    fn native_bool_jest_konwertowany_na_u32() {
        let data = TensorData::from([[true, false, true]]);
        assert_eq!(data.dtype, DType::Bool(BoolStore::Native));
        let snap = burn_store::TensorSnapshot::from_data(
            data,
            vec!["mask".into()],
            vec!["Struct:Model".into()],
            Default::default(),
        );
        let out = BoolNativeToU32Adapter.adapt(&snap);
        assert_eq!(out.dtype, DType::Bool(BoolStore::U32));
        let out_data = out.to_data().expect("materializacja danych");
        assert_eq!(out_data.dtype, DType::Bool(BoolStore::U32));
        assert_eq!(out_data.shape, snap.shape);
    }

    #[test]
    fn nie_bool_pozostaje_bez_zmian() {
        let data = TensorData::from([1.0f32, 2.0, 3.0]);
        let snap = burn_store::TensorSnapshot::from_data(
            data,
            vec!["w".into()],
            vec!["Struct:Linear".into()],
            Default::default(),
        );
        let out = BoolNativeToU32Adapter.adapt(&snap);
        assert_eq!(out.dtype, DType::F32);
    }
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
    static EXEC: std::sync::OnceLock<std::sync::mpsc::Sender<InferJob>> =
        std::sync::OnceLock::new();
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
