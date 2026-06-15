// =============================================================================
// File: vision/ocr_plate.rs — license-plate OCR (fast-plate-ocr, ONNX via `ort`)
// =============================================================================
//
// Reads alphanumeric license plates from detector crops of class
// `tablica_rejestracyjna`. Loads `plate_ocr.onnx` (the exported fast-plate-ocr
// CCT model) through ONNX Runtime and turns a crop into a plate string.
//
// Preprocessing mirrors the training transform exactly: RGB crop → grayscale
// (BT.601 luma) → 140×70 bilinear stretch → raw uint8 NHWC tensor [1,70,140,1]
// with NO /255 and NO normalization (the model ingests raw 0..255 bytes).
//
// The graph emits a flat [1,333] tensor = 9 slots × 37 vocab logits (row-major:
// slot s occupies [s*vocab .. s*vocab+vocab]). Postprocessing is a per-slot
// argmax → character via the alphabet, dropping the pad character. An empty
// result (all pads) maps to `None`.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
use ort::session::Session;
use ort::value::Value;
use serde::Deserialize;
use tracing::info;

use crate::paths;

/// `plate-ocr-config.json` shape — the deploy-time config next to the model.
#[derive(Debug, Deserialize)]
struct OcrConfig {
    alphabet: String,
    pad_char: String,
    max_plate_slots: usize,
    vocab_size: usize,
    img_height: u32,
    img_width: u32,
}

/// Loaded plate-OCR session plus its decoded config. `Session::run` needs
/// `&mut`, so `read` takes `&mut self`; a single shared instance is driven from
/// one analysis task (or behind a mutex when shared across cameras).
pub struct PlateOcr {
    session: Session,
    /// Alphabet as chars, indexed by the per-slot argmax (row-major vocab).
    alphabet: Vec<char>,
    pad: char,
    slots: usize,
    vocab: usize,
    img_h: u32,
    img_w: u32,
    /// Graph input tensor name, read from the model so we do not hard-code it.
    input_name: String,
}

impl PlateOcr {
    /// Builds the OCR runner from the deploy-time model dir
    /// (`vision_models_dir()/plate_ocr.onnx` + `plate-ocr-config.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let model_path = dir.join("plate_ocr.onnx");
        let config_path = dir.join("plate-ocr-config.json");

        let config_bytes = std::fs::read(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let cfg: OcrConfig = serde_json::from_slice(&config_bytes)
            .with_context(|| format!("parse {}", config_path.display()))?;

        let alphabet: Vec<char> = cfg.alphabet.chars().collect();
        if alphabet.is_empty() {
            bail!("plate-ocr-config.json: alphabet is empty");
        }
        if alphabet.len() != cfg.vocab_size {
            bail!(
                "plate-ocr-config.json: alphabet len {} != vocab_size {}",
                alphabet.len(),
                cfg.vocab_size
            );
        }
        if cfg.max_plate_slots == 0 {
            bail!("plate-ocr-config.json: max_plate_slots must be > 0");
        }
        if cfg.img_height == 0 || cfg.img_width == 0 {
            bail!("plate-ocr-config.json: img_height/img_width must be > 0");
        }
        let pad = cfg
            .pad_char
            .chars()
            .next()
            .ok_or_else(|| anyhow!("plate-ocr-config.json: pad_char is empty"))?;

        let session = super::ort_session::build_session(&model_path)?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("plate OCR model has no inputs"))?;

        info!(
            "[ocr_plate] loaded {} ({} slots, vocab {}, {}x{}, input '{}')",
            model_path.display(),
            cfg.max_plate_slots,
            cfg.vocab_size,
            cfg.img_width,
            cfg.img_height,
            input_name
        );
        Ok(Self {
            session,
            alphabet,
            pad,
            slots: cfg.max_plate_slots,
            vocab: cfg.vocab_size,
            img_h: cfg.img_height,
            img_w: cfg.img_width,
            input_name,
        })
    }

    /// Runs one crop through the OCR model. `crop_rgb` is tightly packed RGB24 of
    /// size `cw*ch*3`. Returns the recognized plate string, or `None` when the
    /// model reads only pad characters (no plate).
    pub fn read(&mut self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        let gray = self.preprocess(crop_rgb, cw, ch)?;
        // Raw uint8 NHWC [1, H, W, 1]: the model ingests 0..255 bytes directly,
        // so the element type of the tensor must be u8 (no /255, no normalize).
        let shape = [1usize, self.img_h as usize, self.img_w as usize, 1usize];
        let input_value = Value::from_array((shape, gray))?;

        let outputs = self
            .session
            .run(ort::inputs! { self.input_name.as_str() => &input_value })?;

        let output = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow!("plate OCR produced no outputs"))?
            .1;
        let (logits_shape, logits) = output.try_extract_tensor::<f32>()?;

        let expected = self.slots * self.vocab;
        if logits.len() < expected {
            bail!(
                "plate OCR logits len {} < slots*vocab {} (shape {:?})",
                logits.len(),
                expected,
                &*logits_shape
            );
        }

        let mut plate = String::with_capacity(self.slots);
        for s in 0..self.slots {
            let slot = &logits[s * self.vocab..s * self.vocab + self.vocab];
            let mut best_idx = 0usize;
            let mut best_logit = f32::NEG_INFINITY;
            for (idx, &l) in slot.iter().enumerate() {
                if l > best_logit {
                    best_logit = l;
                    best_idx = idx;
                }
            }
            let ch = self.alphabet[best_idx];
            if ch != self.pad {
                plate.push(ch);
            }
        }

        if plate.is_empty() {
            Ok(None)
        } else {
            Ok(Some(plate))
        }
    }

    /// RGB24 crop → raw grayscale uint8, stretch-resized to `img_w × img_h`.
    /// Resizing happens on RGB (the SIMD resizer is RGB-only), then BT.601 luma
    /// collapses each pixel to one byte: 0.299R + 0.587G + 0.114B.
    fn preprocess(&self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
        let resized = crate::vision::resize::resize_rgb(rgb, w, h, self.img_w, self.img_h)
            .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;

        let pixels = (self.img_w as usize) * (self.img_h as usize);
        let mut gray = Vec::with_capacity(pixels);
        for px in resized.chunks_exact(3) {
            let r = px[0] as f32;
            let g = px[1] as f32;
            let b = px[2] as f32;
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            gray.push(luma.round().clamp(0.0, 255.0) as u8);
        }
        Ok(gray)
    }
}
