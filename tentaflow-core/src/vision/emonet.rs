// =============================================================================
// Plik: vision/emonet.rs
// Opis: EmoNet (face-analysis/emonet) — emocje + valence/arousal. UPSTREAM
//       brak publicznego ONNX (wylacznie PyTorch checkpoint), wiec setup.sh
//       nie pobiera modelu i runtime dostaje pusty placeholder. `load()`
//       failuje czysto — deploy handler propaguje blad do GUI.
//
//       Po manualnym eksporcie z PyTorch (`emonet_8.onnx`) i wrzuceniu do
//       `tentaflow-core/models/vision/emonet.onnx`, build.rs wciagnie plik
//       do binarki przy nastepnym `cargo build` i ten silnik bedzie dzialal.
// =============================================================================

use std::path::Path;

use anyhow::{anyhow, Result};

pub struct EmonetEngine;

pub fn load(model_path: &Path) -> Result<EmonetEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "EmoNet ONNX nie dostarczony — upstream udostepnia tylko PyTorch. \
             Eksportuj recznie i wrzuc do {}",
            model_path.display()
        ));
    }
    Err(anyhow!(
        "EmoNet inference jeszcze nie wpiety — load() po dostarczeniu ONNX'a"
    ))
}
