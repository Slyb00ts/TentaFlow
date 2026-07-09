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
use crate::vision::ocr_prep;
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_plate::Model;
use std::borrow::Cow;
use std::sync::Arc;

/// Nazwa tensora wejściowego w grafie ONNX (`[batch,H,W,1]`, uint8 NHWC).
#[cfg(feature = "inference-supertonic")]
const INPUT_NAME: &str = "input";

// Plate-OCR session-pool size comes from `[vision] plate_sessions` (default 4);
// >1 lets many crops OCR concurrently.

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
            // Fixed H×W but VARIABLE batch: cold-path enrichment batches every
            // plate crop of a frame into ONE forward (`read_batch`), so pin a TRT
            // engine over 1..=max_batch and the first inference of each new batch
            // size does not trigger a per-shape rebuild. Tunable via `[vision]
            // opt_batch`/`max_batch` to match the detector/classifier cross-crop
            // batching knobs (defaults opt=8, max=16); changing these makes TRT
            // rebuild the engine on next load.
            let vision = crate::vision::settings::get();
            let opt_batch = vision.opt_batch.filter(|&n| n >= 1).unwrap_or(8);
            let max_batch = vision
                .max_batch
                .filter(|&n| n >= opt_batch)
                .unwrap_or(opt_batch.max(16));
            let trt_profile = crate::vision::ort_common::TrtShapeProfile {
                input_name: INPUT_NAME.to_string(),
                min_batch: 1,
                opt_batch,
                max_batch,
                // NHWC: [batch, H, W, 1] — kanał jest ostatnim wymiarem, więc
                // profil TRT opisuje channels=H, height=W, width=1.
                channels: cfg.img_height as usize,
                height: cfg.img_width,
                width: 1,
            };
            let n = crate::vision::ort_common::pool_size(vision.plate_sessions);
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &onnx_path,
                &dir.join("trt-cache-plate"),
                Some(&trt_profile),
                n,
                // FP32 — fp16 flips plate glyphs (3↔2, 8↔0). See `ocr_fp16`.
                crate::vision::ort_common::ocr_fp16(),
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
        let deskew = ocr_prep::deskew_enabled();
        let dump = ocr_prep::dump_dir().is_some();
        let (gray, deskewed) = self.preprocess(crop_rgb, cw, ch, deskew, dump)?;
        let logits = self.forward_logits(&gray)?;
        let (raw, score) = self.decode_logits_scored(&logits)?;
        if dump {
            ocr_prep::dump_ocr_sample(
                "plate",
                crop_rgb,
                cw,
                ch,
                deskewed.as_ref().map(|(d, dw, dh)| (d.as_slice(), *dw, *dh)),
                &gray,
                self.img_w,
                self.img_h,
                raw.as_deref(),
                score,
            );
        }
        Ok(raw)
    }

    /// A/B one crop through both preprocessing paths (current stretch vs. deskew)
    /// on the SAME loaded model, returning the validated read + confidence for
    /// each. Used by the offline A/B harness (`examples/ocr_deskew_ab.rs`) so the
    /// accuracy delta is measured, not assumed. Not on any hot path.
    pub fn read_ab(
        &self,
        crop_rgb: &[u8],
        cw: u32,
        ch: u32,
    ) -> Result<((Option<String>, f32), (Option<String>, f32))> {
        let run = |deskew: bool| -> Result<(Option<String>, f32)> {
            let (gray, _) = self.preprocess(crop_rgb, cw, ch, deskew, false)?;
            let logits = self.forward_logits(&gray)?;
            let (raw, score) = self.decode_logits_scored(&logits)?;
            let validated = raw.filter(|p| waliduj_tablice_pl(p));
            Ok((validated, score))
        };
        Ok((run(false)?, run(true)?))
    }

    /// Batched read: `crops` (RGB24 + wymiary) w JEDNYM forwardzie na modelu
    /// dynamic-batch (`[n,H,W,1]` uint8 NHWC), zamiast n forwardów batch=1.
    /// Reużywa `preprocess` (deskew + grayscale + resize) + `decode_logits_scored`
    /// + walidację PL,
    /// więc wynik per crop jest bit-identyczny ze ścieżką [`Self::read`]. Wyjście
    /// `[n, slots*vocab]` slice'owane per crop — kolejność == kolejność `crops`,
    /// długość == `crops.len()`. Odczyt niezwalidowany (patrz [`waliduj_tablice_pl`])
    /// → `None` w danym slocie. Wzoruje `adr_ocr::forward_batch`.
    #[cfg(feature = "inference-supertonic")]
    pub fn read_batch(&self, crops: &[(Arc<[u8]>, u32, u32)]) -> Result<Vec<Option<String>>> {
        if crops.is_empty() {
            return Ok(Vec::new());
        }
        let n = crops.len();
        let pixels = self.img_h as usize * self.img_w as usize;
        let deskew = ocr_prep::deskew_enabled();
        let dump = ocr_prep::dump_dir().is_some();
        // Preprocessing WSPÓŁDZIELONY z `read` — każdy crop → grayscale [H*W] w
        // spójny wycinek bufora batcha; sloty ułożone row-major [n,H,W,1].
        let mut data = vec![0u8; n * pixels];
        // Retained ONLY for the crop dump (env-gated) — zero extra work otherwise.
        let mut dump_grays: Vec<Vec<u8>> = if dump { Vec::with_capacity(n) } else { Vec::new() };
        let mut dump_deskews: Vec<Option<(Vec<u8>, u32, u32)>> =
            if dump { Vec::with_capacity(n) } else { Vec::new() };
        for (i, (crop, cw, ch)) in crops.iter().enumerate() {
            let (gray, deskewed) = self.preprocess(crop, *cw, *ch, deskew, dump)?;
            data[i * pixels..(i + 1) * pixels].copy_from_slice(&gray);
            if dump {
                dump_deskews.push(deskewed);
                dump_grays.push(gray);
            }
        }
        let tensor_shape = (n, self.img_h as usize, self.img_w as usize, 1usize);
        let input = ndarray::Array4::from_shape_vec(tensor_shape, data)
            .map_err(|e| anyhow!("ocr_plate: build batch tensor {tensor_shape:?}: {e}"))?;
        let expected = self.slots * self.vocab;

        let per_item: Vec<Vec<f32>> = self.pool.run(move |session| {
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
            if shape.len() != 2 || shape[0] as usize != n || shape[1] as usize != expected {
                bail!("ocr_plate: batch output shape {shape:?} != [{n}, {expected}] (slots*vocab)");
            }
            if logits.len() < n * expected {
                bail!("ocr_plate: batch logits len {} < {n}*{expected}", logits.len());
            }
            Ok((0..n)
                .map(|i| logits[i * expected..(i + 1) * expected].to_vec())
                .collect())
        })?;

        per_item
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let (raw, score) = self.decode_logits_scored(l)?;
                if dump {
                    let (crop, cw, ch) = &crops[i];
                    ocr_prep::dump_ocr_sample(
                        "plate",
                        crop,
                        *cw,
                        *ch,
                        dump_deskews[i].as_ref().map(|(d, dw, dh)| (d.as_slice(), *dw, *dh)),
                        &dump_grays[i],
                        self.img_w,
                        self.img_h,
                        raw.as_deref(),
                        score,
                    );
                }
                Ok(match raw {
                    Some(plate) if waliduj_tablice_pl(&plate) => Some(plate),
                    _ => None,
                })
            })
            .collect()
    }

    /// Ścieżka Burn: model wkompilowany pod `[1,H,W,1]`, więc batch to pętla po
    /// pojedynczych forwardach (jak `adr_ocr::forward_batch` w wariancie tract).
    #[cfg(not(feature = "inference-supertonic"))]
    pub fn read_batch(&self, crops: &[(Arc<[u8]>, u32, u32)]) -> Result<Vec<Option<String>>> {
        crops
            .iter()
            .map(|(crop, cw, ch)| self.read(crop, *cw, *ch))
            .collect()
    }

    /// Per-slot argmax płaskich logitów `[slots*vocab]` (row-major) → surowy
    /// string tablicy (BEZ walidacji formatu), z pominięciem znaku wypełnienia.
    /// Wydzielone z [`Self::decode`], by ścieżka pojedyncza i [`Self::read_batch`]
    /// dekodowały identycznie. `None`, gdy same znaki wypełnienia.
    /// Per-slot argmax → raw plate string plus a mean confidence: the average
    /// softmax probability of the chosen character across non-pad slots (0..1).
    /// The confidence is used for the crop-dump filename so a human can rank
    /// dumped reads; the string is identical to a plain per-slot argmax decode.
    fn decode_logits_scored(&self, logits: &[f32]) -> Result<(Option<String>, f32)> {
        let expected = self.slots * self.vocab;
        if logits.len() < expected {
            bail!(
                "plate OCR logits len {} < slots*vocab {}",
                logits.len(),
                expected
            );
        }

        let mut plate = String::with_capacity(self.slots);
        let mut score_sum = 0.0f32;
        let mut score_n = 0usize;
        for s in 0..self.slots {
            let slot = &logits[s * self.vocab..s * self.vocab + self.vocab];
            let (best_idx, prob) = softmax_argmax(slot);
            let c = self.alphabet[best_idx];
            if c != self.pad {
                plate.push(c);
                score_sum += prob;
                score_n += 1;
            }
        }

        let score = if score_n > 0 {
            score_sum / score_n as f32
        } else {
            0.0
        };
        if plate.is_empty() {
            Ok((None, score))
        } else {
            Ok((Some(plate), score))
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

    /// RGB24 crop → raw grayscale uint8, resized to `img_w × img_h`.
    /// BT.601 luma collapses each pixel to one byte: 0.299R + 0.587G + 0.114B.
    ///
    /// When `deskew` is set the padded crop is first perspective-rectified to an
    /// upright frontal plate (`ocr_prep::deskew_plate_rgb`); angled plates then
    /// reach the model un-keystoned instead of stretched. If no confident plate
    /// quad is found the raw crop is used unchanged, so this never reads worse
    /// than the previous stretch path. `keep_deskew` retains the rectified crop
    /// (for the env-gated dump); it is `None` otherwise (zero extra allocation).
    fn preprocess(
        &self,
        rgb: &[u8],
        w: u32,
        h: u32,
        deskew: bool,
        keep_deskew: bool,
    ) -> Result<(Vec<u8>, Option<(Vec<u8>, u32, u32)>)> {
        let deskewed = if deskew {
            ocr_prep::deskew_plate_rgb(rgb, w, h)
        } else {
            None
        };
        let (src, sw, sh): (&[u8], u32, u32) = match &deskewed {
            Some((d, dw, dh)) => (d.as_slice(), *dw, *dh),
            None => (rgb, w, h),
        };

        // Małe cropy (ucięte/oddalone tablice) najpierw powiększamy, dopiero potem
        // sprowadzamy do rozmiaru modelu — patrz [`Self::maybe_upscale`].
        let (buf, uw, uh) = self.maybe_upscale(src, sw, sh)?;
        let resized = crate::vision::resize::resize_rgb(&buf, uw, uh, self.img_w, self.img_h)
            .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;

        let pixels = (self.img_w as usize) * (self.img_h as usize);
        let mut gray = Vec::with_capacity(pixels);
        for px in resized.chunks_exact(3) {
            let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
            gray.push(luma.round().clamp(0.0, 255.0) as u8);
        }
        let kept = if keep_deskew { deskewed } else { None };
        Ok((gray, kept))
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

/// Argmax of a logit slot plus the softmax probability of that argmax
/// (numerically stable). Used only to attach a confidence to dumped reads.
fn softmax_argmax(row: &[f32]) -> (usize, f32) {
    let mut max = f32::NEG_INFINITY;
    let mut best = 0usize;
    for (i, &v) in row.iter().enumerate() {
        if v > max {
            max = v;
            best = i;
        }
    }
    let mut sum = 0.0f32;
    for &v in row {
        sum += (v - max).exp();
    }
    let prob = if sum > 0.0 { 1.0 / sum } else { 0.0 };
    (best, prob)
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
