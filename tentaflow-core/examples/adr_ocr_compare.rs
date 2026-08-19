// =============================================================================
// Plik: examples/adr_ocr_compare.rs
// Opis: Porównuje wyspecjalizowany OCR ADR z PP-OCRv5 na rzeczywistych cropach tablic ADR.
// Przykład: cargo run --release --example adr_ocr_compare -- --expected-un 1202 cropy/*.png
// =============================================================================

use std::path::Path;

use anyhow::{bail, Result};

use tentaflow_core::paths;
use tentaflow_core::vision::adr;
use tentaflow_core::vision::adr_ocr::AdrOcr;
use tentaflow_core::vision::onnx_ocr::OnnxOcrEngine;
use tentaflow_core::vision::OcrRunner;

struct Args {
    expected_un: Option<String>,
    paths: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut expected_un = None;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--expected-un" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--expected-un wymaga czterocyfrowej wartości"))?;
            if value.len() != 4 || !value.chars().all(|ch| ch.is_ascii_digit()) {
                bail!("--expected-un musi być czterocyfrowym numerem UN");
            }
            expected_un = Some(value);
        } else {
            paths.push(arg);
        }
    }
    if paths.is_empty() {
        bail!("podaj co najmniej jeden crop obrazu ADR");
    }
    Ok(Args { expected_un, paths })
}

fn strict_candidate(un: &str) -> Option<String> {
    adr::pary_kemler_un()
        .into_iter()
        .find(|(_, known_un)| known_un == un)
        .map(|(kemler, known_un)| format!("{kemler}/{known_un}"))
}

fn lowest_numeric_line(lines: &[String]) -> Option<String> {
    lines.iter().rev().find_map(|line| {
        let digits: String = line.chars().filter(|ch| ch.is_ascii_digit()).collect();
        (3..=4).contains(&digits.len()).then_some(digits)
    })
}

fn image_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let model_dir = paths::vision_models_dir();
    let adr_ocr = AdrOcr::from_dir(&model_dir)?;
    let ppocr = OnnxOcrEngine::from_dir(&model_dir)?;
    let mut specialized_exact_hits = 0usize;
    let mut ppocr_exact_hits = 0usize;

    println!("modele: {}", model_dir.display());
    if let Some(expected_un) = &args.expected_un {
        println!("oczekiwany UN: {expected_un}");
    }
    println!();

    for path in &args.paths {
        let image = image::open(path)?.to_rgb8();
        let (width, height) = image.dimensions();
        let adr_raw = adr_ocr.read_adr(image.as_raw(), width, height);
        let ppocr_lines = ppocr.read_lines(image.as_raw(), width, height)?;
        let ppocr_raw = lowest_numeric_line(&ppocr_lines);
        let adr_un = adr_raw.as_ref().map(|(_, un)| un.as_str());

        println!("{}", image_name(path));
        println!("  ADR CRNN raw: {adr_raw:?}");
        println!(
            "  ADR CRNN strict: {:?}; istniejący snap: {:?}",
            adr_un.and_then(strict_candidate),
            adr_un.and_then(adr::snap_adr)
        );
        println!("  PP-OCRv5 lines: {ppocr_lines:?}");
        println!(
            "  PP-OCRv5 raw UN: {ppocr_raw:?}; strict: {:?}; istniejący snap: {:?}",
            ppocr_raw.as_deref().and_then(strict_candidate),
            adr::snap_adr_from_lines(&ppocr_lines)
        );

        if let Some(expected_un) = &args.expected_un {
            specialized_exact_hits += usize::from(adr_un == Some(expected_un.as_str()));
            ppocr_exact_hits += usize::from(ppocr_raw.as_deref() == Some(expected_un.as_str()));
        }
    }

    if let Some(expected_un) = &args.expected_un {
        println!(
            "\nwynik raw UN={expected_un}: ADR CRNN {specialized_exact_hits}/{}; PP-OCRv5 {ppocr_exact_hits}/{}",
            args.paths.len(),
            args.paths.len()
        );
    }
    Ok(())
}
