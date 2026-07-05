// =============================================================================
// File: examples/adr_probe.rs — real-image ADR OCR probe (PP-OCRv5 + snap)
// =============================================================================
//
// Verifies the ADR reading path end-to-end on REAL cropped ADR plates:
//   crop → OnnxOcrEngine::read_lines (PP-OCRv5 det→rec) → adr::snap_adr_from_lines
//   → "<kemler>/<un> <opis>" from adr-list.json.
//
// No Tesseract anywhere. Pass one or more image paths as CLI args.
//   cargo run --release --features inference-vision-gpu --example adr_probe -- crops/*.png
#![cfg(feature = "inference-vision-gpu")]

use anyhow::Result;

use tentaflow_core::paths;
use tentaflow_core::vision::adr;
use tentaflow_core::vision::onnx_ocr::OnnxOcrEngine;
use tentaflow_core::vision::OcrRunner;

fn main() -> Result<()> {
    let dir = paths::vision_models_dir();
    println!("PP-OCRv5 z: {}", dir.display());
    let engine = OnnxOcrEngine::from_dir(&dir)?;
    println!("model załadowany.\n");

    let mut hit = 0usize;
    let mut total = 0usize;
    for path in std::env::args().skip(1) {
        total += 1;
        let img = image::open(&path)?.to_rgb8();
        let (w, h) = img.dimensions();
        let lines = engine.read_lines(img.as_raw(), w, h)?;
        let snap = adr::snap_adr_from_lines(&lines);
        if snap.is_some() {
            hit += 1;
        }
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        println!("{name}: linie={lines:?} -> ADR={snap:?}");
    }
    println!("\n== {hit}/{total} cropów dopasowano do listy ADR ==");
    Ok(())
}
