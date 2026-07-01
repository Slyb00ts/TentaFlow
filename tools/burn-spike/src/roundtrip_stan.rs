// =============================================================================
// Plik: tools/burn-spike/src/roundtrip_stan.rs
// Opis: Walidacja ładowania wag stanu (model_stan.bpk) do zregenerowanej
//       architektury generated/stan.rs oraz kształtu wyjścia forward [1,4].
// Przykład:
//   cargo run -p burn-spike --bin roundtrip-stan -- .runtime/models/vision/model_stan.bpk
// =============================================================================

mod stan {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tentaflow-core/src/vision/generated/stan.rs"
    ));
}

use burn::backend::NdArray;
use burn::tensor::Tensor;
use burn_store::{BurnpackStore, ModuleSnapshot};

type B = NdArray<f32>;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Użycie: roundtrip-stan <model_stan.bpk>");
        return std::process::ExitCode::FAILURE;
    }
    let path = std::path::PathBuf::from(&args[1]);
    let device = Default::default();

    let mut model = stan::Model::<B>::new(&device);
    let mut store = BurnpackStore::from_file(&path);
    if let Err(e) = model.load_from(&mut store) {
        eprintln!("[roundtrip-stan] BŁĄD ładowania {}: {e}", path.display());
        return std::process::ExitCode::FAILURE;
    }
    let input = Tensor::<B, 4>::zeros([1, 3, 224, 224], &device);
    let out = model.forward(input);
    println!("[roundtrip-stan] forward OK — wyjście: {:?}", out.dims());
    println!("[roundtrip-stan] WYNIK: OK");
    std::process::ExitCode::SUCCESS
}
