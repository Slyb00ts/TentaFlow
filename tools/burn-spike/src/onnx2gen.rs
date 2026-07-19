// =============================================================================
// Plik: tools/burn-spike/src/onnx2gen.rs
// Opis: Narzędzie CLI regenerujące PARĘ artefaktów z modelu ONNX przez
//       burn_onnx::ModelGen — plik architektury `.rs` (moduł `Model`) oraz plik
//       wag `.bpk` (BurnpackStore). Runtime TentaVision ładuje wagi z `.bpk` do
//       architektury z zatwierdzonego `generated/*.rs`, więc oba muszą pochodzić
//       z tego samego ONNX.
// Przykład:
//   cargo run -p burn-spike --bin onnx2gen -- model.onnx out.rs out.bpk
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burn_onnx::ModelGen;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("Użycie: onnx2gen <wejscie.onnx> <wyjscie.rs> <wyjscie.bpk>");
        eprintln!("Przykład: onnx2gen rfdetr-base.onnx generated/rfdetr.rs rfdetr-base.bpk");
        return ExitCode::FAILURE;
    }

    let input = PathBuf::from(&args[1]);
    let out_rs = PathBuf::from(&args[2]);
    let out_bpk = PathBuf::from(&args[3]);

    if let Err(e) = run(&input, &out_rs, &out_bpk) {
        eprintln!("[onnx2gen] błąd: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Regeneruje `.rs` + `.bpk` z pojedynczego pliku ONNX przez `burn_onnx::ModelGen`.
fn run(input: &Path, out_rs: &Path, out_bpk: &Path) -> Result<(), String> {
    if !input.exists() {
        return Err(format!("plik wejściowy nie istnieje: {}", input.display()));
    }
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("nie można ustalić nazwy bazowej z {}", input.display()))?;

    // Unikalny katalog tymczasowy na artefakty codegenu (rs + bpk).
    let work_dir = std::env::temp_dir().join(format!("onnx2gen-{}-{}", std::process::id(), stem));
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| format!("tworzenie {}: {e}", work_dir.display()))?;

    let work_dir_str = work_dir
        .to_str()
        .ok_or_else(|| "ścieżka tymczasowa nie jest poprawnym UTF-8".to_string())?;
    let input_str = input
        .to_str()
        .ok_or_else(|| "ścieżka ONNX nie jest poprawnym UTF-8".to_string())?;

    eprintln!("[onnx2gen] regeneracja {} -> {} + {}", input.display(), out_rs.display(), out_bpk.display());
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

    let produced_rs = work_dir.join(format!("{stem}.rs"));
    let produced_bpk = work_dir.join(format!("{stem}.bpk"));
    if !produced_rs.exists() {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err(format!("ModelGen nie wyprodukował pliku architektury: {}", produced_rs.display()));
    }
    if !produced_bpk.exists() {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err(format!("ModelGen nie wyprodukował pliku wag: {}", produced_bpk.display()));
    }

    copy_to(&produced_rs, out_rs)?;
    copy_to(&produced_bpk, out_bpk)?;

    let rs_bytes = std::fs::metadata(out_rs).map(|m| m.len()).unwrap_or(0);
    let bpk_bytes = std::fs::metadata(out_bpk).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work_dir);

    eprintln!(
        "[onnx2gen] gotowe: {} ({} B) + {} ({:.1} MiB) w {:.0} ms",
        out_rs.display(),
        rs_bytes,
        out_bpk.display(),
        bpk_bytes as f64 / (1024.0 * 1024.0),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

/// Kopiuje plik `src` do `dst`, tworząc katalog docelowy w razie potrzeby.
fn copy_to(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("tworzenie katalogu {}: {e}", parent.display()))?;
        }
    }
    std::fs::copy(src, dst)
        .map_err(|e| format!("kopiowanie {} -> {}: {e}", src.display(), dst.display()))?;
    Ok(())
}
