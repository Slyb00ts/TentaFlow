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

    // --- RF-DETR: [1,3,560,560] -> ([1,300,4], [1,300,18]) ---
    match run_rfdetr(&rfdetr_bpk, &device) {
        Ok((d0, d1)) => {
            let shapes_ok = d0 == [1, 300, 4] && d1 == [1, 300, 18];
            println!(
                "[gpu-forward] RF-DETR forward OK — wyjścia {:?} + {:?} {}",
                d0,
                d1,
                if shapes_ok { "(kształty OK)" } else { "(NIEOCZEKIWANE kształty)" }
            );
            ok &= shapes_ok;
        }
        Err(e) => {
            eprintln!("[gpu-forward] RF-DETR BŁĄD/PANIC: {e}");
            ok = false;
        }
    }

    // --- RF-DETR wielokamerowo: 2 klatki przez pętlę batch=1 (jak runtime) ---
    // Odtwarza to, co robi `detector_rfdetr::detect_batch` po fixie: zamiast
    // jednego forwardu `[2,3,560,560]` (który panikuje `Add` na modelu fixed-
    // batch-1) — dwa osobne forwardy `[1,3,560,560]`. Potwierdza BRAK panica
    // oraz kształty `[1,300,4]`+`[1,300,18]` dla KAŻDEJ klatki.
    match run_rfdetr_two_frames(&rfdetr_bpk, &device) {
        Ok(shapes) => {
            let all_ok = shapes
                .iter()
                .all(|&(d0, d1)| d0 == [1, 300, 4] && d1 == [1, 300, 18]);
            println!(
                "[gpu-forward] RF-DETR 2 klatki (pętla batch=1) OK — {:?} {}",
                shapes,
                if all_ok { "(kształty OK, brak panica Add)" } else { "(NIEOCZEKIWANE kształty)" }
            );
            ok &= all_ok;
        }
        Err(e) => {
            eprintln!("[gpu-forward] RF-DETR 2 klatki BŁĄD/PANIC: {e}");
            ok = false;
        }
    }

    // --- STAN: [1,3,224,224] -> [1,4] ---
    match run_stan(&stan_bpk, &device) {
        Ok(d) => {
            let shape_ok = d == [1, 4];
            println!(
                "[gpu-forward] STAN forward OK — wyjście {:?} {}",
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

/// Ładuje RF-DETR z `.bpk` (z adapterem Bool) i robi forward na zerowym wejściu.
/// Panic z backendu (np. `bool_from_data`) łapiemy, aby zwrócić czytelny błąd.
fn run_rfdetr(
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
        let input = Tensor::<B, 4>::zeros([1, 3, 560, 560], device);
        let (o0, o1) = model.forward(input);
        let d0 = o0.dims();
        let d1 = o1.dims();
        // Wymuś synchronizację (read-back) — bez tego kernele mogą nie wykonać się.
        let _ = o0.to_data();
        let _ = o1.to_data();
        Ok((d0, d1))
    }))
    .map_err(|_| "panic w forward RF-DETR (patrz komunikat powyżej)".to_string())?
}

/// Ładuje RF-DETR raz i wykonuje DWA osobne forwardy `[1,3,560,560]` (pętla
/// batch=1) — replika ścieżki wielokamerowej runtime'u po fixie. Wejście jest
/// NIEZEROWE (ramp + ones), aby uruchomić realne ścieżki grafu, nie tylko zera.
/// Panic z backendu (np. shape `Add`) łapiemy i zwracamy jako błąd.
fn run_rfdetr_two_frames(
    path: &str,
    device: &Dev,
) -> Result<Vec<([usize; 3], [usize; 3])>, String> {
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

        // Dwie różne, niezerowe klatki: ramp znormalizowany do ~[-2,2] oraz ones.
        let numel = 3 * 560 * 560;
        let ramp: Vec<f32> = (0..numel)
            .map(|i| (i % 255) as f32 / 255.0 * 4.0 - 2.0)
            .collect();
        let frames = [
            Tensor::<B, 4>::from_data(
                burn::tensor::TensorData::new(ramp, [1, 3, 560, 560]),
                device,
            ),
            Tensor::<B, 4>::ones([1, 3, 560, 560], device),
        ];

        let mut shapes = Vec::with_capacity(frames.len());
        for input in frames {
            let (o0, o1) = model.forward(input);
            let d0 = o0.dims();
            let d1 = o1.dims();
            let _ = o0.to_data();
            let _ = o1.to_data();
            shapes.push((d0, d1));
        }
        Ok(shapes)
    }))
    .map_err(|_| "panic w forward RF-DETR 2 klatki (patrz komunikat powyżej)".to_string())?
}

/// Ładuje model STAN z `.bpk` i robi forward na zerowym wejściu.
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
        let input = Tensor::<B, 4>::zeros([1, 3, 224, 224], device);
        let out = model.forward(input);
        let d = out.dims();
        let _ = out.to_data();
        Ok(d)
    }))
    .map_err(|_| "panic w forward STAN (patrz komunikat powyżej)".to_string())?
}
