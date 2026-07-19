// =============================================================================
// File: vision/ocr_plate.rs — license-plate OCR (fast-plate-ocr, ort+TRT / Burn)
// =============================================================================
//
// Reads alphanumeric plates from detector crops of class `tablica_rejestracyjna`.
// Backend inferencji wybierany cfg/feature:
//   * `inference-supertonic` (ONNX Runtime, crate `ort`) → pula sesji ort
//     (TensorRT→CUDA→CPU), model `plate_ocr.onnx`. Pula jest wewnętrznie
//     współbieżna, więc forward NIE idzie przez jednowątkowy egzekutor Burn/wgpu
//     — cold-path OCR nie serializuje się na tym wątku ani nie konkuruje z detektorem.
//   * inaczej → wendorowany `burn_plate` (build-time ONNX→Burn codegen), wagi z
//     `plate_ocr.bpk`; forward MUSI iść przez `burn_backend::run_blocking`
//     (jeden wątek GPU — równoległe forwardy wgpu psują pamięć).
//
// Preprocessing mirrors the training transform exactly: RGB crop → grayscale
// (BT.601 luma) → 140×70 bilinear stretch → raw uint8 NHWC tensor [1,70,140,1]
// with NO /255 and NO normalization (the model ingests raw 0..255 bytes).
//
// The graph emits a flat [1,333] tensor = 9 slots × 37 vocab logits (row-major:
// slot s occupies [s*vocab .. s*vocab+vocab]). Postprocessing is a per-slot
// argmax → character via the alphabet, dropping the pad character — identyczne
// dla obu backendów.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
#[cfg(not(feature = "inference-supertonic"))]
use burn::tensor::{Int, Tensor, TensorData};
#[cfg(not(feature = "inference-supertonic"))]
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_plate::Model;
use std::borrow::Cow;

/// Nazwa tensora wejściowego w grafie ONNX (`[batch,H,W,1]`, uint8 NHWC).
#[cfg(feature = "inference-supertonic")]
const INPUT_NAME: &str = "input";

/// Env sterujący rozmiarem puli sesji ort dla OCR tablic. Domyślnie 1 =
/// bit-identyczna z pojedynczą sesją; >1 pozwala wielu cropom OCR-ować równolegle.
#[cfg(feature = "inference-supertonic")]
const PLATE_SESSIONS_ENV: &str = "TENTAFLOW_PLATE_SESSIONS";
#[cfg(feature = "inference-supertonic")]
const DEFAULT_PLATE_SESSIONS: usize = 1;

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

/// Loaded plate-OCR model + decoded config + backend.
pub struct PlateOcr {
    /// Pula sesji ONNX Runtime (TensorRT→CUDA→CPU) — ścieżka ort. Wewnętrznie
    /// współbieżna, więc `read`/`decode` biorą `&self` (interior mutability).
    #[cfg(feature = "inference-supertonic")]
    pool: crate::vision::ort_common::SessionPool,
    #[cfg(not(feature = "inference-supertonic"))]
    model: Model<VisionBackend>,
    #[cfg(not(feature = "inference-supertonic"))]
    device: VisionDevice,
    alphabet: Vec<char>,
    pad: char,
    slots: usize,
    vocab: usize,
    img_h: u32,
    img_w: u32,
}

impl PlateOcr {
    /// Builds the OCR runner from the deploy-time model dir
    /// (`vision_models_dir()/plate_ocr.bpk` + `plate-ocr-config.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
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

        // Ścieżka ort+TensorRT: pula sesji na `plate_ocr.onnx`. Wejście uint8 NHWC
        // o stałym rozmiarze modelu (H×W) i dynamicznym batchu → pin batch=1.
        #[cfg(feature = "inference-supertonic")]
        {
            let onnx_path = dir.join("plate_ocr.onnx");
            if !onnx_path.exists() {
                bail!("plate-OCR ONNX missing: {}", onnx_path.display());
            }
            crate::vision::ort_common::ensure_ort_dylib();
            let trt_profile = crate::vision::ort_common::TrtShapeProfile {
                input_name: INPUT_NAME.to_string(),
                min_batch: 1,
                opt_batch: 1,
                max_batch: 1,
                // NHWC: [batch, H, W, 1] — kanał jest ostatnim wymiarem, więc
                // profil TRT opisuje channels=H, height=W, width=1.
                channels: cfg.img_height as usize,
                height: cfg.img_width,
                width: 1,
            };
            let n = crate::vision::ort_common::pool_size_from_env(
                PLATE_SESSIONS_ENV,
                DEFAULT_PLATE_SESSIONS,
            );
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &onnx_path,
                &dir.join("trt-cache-plate"),
                Some(&trt_profile),
                n,
            )?;
            info!(
                "[ocr_plate] loaded {} ({} slots, vocab {}, {}x{}, backend ort TensorRT→CUDA→CPU, pool={} session(s))",
                onnx_path.display(),
                cfg.max_plate_slots,
                cfg.vocab_size,
                cfg.img_width,
                cfg.img_height,
                pool.len()
            );
            Ok(Self {
                pool,
                alphabet,
                pad,
                slots: cfg.max_plate_slots,
                vocab: cfg.vocab_size,
                img_h: cfg.img_height,
                img_w: cfg.img_width,
            })
        }

        #[cfg(not(feature = "inference-supertonic"))]
        {
            let weights_path = dir.join("plate_ocr.bpk");
            if !weights_path.exists() {
                bail!("plate-OCR weights missing: {}", weights_path.display());
            }
            let device = burn_backend::device();
            let mut model = Model::<VisionBackend>::new(&device);
            let mut store = BurnpackStore::from_file(&weights_path);
            model
                .load_from(&mut store)
                .map_err(|e| anyhow!("load plate weights {}: {e}", weights_path.display()))?;

            info!(
                "[ocr_plate] loaded {} ({} slots, vocab {}, {}x{})",
                weights_path.display(),
                cfg.max_plate_slots,
                cfg.vocab_size,
                cfg.img_width,
                cfg.img_height
            );
            Ok(Self {
                model,
                device,
                alphabet,
                pad,
                slots: cfg.max_plate_slots,
                vocab: cfg.vocab_size,
                img_h: cfg.img_height,
                img_w: cfg.img_width,
            })
        }
    }

    /// Odczyt tablicy rejestracyjnej z jednego cropa (RGB24, `cw*ch*3`). Surowy
    /// odczyt modelu przepuszczamy przez walidację formatu PL (patrz
    /// [`waliduj_tablice_pl`]) — gdy wynik nie jest sensownym numerem (za krótki/
    /// za długi/same cyfry/obce znaki), zwracamy `None`, żeby nie pokazywać śmiecia.
    pub fn read(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        match self.decode(crop_rgb, cw, ch)? {
            Some(plate) if waliduj_tablice_pl(&plate) => Ok(Some(plate)),
            _ => Ok(None),
        }
    }

    /// Wspólny rdzeń OCR: preprocessing (upscale + resize + grayscale) → forward →
    /// argmax per slot → surowy string modelu (BEZ walidacji formatu). Używany
    /// przez [`Self::read`] (z walidacją PL). Zwraca `None`, gdy model odczytał
    /// same znaki wypełnienia.
    fn decode(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        let gray = self.preprocess(crop_rgb, cw, ch)?;
        let logits = self.forward_logits(&gray)?;

        let expected = self.slots * self.vocab;
        if logits.len() < expected {
            bail!(
                "plate OCR logits len {} < slots*vocab {}",
                logits.len(),
                expected
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
            let c = self.alphabet[best_idx];
            if c != self.pad {
                plate.push(c);
            }
        }

        if plate.is_empty() {
            Ok(None)
        } else {
            Ok(Some(plate))
        }
    }

    /// Forward jednego preprocessowanego bufora grayscale (uint8 NHWC `[1,H,W,1]`)
    /// → płaskie logity `[slots*vocab]` (row-major). Wejście to surowe 0..255 bez
    /// normalizacji — model ingeruje bajty wprost. Ścieżka ort: pojedynczy
    /// `session.run` na puli sesji; ścieżka Burn: `guarded_forward` (jeden wątek).
    #[cfg(feature = "inference-supertonic")]
    fn forward_logits(&self, gray: &[u8]) -> Result<Vec<f32>> {
        let tensor_shape = (1usize, self.img_h as usize, self.img_w as usize, 1usize);
        let input = ndarray::Array4::from_shape_vec(tensor_shape, gray.to_vec())
            .map_err(|e| anyhow!("ocr_plate: build tensor {tensor_shape:?}: {e}"))?;
        let expected = self.slots * self.vocab;

        // Forward + extraction run on the session's dedicated thread (see
        // `SessionPool::run`); only the owned logits cross back.
        self.pool.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("ocr_plate: Value::from_array: {e}"))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("ocr_plate: model has no inputs"))?;
            let output_name = session
                .outputs()
                .first()
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow!("ocr_plate: model has no outputs"))?;
            let outputs = session
                .run(ort::inputs! { input_name => value })
                .map_err(|e| anyhow!("ocr_plate: session.run: {e}"))?;
            let (shape, logits) = outputs[output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("ocr_plate: extract logits: {e}"))?;
            // Exact-shape contract `[1, slots*vocab]` (row-major flat logits). A
            // larger/differently-shaped tensor must fail loudly, not silently decode
            // from the first slots*vocab values (mirrors the classifier's strictness).
            if shape.len() != 2 || shape[0] != 1 || shape[1] as usize != expected {
                bail!("ocr_plate: output shape {shape:?} != [1, {expected}] (slots*vocab)");
            }
            if logits.len() != expected {
                bail!("ocr_plate: logits len {} != slots*vocab {expected}", logits.len());
            }
            Ok(logits.to_vec())
        })
    }

    #[cfg(not(feature = "inference-supertonic"))]
    fn forward_logits(&self, gray: &[u8]) -> Result<Vec<f32>> {
        // Raw uint8 NHWC [1, H, W, 1] as Int — the model ingests 0..255 directly.
        let data: Vec<i32> = gray.iter().map(|&b| b as i32).collect();
        let shape = [1usize, self.img_h as usize, self.img_w as usize, 1usize];
        let input =
            Tensor::<VisionBackend, 4, Int>::from_data(TensorData::new(data, shape), &self.device);
        let out = crate::vision::burn_backend::guarded_forward("plate-ocr", || {
            self.model.forward(input)
        })?;
        out.to_data()
            .to_vec()
            .map_err(|e| anyhow!("plate logits to_vec: {e:?}"))
    }

    /// RGB24 crop → raw grayscale uint8, stretch-resized to `img_w × img_h`.
    /// BT.601 luma collapses each pixel to one byte: 0.299R + 0.587G + 0.114B.
    fn preprocess(&self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
        // Małe cropy (ucięte/oddalone tablice) najpierw powiększamy, dopiero potem
        // sprowadzamy do rozmiaru modelu — patrz [`Self::maybe_upscale`].
        let (buf, sw, sh) = self.maybe_upscale(rgb, w, h)?;
        let resized = crate::vision::resize::resize_rgb(&buf, sw, sh, self.img_w, self.img_h)
            .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;

        let pixels = (self.img_w as usize) * (self.img_h as usize);
        let mut gray = Vec::with_capacity(pixels);
        for px in resized.chunks_exact(3) {
            let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
            gray.push(luma.round().clamp(0.0, 255.0) as u8);
        }
        Ok(gray)
    }

    /// Gdy źródłowy crop jest niższy niż ~2× wysokości modelu (mała rozdzielczość,
    /// np. oddalona/ucięta tablica), powiększamy go bilinearnie do wysokości
    /// `img_h * 2` (szerokość proporcjonalnie), zanim `preprocess` sprowadzi go do
    /// `img_w × img_h`. Dwustopniowy resize (upscale → downscale) daje ostrzejsze
    /// krawędzie znaków niż bezpośredni downscale z małego bufora. Gdy crop jest
    /// już wystarczająco duży — zwracamy go bez kopii (`Cow::Borrowed`).
    fn maybe_upscale<'a>(
        &self,
        rgb: &'a [u8],
        w: u32,
        h: u32,
    ) -> Result<(Cow<'a, [u8]>, u32, u32)> {
        let target_h = self.img_h.saturating_mul(2);
        if h >= target_h || h == 0 || w == 0 {
            return Ok((Cow::Borrowed(rgb), w, h));
        }
        let scale = target_h as f32 / h as f32;
        let new_w = ((w as f32 * scale).round() as u32).max(1);
        let up = crate::vision::resize::resize_rgb(rgb, w, h, new_w, target_h)
            .map_err(|e| anyhow!("upscale resize_rgb failed: {e}"))?;
        Ok((Cow::Owned(up), new_w, target_h))
    }
}

/// Waliduje odczyt jako sensowny polski numer rejestracyjny. Reguła (heurystyka
/// odrzucająca śmieci OCR, nie pełna weryfikacja wyróżników):
/// - długość 4-8 znaków,
/// - tylko wielkie litery ASCII i cyfry,
/// - 1-3 początkowe litery (wyróżnik powiatu), potem mix cyfr/liter,
/// - co najmniej jedna cyfra (odrzuca ciągi samych liter).
pub fn waliduj_tablice_pl(tekst: &str) -> bool {
    let len = tekst.chars().count();
    if !(4..=8).contains(&len) {
        return false;
    }
    if !tekst
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return false;
    }
    let leading = tekst.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    if !(1..=3).contains(&leading) {
        return false;
    }
    tekst.chars().any(|c| c.is_ascii_digit())
}
