// =============================================================================
// Plik: tools/burn-spike/src/onnx2bpk.rs
// Opis: Narzędzie CLI konwertujące wagi modelu z pliku .onnx do formatu .bpk
//       (BurnpackStore), którego wymaga runtime TentaVision.
// Przykład: cargo run -p burn-spike --bin onnx2bpk -- model.onnx model.bpk
// =============================================================================
//
// Mechanika (burn-onnx 0.21):
//   `burn_onnx::ModelGen` przy generowaniu kodu Rust z ONNX zapisuje RÓWNIEŻ
//   plik `.bpk` z wagami (patrz burn-onnx `graph.rs::with_burnpack` — wywołuje
//   `BurnpackStore::write_to_file`). Konwersja czyta inicjalizatory (wagi) z
//   grafu ONNX i serializuje je do BurnpackStore. Cała operacja dzieje się na
//   CPU — nie wymaga GPU/backendu.
//
// Narzędzie uruchamia ModelGen do katalogu tymczasowego (produkuje `<stem>.rs`
// oraz `<stem>.bpk`), po czym kopiuje wynikowy `.bpk` pod wskazaną ścieżkę.
// Wygenerowany `.rs` jest artefaktem pomocniczym — architektura runtime pochodzi
// z zatwierdzonego `tentaflow-core/src/vision/generated/*.rs`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burn_onnx::ModelGen;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Użycie: onnx2bpk <wejscie.onnx> <wyjscie.bpk>");
        eprintln!("Przykład: onnx2bpk rfdetr-base.onnx /tmp/rfdetr-base.bpk");
        return ExitCode::FAILURE;
    }

    let input = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    if let Err(e) = run(&input, &output) {
        eprintln!("[onnx2bpk] błąd: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Konwertuje pojedynczy plik ONNX do `.bpk` przez `burn_onnx::ModelGen`.
fn run(input: &Path, output: &Path) -> Result<(), String> {
    if !input.exists() {
        return Err(format!("plik wejściowy nie istnieje: {}", input.display()));
    }
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("nie można ustalić nazwy bazowej z {}", input.display()))?;

    // Unikalny katalog tymczasowy na artefakty codegenu (rs + bpk).
    let work_dir = std::env::temp_dir().join(format!(
        "onnx2bpk-{}-{}",
        std::process::id(),
        stem
    ));
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| format!("tworzenie {}: {e}", work_dir.display()))?;

    let work_dir_str = work_dir
        .to_str()
        .ok_or_else(|| "ścieżka tymczasowa nie jest poprawnym UTF-8".to_string())?;
    let input_str = input
        .to_str()
        .ok_or_else(|| "ścieżka ONNX nie jest poprawnym UTF-8".to_string())?;

    eprintln!("[onnx2bpk] konwersja {} -> {}", input.display(), output.display());
    let t0 = std::time::Instant::now();

    // ModelGen wewnętrznie panikuje przy niepoprawnym ONNX — łapiemy panikę,
    // aby zwrócić czytelny błąd zamiast surowego stack-trace.
    let input_owned = input_str.to_string();
    let work_owned = work_dir_str.to_string();
    let gen_result = std::panic::catch_unwind(move || {
        ModelGen::new()
            .input(&input_owned)
            .out_dir(&work_owned)
            .run_from_cli();
    });
    if gen_result.is_err() {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err("ModelGen nie sparsował/przekonwertował pliku ONNX (patrz log powyżej)".into());
    }

    let produced = work_dir.join(format!("{stem}.bpk"));
    if !produced.exists() {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err(format!(
            "ModelGen nie wyprodukował pliku wag: {}",
            produced.display()
        ));
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("tworzenie katalogu wyjściowego {}: {e}", parent.display()))?;
        }
    }
    std::fs::copy(&produced, output)
        .map_err(|e| format!("kopiowanie {} -> {}: {e}", produced.display(), output.display()))?;

    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work_dir);

    eprintln!(
        "[onnx2bpk] gotowe: {} ({:.1} MiB) w {:.0} ms",
        output.display(),
        bytes as f64 / (1024.0 * 1024.0),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
