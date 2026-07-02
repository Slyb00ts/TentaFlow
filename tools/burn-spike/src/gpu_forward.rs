// =============================================================================
// Plik: tools/burn-spike/src/gpu_forward.rs
// Opis: Walidacja inferencji modeli wizyjnych NA BACKENDZIE GPU (wgpu/cubecl —
//       ten sam, którego używa runtime TentaVision: CubeBackend<WgpuRuntime>).
//       Ładuje zatwierdzone architektury z generated/{rfdetr,stan}.rs, wczytuje
//       wagi z .bpk i wykonuje forward na zerowym wejściu, potwierdzając BRAK
//       panica `bool_from_data Bool(Native)` oraz poprawne kształty wyjść.
// Przykład:
//   cargo run -p burn-spike --bin gpu-forward -- \
//       .runtime/models/vision/rfdetr-base.bpk .runtime/models/vision/model_stan.bpk
//   cargo run -p burn-spike --features cuda --bin gpu-forward -- <rfdetr.bpk> <stan.bpk>
// =============================================================================

mod rfdetr {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tentaflow-core/src/vision/generated/rfdetr.rs"
    ));
}
mod stan {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tentaflow-core/src/vision/generated/stan.rs"
    ));
}

use std::rc::Rc;

use burn::tensor::{BoolStore, DType, Tensor};
use burn_store::{BurnpackStore, ModuleAdapter, ModuleSnapshot, TensorSnapshot};

// Wybór backendu GPU. Domyślnie wgpu — dokładnie backend runtime'u
// (CubeBackend<WgpuRuntime>), na którym reprodukuje się crash `bool_from_data`.
#[cfg(feature = "cuda")]
mod backend {
    pub type B = burn::backend::Cuda<f32, i32>;
    pub type Dev = burn::backend::cuda::CudaDevice;
    pub fn device() -> Dev {
        Default::default()
    }
    pub const NAME: &str = "Burn-CUDA (cubecl)";
}
#[cfg(all(feature = "vulkan", not(feature = "cuda")))]
mod backend {
    pub type B = burn::backend::Vulkan<f32, i32>;
    pub type Dev = burn::backend::wgpu::WgpuDevice;
    pub fn device() -> Dev {
        Default::default()
    }
    pub const NAME: &str = "Burn-Vulkan (cubecl)";
}
#[cfg(not(any(feature = "cuda", feature = "vulkan")))]
mod backend {
    pub type B = burn::backend::wgpu::Wgpu<f32, i32>;
    pub type Dev = burn::backend::wgpu::WgpuDevice;
    pub fn device() -> Dev {
        Default::default()
    }
    pub const NAME: &str = "Burn-wgpu (cubecl)";
}

use backend::{B, Dev};

/// Adapter konwertujący stałe tensory `Bool(Native)` -> `Bool(U32)` przy
/// ładowaniu wag. Kopia logiki runtime'u (`burn_backend::BoolNativeToU32Adapter`),
/// bo `tools/burn-spike` jest osobnym crate'em i nie linkuje `tentaflow-core`.
#[derive(Debug, Clone, Default)]
struct BoolNativeToU32Adapter;

impl ModuleAdapter for BoolNativeToU32Adapter {
    fn adapt(&self, snapshot: &TensorSnapshot) -> TensorSnapshot {
        if snapshot.dtype != DType::Bool(BoolStore::Native) {
            return snapshot.clone();
        }
        let target = DType::Bool(BoolStore::U32);
        let source = snapshot.clone_data_fn();
        let data_fn = Rc::new(move || {
            let data = source()?;
            Ok(data.convert_dtype(target))
        });
        TensorSnapshot::from_closure(
            data_fn,
            target,
            snapshot.shape.clone(),
            snapshot.path_stack.clone().unwrap_or_default(),
            snapshot.container_stack.clone().unwrap_or_default(),
            snapshot.tensor_id.unwrap_or_default(),
        )
    }

    fn clone_box(&self) -> Box<dyn ModuleAdapter> {
        Box::new(self.clone())
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let rfdetr_bpk = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| ".runtime/models/vision/rfdetr-base.bpk".to_string());
    let stan_bpk = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| ".runtime/models/vision/model_stan.bpk".to_string());

    let device = backend::device();
    println!("[gpu-forward] backend = {}", backend::NAME);

    let mut ok = true;

    // --- RF-DETR fixed batch=8: [8,3,560,560] -> ([8,300,4], [8,300,18]) ---
    // Architektura po regeneracji ma zapieczony batch=8 (constant-folding onnxsim
    // z --overwrite-input-shape input:8,3,560,560). Sprawdzamy że pojedynczy
    // forward całego batcha przechodzi bez panica (Bool mask, Add na kształtach)
    // i zwraca statyczne kształty [8,300,4]+[8,300,18].
    match run_rfdetr_batch8(&rfdetr_bpk, &device) {
        Ok((d0, d1)) => {
            let shapes_ok = d0 == [8, 300, 4] && d1 == [8, 300, 18];
            println!(
                "[gpu-forward] RF-DETR batch=8 forward OK — wyjścia {:?} + {:?} {}",
                d0,
                d1,
                if shapes_ok { "(kształty OK, brak panica Bool/Add)" } else { "(NIEOCZEKIWANE kształty)" }
            );
            ok &= shapes_ok;
        }
        Err(e) => {
            eprintln!("[gpu-forward] RF-DETR batch=8 BŁĄD/PANIC: {e}");
            ok = false;
        }
    }

    // --- STAN fixed batch=8: [8,3,224,224] -> [8,4] ---
    // Architektura po regeneracji ma zapieczony batch=8 (onnxsim
    // --overwrite-input-shape input:8,3,224,224). Reshape używa `[-1,1280]`, więc
    // sam graf jest batch-elastyczny, ale walidujemy pełny batch=8 zgodnie z tym,
    // czego oczekuje `classify_batch` (padding slotów do 8).
    match run_stan(&stan_bpk, &device) {
        Ok(d) => {
            let shape_ok = d == [8, 4];
            println!(
                "[gpu-forward] STAN batch=8 forward OK — wyjście {:?} {}",
                d,
                if shape_ok { "(kształt OK)" } else { "(NIEOCZEKIWANY kształt)" }
            );
            ok &= shape_ok;
        }
        Err(e) => {
            eprintln!("[gpu-forward] STAN BŁĄD/PANIC: {e}");
            ok = false;
        }
    }

    if ok {
        println!("[gpu-forward] WYNIK: OK — inferencja na GPU bez panica, kształty zgodne");
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("[gpu-forward] WYNIK: BŁĄD");
        std::process::ExitCode::FAILURE
    }
}

/// Ładuje RF-DETR (fixed batch=8) z `.bpk` (z adapterem Bool) i wykonuje jeden
/// forward na NIEZEROWYM wejściu `[8,3,560,560]` (ramp per-element, aby pobudzić
/// realne ścieżki grafu, a nie same zera). Panic z backendu (np. `bool_from_data`
/// przy masce, czy niezgodność kształtów `Add`) łapiemy i zwracamy jako błąd.
fn run_rfdetr_batch8(
    path: &str,
    device: &Dev,
) -> Result<([usize; 3], [usize; 3]), String> {
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        return Err(format!("plik nie istnieje: {}", path.display()));
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut model = rfdetr::Model::<B>::new(device);
        let mut store =
            BurnpackStore::from_file(&path).with_from_adapter(BoolNativeToU32Adapter);
        model
            .load_from(&mut store)
            .map_err(|e| format!("load_from: {e}"))?;

        // Niezerowy ramp znormalizowany do ~[-2,2] na całym batchu [8,3,560,560].
        let numel = 8 * 3 * 560 * 560;
        let ramp: Vec<f32> = (0..numel)
            .map(|i| (i % 255) as f32 / 255.0 * 4.0 - 2.0)
            .collect();
        let input = Tensor::<B, 4>::from_data(
            burn::tensor::TensorData::new(ramp, [8, 3, 560, 560]),
            device,
        );

        let (o0, o1) = model.forward(input);
        let d0 = o0.dims();
        let d1 = o1.dims();
        // Wymuś synchronizację (read-back) — bez tego kernele mogą nie wykonać się.
        let _ = o0.to_data();
        let _ = o1.to_data();
        Ok((d0, d1))
    }))
    .map_err(|_| "panic w forward RF-DETR batch=8 (patrz komunikat powyżej)".to_string())?
}

/// Ładuje model STAN (fixed batch=8) z `.bpk` i robi jeden forward na niezerowym
/// wejściu `[8,3,224,224]` (ramp per-element, aby pobudzić realne ścieżki grafu).
fn run_stan(
    path: &str,
    device: &Dev,
) -> Result<[usize; 2], String> {
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        return Err(format!("plik nie istnieje: {}", path.display()));
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut model = stan::Model::<B>::new(device);
        let mut store =
            BurnpackStore::from_file(&path).with_from_adapter(BoolNativeToU32Adapter);
        model
            .load_from(&mut store)
            .map_err(|e| format!("load_from: {e}"))?;
        let numel = 8 * 3 * 224 * 224;
        let ramp: Vec<f32> = (0..numel)
            .map(|i| (i % 255) as f32 / 255.0 * 4.0 - 2.0)
            .collect();
        let input = Tensor::<B, 4>::from_data(
            burn::tensor::TensorData::new(ramp, [8, 3, 224, 224]),
            device,
        );
        let out = model.forward(input);
        let d = out.dims();
        let _ = out.to_data();
        Ok(d)
    }))
    .map_err(|_| "panic w forward STAN (patrz komunikat powyżej)".to_string())?
}
