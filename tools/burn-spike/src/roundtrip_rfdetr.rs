// =============================================================================
// Plik: tools/burn-spike/src/roundtrip_rfdetr.rs
// Opis: Walidacja round-trip konwertera onnx2bpk na modelu RF-DETR. Ładuje
//       wygenerowaną architekturę (generated/rfdetr.rs) i porównuje forward
//       dla wag ze świeżo skonwertowanego .bpk vs oryginalnego .bpk.
// Przykład:
//   cargo run -p burn-spike --bin roundtrip-rfdetr -- \
//       /tmp/rfdetr-base-roundtrip.bpk .runtime/models/vision/rfdetr-base.bpk
// =============================================================================
//
// Backend: NdArray (CPU) — konwersja i weryfikacja wag nie wymagają GPU.
// Architektura pochodzi z zatwierdzonego pliku generated/rfdetr.rs (przez
// include!), dokładnie tak jak robi to runtime `detector_rfdetr.rs`.

mod rfdetr {
    // Architektura RF-DETR wygenerowana z ONNX przez burn-onnx (ta sama, której
    // używa runtime TentaVision). Włączamy ją źródłowo, aby ładować do niej wagi.
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tentaflow-core/src/vision/generated/rfdetr.rs"
    ));
}

use burn::backend::ndarray::NdArrayDevice;
use burn::backend::NdArray;
use burn::tensor::Tensor;
use burn_store::{BurnpackStore, ModuleSnapshot};

type B = NdArray<f32>;

/// Rozdzielczość wejścia, której oczekuje wyeksportowany graf RF-DETR.
const RESOLUTION: usize = 560;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("Użycie: roundtrip-rfdetr <skonwertowany.bpk> [oryginalny.bpk]");
        return std::process::ExitCode::FAILURE;
    }
    let path_new = std::path::PathBuf::from(&args[1]);
    let path_ref = args.get(2).map(std::path::PathBuf::from);

    let device = Default::default();

    // 1) Załaduj architekturę i wagi ze świeżo skonwertowanego .bpk.
    println!("[roundtrip] ładowanie skonwertowanego: {}", path_new.display());
    let model_new = match load_model(&path_new, &device) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[roundtrip] BŁĄD ładowania {}: {e}", path_new.display());
            return std::process::ExitCode::FAILURE;
        }
    };

    // 2) Forward na zerowym wejściu [1,3,560,560].
    let input = Tensor::<B, 4>::zeros([1, 3, RESOLUTION, RESOLUTION], &device);
    let (out0_new, out1_new) = model_new.forward(input.clone());
    let d0 = out0_new.dims();
    let d1 = out1_new.dims();
    println!("[roundtrip] forward OK — wyjścia: {:?} oraz {:?}", d0, d1);

    let new0: Vec<f32> = out0_new.to_data().to_vec().expect("out0 to_vec");
    let new1: Vec<f32> = out1_new.to_data().to_vec().expect("out1 to_vec");

    // 3) (Opcjonalnie) porównaj z oryginalnym .bpk pod kątem kształtu i wartości.
    if let Some(path_ref) = path_ref {
        println!("[roundtrip] ładowanie oryginalnego: {}", path_ref.display());
        let model_ref = match load_model(&path_ref, &device) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[roundtrip] BŁĄD ładowania {}: {e}", path_ref.display());
                return std::process::ExitCode::FAILURE;
            }
        };
        let (out0_ref, out1_ref) = model_ref.forward(input);
        if out0_ref.dims() != d0 || out1_ref.dims() != d1 {
            eprintln!(
                "[roundtrip] NIEZGODNOŚĆ kształtów: ref {:?}/{:?} vs new {:?}/{:?}",
                out0_ref.dims(),
                out1_ref.dims(),
                d0,
                d1
            );
            return std::process::ExitCode::FAILURE;
        }
        let ref0: Vec<f32> = out0_ref.to_data().to_vec().expect("ref0 to_vec");
        let ref1: Vec<f32> = out1_ref.to_data().to_vec().expect("ref1 to_vec");

        let diff0 = max_abs_diff(&new0, &ref0);
        let diff1 = max_abs_diff(&new1, &ref1);
        println!(
            "[roundtrip] max |Δ| wyjście0 = {:.3e}, wyjście1 = {:.3e}",
            diff0, diff1
        );

        let tol = 1e-4_f32;
        if diff0 <= tol && diff1 <= tol {
            println!("[roundtrip] WYNIK: OK — round-trip zgodny (tolerancja {tol:.0e})");
        } else {
            eprintln!("[roundtrip] WYNIK: RÓŻNICA przekracza tolerancję {tol:.0e}");
            return std::process::ExitCode::FAILURE;
        }
    } else {
        println!("[roundtrip] WYNIK: OK — ładowanie i forward skonwertowanego .bpk powiodły się");
    }

    std::process::ExitCode::SUCCESS
}

/// Buduje model z architektury generated/rfdetr.rs i ładuje wagi z pliku `.bpk`.
fn load_model(
    path: &std::path::Path,
    device: &NdArrayDevice,
) -> Result<rfdetr::Model<B>, String> {
    if !path.exists() {
        return Err(format!("plik nie istnieje: {}", path.display()));
    }
    let mut model = rfdetr::Model::<B>::new(device);
    let mut store = BurnpackStore::from_file(path);
    model
        .load_from(&mut store)
        .map_err(|e| format!("load_from: {e}"))?;
    Ok(model)
}

/// Maksymalna różnica bezwzględna między dwoma wektorami (0 gdy różne długości).
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::INFINITY;
    }
    a.iter()
        .zip(b.iter())
        .fold(0.0_f32, |acc, (x, y)| acc.max((x - y).abs()))
}
