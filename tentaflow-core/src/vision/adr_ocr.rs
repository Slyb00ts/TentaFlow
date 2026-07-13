// =============================================================================
// Plik: vision/adr_ocr.rs
// Opis: Nasz wytrenowany czytnik numerów ADR (mały CRNN, ~4 MB). Czyta
//       pomarańczową planszę Kemler/UN: górny wiersz (kemler) i dolny (numer UN),
//       z wyszukiwaniem orientacji (tablice VID bywają obrócone o ~90°).
//       Backend inferencji wybierany cfg/feature — TAK SAMO jak `ocr_plate`:
//         * `inference-supertonic` (ONNX Runtime, crate `ort`) → pula sesji ort
//           (TensorRT→CUDA→CPU) na `adr_ocr.onnx`. OCR biegnie na GPU; forward NIE
//           serializuje się na jednowątkowym egzekutorze CPU. To ścieżka produkcyjna.
//         * inaczej → `tract-onnx` (pure Rust, CPU) na tym samym `adr_ocr.onnx`.
//       Algorytm 1:1 z `scripts/train-adr-ocr/eval.py` (identyczny dla obu backendów):
//         * preprocessing: grayscale → resize 32×128 (H×W) → (pix/255-0.5)/0.5,
//         * model: wejście [1,1,32,128], wyjście logity [1,T,C], C=11 (10 cyfr +
//           blank na indeksie 0), alfabet `0123456789`,
//         * CTC greedy decode (blank=0, kompresja powtórzeń), pewność = średnia
//           softmax-max po wybranych krokach,
//         * split górny/dolny wiersz z 6% przerwą wokół linii środkowej,
//         * orientation-search 0/90/180/270 z heurystyką długości,
//         * `read_adr_batch`: cross-placard batch — wiersze WIELU tablic w
//           jednym forwardzie `[total_rows,1,32,128]` (ścieżka pojedyncza jest
//           batchem o jednym elemencie, więc wyniki są identyczne).
//       Numer UN dociągamy do katalogu przez `adr::snap_adr`. Ten silnik jest
//       GŁÓWNYM czytnikiem ADR; PP-OCRv5 (`onnx_ocr`) jest fallbackiem, gdy nasz
//       model jest niedostępny lub nic nie odczyta (patrz `local_cv::ocr_adr_local`).
// =============================================================================

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use tracing::warn;

#[cfg(not(feature = "inference-supertonic"))]
use anyhow::bail;
#[cfg(not(feature = "inference-supertonic"))]
use tract_onnx::prelude::*;

#[cfg(not(feature = "inference-supertonic"))]
type Runnable = RunnableModel<TypedFact, Box<dyn TypedOp>>;

/// Wysokość i szerokość wejścia modelu (H×W), zgodnie z `Reader._prep`.
const IMG_H: u32 = 32;
const IMG_W: u32 = 128;
/// Przerwa wokół linii środkowej przy podziale na wiersze (`split_rows`).
const SPLIT_MARGIN: f32 = 0.06;

// --- Row content-trim before resize (`[vision] adr_row_trim`, default ON) ---
// A row from `split_rows` still holds the full placard width: the dark metal
// frame, the surrounding gray background AND the bright orange field with the
// digits centered in it. A 2-digit top row ("99") therefore occupies only the
// CENTER of that width — stretched to `IMG_W` the digits shrink and the empty
// orange margins / frame get decoded as EXTRA digits ("99" -> "396"). Trimming
// the row to the DARK digit content before the resize makes a 2-digit row fill
// the frame like a 4-digit row already does. The bottom row ("3257") already
// spans most of the width, so the trim is a near no-op there (guarded so it can
// never crop tighter than the digits).
/// A column/row counts as "ink" when at least this fraction of the perpendicular
/// dimension is dark. Low enough to catch thin digit strokes.
const ADR_TRIM_CONTENT_FRAC: f32 = 0.08;
/// An ink run whose mean darkness reaches this fraction of the perpendicular
/// dimension is a solid full-extent bar (placard frame / row divider), NOT a
/// digit — digits never fill more than ~half of a row's height in one column.
const ADR_TRIM_SOLID_FRAC: f32 = 0.60;
/// Only solid runs whose center lies in the OUTER band of the axis are treated
/// as frame/border bars and dropped; the digits sit centrally, so this never
/// eats a real digit even a slim "1".
const ADR_TRIM_OUTER_FRAC: f32 = 0.25;
/// Padding added around the detected digit span, as a fraction of the digit
/// band height (so the digits are not glued to the frame edge after cropping).
const ADR_TRIM_PAD_FRAC: f32 = 0.10;
/// Refuse to crop below this fraction of the original width — a single stray
/// dark pixel must not collapse the row.
const ADR_TRIM_MIN_W_FRAC: f32 = 0.12;
/// Refuse to crop below this fraction of the original height.
const ADR_TRIM_MIN_H_FRAC: f32 = 0.20;
/// If the trimmed extent still covers this much of an axis it is already tight
/// (e.g. the bottom row): keep that axis unchanged so the trim can never regress
/// a row that reads correctly today.
const ADR_TRIM_KEEP_W_FRAC: f32 = 0.90;
const ADR_TRIM_KEEP_H_FRAC: f32 = 0.95;

/// Per-call row-trim override for the `adr_rowtrim_ab` A/B harness: `0` =
/// follow `[vision] adr_row_trim`, `1` = forced OFF, `2` = forced ON. The
/// production path never touches this — it stays at the settings value.
static ROW_TRIM_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Bench-only programmatic override so the A/B harness can flip the row trim
/// per read within one process (the settings are frozen process-wide).
pub fn set_row_trim_override(enabled: bool) {
    ROW_TRIM_OVERRIDE.store(
        if enabled { 2 } else { 1 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Whether to content-trim each row before the 32×128 resize.
/// `[vision] adr_row_trim` — default ON; `false` disables (A/B toggle).
fn adr_row_trim() -> bool {
    match ROW_TRIM_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => crate::vision::settings::get().adr_row_trim,
    }
}

/// Ile orientacji tablicy próbować w `read_adr` (1=tylko pionowa, 4=pełny obrót
/// 0/90/180/270). Domyślnie 1: kamery stacjonarne widzą planszę pionowo, więc
/// obroty to zwykle marnowane forwardy. `[vision] adr_orientations` (1..=4).
fn adr_orientations() -> usize {
    crate::vision::settings::get().adr_orientations.clamp(1, 4)
}

/// Nazwy plików bundla w `vision_models_dir()`.
const MODEL_FILE: &str = "adr_ocr.onnx";
const ALPHABET_FILE: &str = "adr_ocr_alphabet.txt";

// ADR-OCR session-pool size comes from `[vision] adr_sessions` (default 4);
// >1 lets many ADR crops OCR concurrently (orientation-search does several
// forwards per crop, so the pool helps with many placards in frame).

/// TRT dynamic-batch profile bounds for the ROW batch (`[rows,1,32,128]`).
/// Cross-placard batching (`read_adr_batch`) packs `2×orientations` rows per
/// placard, so the max covers a full 16-placard batcher flush at the default
/// 1 orientation (16 × 2 = 32 rows). Forwards larger than the max are chunked
/// (`forward_batch_chunked`) so an input never falls outside the pinned TRT
/// shape range — out-of-range inputs would force a slow engine rebuild.
const ADR_ROWS_MAX_BATCH: usize = 32;
#[cfg(feature = "inference-supertonic")]
const ADR_ROWS_OPT_BATCH: usize = 16;

/// Ładuje model CRNN (tract) z USTALONYM wejściem `[1,1,32,128]` (NCHW f32). Model
/// niesie dynamiczne kształty (Shape/Gather/Reshape wokół LSTM), więc czyścimy
/// fakty wyjść pośrednich i pozwalamy tractowi wywnioskować kształty z wejścia.
#[cfg(not(feature = "inference-supertonic"))]
fn load_fixed_input(path: &Path) -> Result<Arc<Runnable>> {
    let mut model = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("tract: ADR OCR ONNX z {}", path.display()))?;

    let inputs: Vec<_> = model.input_outlets()?.to_vec();
    let node_count = model.nodes().len();
    for nid in 0..node_count {
        let slots = model.node(nid).outputs.len();
        for slot in 0..slots {
            let outlet = OutletId::new(nid, slot);
            if inputs.contains(&outlet) {
                continue;
            }
            model.set_outlet_fact(outlet, InferenceFact::default())?;
        }
    }

    model.set_input_fact(
        0,
        InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 1, IMG_H as i32, IMG_W as i32)),
    )?;
    Ok(model.into_optimized()?.into_runnable()?)
}

pub struct AdrOcr {
    /// Pula sesji ONNX Runtime (TensorRT→CUDA→CPU) — ścieżka ort (GPU).
    /// Wewnętrznie współbieżna, więc `read_adr`/`read_row` biorą `&self`.
    #[cfg(feature = "inference-supertonic")]
    pool: crate::vision::ort_common::SessionPool,
    /// Model tract (pure Rust, CPU) — ścieżka fallback bez feature ort.
    #[cfg(not(feature = "inference-supertonic"))]
    model: Arc<Runnable>,
    /// Alfabet klas (bez blanku): klasa `v` (v≠0) mapuje na `alphabet[v-1]`.
    alphabet: Vec<char>,
}

impl AdrOcr {
    /// Buduje silnik z plików bundla w `vision_models_dir()`. Błąd, gdy brak
    /// modelu albo alfabetu (wtedy `local_cv` schodzi na fallback PP-OCRv5).
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let model_path = dir.join(MODEL_FILE);
        let alphabet_path = dir.join(ALPHABET_FILE);

        if !model_path.exists() {
            return Err(anyhow!(
                "adr-ocr: brak modelu {} (bundle nie pobrany)",
                model_path.display()
            ));
        }
        let alphabet_raw = std::fs::read_to_string(&alphabet_path)
            .with_context(|| format!("adr-ocr: alfabet {}", alphabet_path.display()))?;
        let alphabet: Vec<char> = alphabet_raw.trim().chars().collect();
        if alphabet.is_empty() {
            return Err(anyhow!(
                "adr-ocr: alfabet {} jest pusty",
                alphabet_path.display()
            ));
        }

        // ort+TensorRT path: session pool on `adr_ocr.onnx`. The NCHW input
        // [rows,1,32,128] has a VARIABLE batch (2×orientations rows per placard,
        // and `read_adr_batch` packs the rows of MANY placards), so pin a TRT
        // profile over 1..=ADR_ROWS_MAX_BATCH — without it the TRT EP would
        // rebuild an engine on the first forward of EVERY new batch size. The
        // LSTM in the graph internally falls to the CUDA EP (still GPU).
        #[cfg(feature = "inference-supertonic")]
        {
            crate::vision::ort_common::ensure_ort_dylib();
            let n =
                crate::vision::ort_common::pool_size(crate::vision::settings::get().adr_sessions);
            let trt_profile = std::fs::read(&model_path)
                .ok()
                .and_then(|bytes| crate::vision::ort_common::onnx_first_input_name(&bytes))
                .map(|input_name| crate::vision::ort_common::TrtShapeProfile {
                    input_name,
                    min_batch: 1,
                    opt_batch: ADR_ROWS_OPT_BATCH,
                    max_batch: ADR_ROWS_MAX_BATCH,
                    channels: 1,
                    height: IMG_H,
                    width: IMG_W,
                });
            if trt_profile.is_none() {
                warn!(
                    "[adr-ocr] could not resolve the ONNX input name; TRT falls back to lazy per-batch-size engine builds"
                );
            }
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &model_path,
                &dir.join("trt-cache-adr"),
                trt_profile.as_ref(),
                n,
                // FP32 — fp16 corrupts Kemler/UN digit reads (30/1863 → 20/1066).
                crate::vision::ort_common::ocr_fp16(),
            )?;
            tracing::info!(
                "[adr-ocr] loaded {} (alphabet {} chars, backend ort TensorRT→CUDA→CPU, pool={} session(s))",
                model_path.display(),
                alphabet.len(),
                pool.len()
            );
            Ok(Self { pool, alphabet })
        }

        #[cfg(not(feature = "inference-supertonic"))]
        {
            let model = load_fixed_input(&model_path)?;
            tracing::info!(
                "[adr-ocr] loaded {} (alphabet {} chars, backend tract CPU)",
                model_path.display(),
                alphabet.len()
            );
            Ok(Self { model, alphabet })
        }
    }

    /// Batched forward: `data` = `n` sklejonych wejść `[1,32,128]` (row-major),
    /// zwraca `n` × `(logits, T, C)` z wyjścia `[n,T,C]`. ORT robi to JEDNYM
    /// wywołaniem sesji (model jest dynamic-batch), więc 8 orientacji ADR liczy się
    /// jednym forwardem zamiast ośmiu — to główny zysk (8.3 ms → ~1.5 ms/tablicę).
    #[cfg(feature = "inference-supertonic")]
    fn forward_batch(&self, data: Vec<f32>, n: usize) -> Result<Vec<(Vec<f32>, usize, usize)>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let input =
            ndarray::Array4::from_shape_vec((n, 1usize, IMG_H as usize, IMG_W as usize), data)
                .map_err(|e| anyhow!("adr-ocr: budowa tensora batch: {e}"))?;
        self.pool.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("adr-ocr: Value::from_array: {e}"))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("adr-ocr: model has no inputs"))?;
            let output_name = session
                .outputs()
                .first()
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow!("adr-ocr: model has no outputs"))?;
            let outputs = session
                .run(ort::inputs! { input_name => value })
                .map_err(|e| anyhow!("adr-ocr: session.run: {e}"))?;
            let (shape, logits) = outputs[output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("adr-ocr: extract logits: {e}"))?;
            if shape.len() != 3 || shape[0] as usize != n {
                return Err(anyhow!(
                    "adr-ocr: kształt wyjścia {shape:?}, oczekiwano [{n},T,C]"
                ));
            }
            let (t, c) = (shape[1] as usize, shape[2] as usize);
            let per = t.saturating_mul(c);
            if per == 0 || logits.len() < n * per {
                return Err(anyhow!(
                    "adr-ocr: za mało logitów: {} < {n}*{per}",
                    logits.len()
                ));
            }
            Ok((0..n)
                .map(|i| (logits[i * per..(i + 1) * per].to_vec(), t, c))
                .collect())
        })
    }

    /// Forward pojedynczego wejścia `[1,1,32,128]` przez tract (CPU) — fallback.
    #[cfg(not(feature = "inference-supertonic"))]
    fn forward_single(&self, data: Vec<f32>) -> Result<(Vec<f32>, usize, usize)> {
        let input: Tensor =
            tract_ndarray::Array4::from_shape_vec((1, 1, IMG_H as usize, IMG_W as usize), data)
                .map_err(|e| anyhow!("adr-ocr: budowa tensora: {e}"))?
                .into();
        let outputs = self
            .model
            .run(tvec!(input.into()))
            .map_err(|e| anyhow!("adr-ocr: forward: {e}"))?;
        let out = &outputs[0];
        let shape = out.shape().to_vec();
        if shape.len() != 3 || shape[0] != 1 {
            bail!("adr-ocr: kształt wyjścia {shape:?}, oczekiwano (1,T,C)");
        }
        let logits = out
            .view()
            .as_slice::<f32>()
            .map_err(|e| anyhow!("adr-ocr: wyjście nie jest f32: {e}"))?
            .to_vec();
        Ok((logits, shape[1], shape[2]))
    }

    /// Batched forward na tract: brak dynamic-batch w tej ścieżce (fixed [1,…]),
    /// więc pętla po pojedynczych forwardach. Ścieżka nieprodukcyjna (CPU).
    #[cfg(not(feature = "inference-supertonic"))]
    fn forward_batch(&self, data: Vec<f32>, n: usize) -> Result<Vec<(Vec<f32>, usize, usize)>> {
        let per = (IMG_H * IMG_W) as usize;
        (0..n)
            .map(|i| self.forward_single(data[i * per..(i + 1) * per].to_vec()))
            .collect()
    }

    /// Chunked wrapper over `forward_batch`: a row batch larger than
    /// [`ADR_ROWS_MAX_BATCH`] is split into profile-sized forwards, so a
    /// cross-placard batch never falls outside the pinned TRT shape range
    /// (an out-of-range input triggers a slow engine rebuild, not an error).
    fn forward_batch_chunked(
        &self,
        data: Vec<f32>,
        n: usize,
    ) -> Result<Vec<(Vec<f32>, usize, usize)>> {
        if n <= ADR_ROWS_MAX_BATCH {
            return self.forward_batch(data, n);
        }
        let per = (IMG_H * IMG_W) as usize;
        let mut out = Vec::with_capacity(n);
        let mut start = 0usize;
        while start < n {
            let end = (start + ADR_ROWS_MAX_BATCH).min(n);
            out.extend(self.forward_batch(data[start * per..end * per].to_vec(), end - start)?);
            start = end;
        }
        Ok(out)
    }

    /// CTC greedy decode zgodny z `read_batch`: per krok softmax → argmax; znak
    /// `alphabet[v-1]` gdy `v≠0` i `v≠prev`; pewność = średnia softmax-max po
    /// wybranych (niepustych, nie-powtórzonych) krokach.
    fn ctc_greedy_decode(&self, logits: &[f32], t_steps: usize, classes: usize) -> (String, f32) {
        if classes == 0 || logits.len() < t_steps * classes {
            return (String::new(), 0.0);
        }
        let mut text = String::new();
        let mut conf_sum = 0.0f32;
        let mut conf_n = 0usize;
        let mut prev = usize::MAX;
        for t in 0..t_steps {
            let row = &logits[t * classes..(t + 1) * classes];
            let (best, best_p) = softmax_argmax(row);
            if best != 0 && best != prev {
                if let Some(&ch) = self.alphabet.get(best - 1) {
                    text.push(ch);
                    conf_sum += best_p;
                    conf_n += 1;
                }
            }
            prev = best;
        }
        let conf = if conf_n > 0 {
            conf_sum / conf_n as f32
        } else {
            0.0
        };
        (text, conf)
    }

    /// Odczyt tablicy ADR z cropu RGB24 (`cw`×`ch`). Grayscale → wyszukiwanie
    /// orientacji 0/90/180/270 (tablice VID bywają obrócone): dla każdej obrót,
    /// podział na wiersze i odczyt góra(kemler)+dół(UN); score = `conf_kemler +
    /// conf_un`, +0.15 gdy `len(kemler)∈{2,3}`, +0.25 gdy `len(un)==4`. Zwraca
    /// `(kemler, un)` z najlepszej orientacji albo `None`, gdy nic sensownego
    /// (oba wiersze puste) — wtedy `local_cv` schodzi na fallback PP-OCRv5.
    /// The single-crop path IS a one-element batch — zero logic drift from
    /// [`Self::read_adr_batch`].
    pub fn read_adr(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Option<(String, String)> {
        self.read_adr_batch_refs(&[(crop_rgb, cw, ch)])
            .pop()
            .flatten()
            .map(|(kemler, un, _conf)| (kemler, un))
    }

    /// Cross-placard batched read: the same per-placard math as
    /// [`Self::read_adr`], but the rows of ALL crops (2×orientations per crop)
    /// are packed into ONE `[total_rows,1,32,128]` tensor and computed by one
    /// pooled forward (chunked at [`ADR_ROWS_MAX_BATCH`] to stay inside the TRT
    /// profile). Result `i` corresponds to crop `i` and is identical to a
    /// per-crop `read_adr` call.
    /// Result `i` is `Some((kemler, un, confidence))` where `confidence` is the
    /// mean CTC softmax-max (0..1) of the winning orientation's two rows — the
    /// same scale as the plate OCR — so the temporal vote can gate a placard on
    /// read confidence, not just count. `None` when nothing sensible was read.
    pub fn read_adr_batch(
        &self,
        crops: &[(Arc<[u8]>, u32, u32)],
    ) -> Vec<Option<(String, String, f32)>> {
        let refs: Vec<(&[u8], u32, u32)> =
            crops.iter().map(|(c, w, h)| (c.as_ref(), *w, *h)).collect();
        self.read_adr_batch_refs(&refs)
    }

    /// Shared core of the single and batched paths: prep every crop's rows
    /// (`prep_rows`), pack them into one batch, forward, CTC decode, then score
    /// orientations + dump per crop. The slot map carries GLOBAL batch
    /// positions, so logits route back to the exact (placard, orientation, row).
    fn read_adr_batch_refs(
        &self,
        crops: &[(&[u8], u32, u32)],
    ) -> Vec<Option<(String, String, f32)>> {
        if crops.is_empty() {
            return Vec::new();
        }
        // Orientation count from `adr_orientations()`: default 1 (upright only)
        // — stationary cameras see the placard upright, so the 90/180/270
        // rotations are usually wasted forwards. `[vision] adr_orientations = 4`
        // enables the full search for feeds where VID placards come rotated.
        let orientations = adr_orientations();
        let per = (IMG_H * IMG_W) as usize;
        let mut batch: Vec<f32> = Vec::with_capacity(crops.len() * 2 * orientations * per);
        let mut next = 0usize;
        // Per crop: orientation slot map → batch positions (None = invalid crop
        // or no rows at all — exactly the cases where the single-crop path used
        // to bail out before the forward).
        let crop_slots: Vec<Option<Vec<(Option<usize>, Option<usize>)>>> = crops
            .iter()
            .map(|&(rgb, cw, ch)| prep_rows(rgb, cw, ch, orientations, &mut batch, &mut next))
            .collect();
        if next == 0 {
            return vec![None; crops.len()];
        }
        let decoded: Vec<(String, f32)> = match self.forward_batch_chunked(batch, next) {
            Ok(items) => items
                .iter()
                .map(|(logits, t, c)| self.ctc_greedy_decode(logits, *t, *c))
                .collect(),
            Err(e) => {
                warn!("[adr-ocr] batch forward: {e}");
                return vec![None; crops.len()];
            }
        };

        crops
            .iter()
            .zip(crop_slots)
            .map(|(&(rgb, cw, ch), slots)| {
                let slots = slots?;
                // `conf` = mean CTC softmax-max over the rows that produced text
                // (0..1); `score` = that plus the orientation-selection length
                // bonuses (only ranks orientations, never leaves the crop).
                let mut best: Option<(f32, String, String, f32)> = None;
                for (ti, bi) in slots {
                    let (kemler, kc) = ti.map(|i| decoded[i].clone()).unwrap_or_default();
                    let (un, uc) = bi.map(|i| decoded[i].clone()).unwrap_or_default();
                    let mut score = kc + uc;
                    if (2..=3).contains(&kemler.len()) {
                        score += 0.15;
                    }
                    if un.len() == 4 {
                        score += 0.25;
                    }
                    // Mean confidence over the rows that actually decoded text —
                    // an empty row contributes no confidence and no divisor.
                    let (mut cs, mut cn) = (0.0f32, 0u32);
                    if !kemler.is_empty() {
                        cs += kc;
                        cn += 1;
                    }
                    if !un.is_empty() {
                        cs += uc;
                        cn += 1;
                    }
                    let conf = if cn > 0 { cs / cn as f32 } else { 0.0 };
                    if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                        best = Some((score, kemler, un, conf));
                    }
                }

                // `prep_rows` returned Some, so the buffer holds `expected` bytes.
                let expected = (cw as usize) * (ch as usize) * 3;
                let (label, score) = match &best {
                    Some((s, k, u, _)) => (format!("{k}_{u}"), *s),
                    None => ("none".to_string(), 0.0),
                };
                dump_adr(&rgb[..expected], cw, ch, &label, score);

                match best {
                    Some((_, kemler, un, conf)) if !kemler.is_empty() || !un.is_empty() => {
                        Some((kemler, un, conf))
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

/// Preps ALL rows of ONE placard for the shared batch: grayscale →
/// `orientations` rotation variants (k=0 original; each further k applies
/// np.rot90 once more, CCW) → `split_rows` → `preprocess_row`. Row data is
/// appended to `batch` and `next` (the global position counter) grows by every
/// appended row. Returns the orientation slot map with GLOBAL positions (a
/// `None` slot = row empty/too small) or `None` when the crop is invalid or
/// yielded no rows at all.
fn prep_rows(
    crop_rgb: &[u8],
    cw: u32,
    ch: u32,
    orientations: usize,
    batch: &mut Vec<f32>,
    next: &mut usize,
) -> Option<Vec<(Option<usize>, Option<usize>)>> {
    if cw == 0 || ch == 0 {
        return None;
    }
    let expected = (cw as usize) * (ch as usize) * 3;
    if crop_rgb.len() < expected {
        return None;
    }
    let gray = rgb_to_gray(&crop_rgb[..expected], cw, ch);

    let mut slots: Vec<(Option<usize>, Option<usize>)> = Vec::with_capacity(orientations);
    let mut produced = false;
    let mut rot = gray;
    let (mut rw, mut rh) = (cw, ch);
    for k in 0..orientations {
        if k > 0 {
            let (r, nw, nh) = rot90_ccw(&rot, rw, rh);
            rot = r;
            rw = nw;
            rh = nh;
        }
        let (top, top_h, bot, bot_h) = split_rows(&rot, rw, rh);
        let mut push = |data: Option<Vec<f32>>| {
            data.map(|d| {
                batch.extend_from_slice(&d);
                let idx = *next;
                *next += 1;
                produced = true;
                idx
            })
        };
        let ti = push(preprocess_row(top, rw, top_h));
        let bi = push(preprocess_row(bot, rw, bot_h));
        slots.push((ti, bi));
    }
    produced.then_some(slots)
}

/// Config-gated ADR crop dump (`[vision] ocr_dump_dir`): writes the raw RGB crop
/// plus its full grayscale preview named with the read result, so a human can
/// see whether the ADR placard is clipped/skewed/too small. No-op when the dump
/// dir is unset, and compiled out entirely without `inference-vision-gpu` (the
/// deskew/dump module is gated on that feature). ADR perspective rectification is
/// intentionally NOT applied yet: the placard is near-square with two rows and an
/// orientation search, so it needs its own quad tuning on real captures.
#[cfg(feature = "inference-vision-gpu")]
fn dump_adr(rgb: &[u8], w: u32, h: u32, label: &str, score: f32) {
    if crate::vision::ocr_prep::dump_dir().is_none() {
        return;
    }
    let gray = rgb_to_gray(rgb, w, h);
    crate::vision::ocr_prep::dump_ocr_sample(
        "adr",
        rgb,
        w,
        h,
        None,
        &gray,
        w,
        h,
        Some(label),
        score,
    );
}
#[cfg(not(feature = "inference-vision-gpu"))]
fn dump_adr(_rgb: &[u8], _w: u32, _h: u32, _label: &str, _score: f32) {}

/// Preprocessing jednego wiersza grayscale do wektora `[32*128]` f32: opcjonalny
/// content-trim (`adr_row_trim`) do samych cyfr, potem resize do 32×128 +
/// normalizacja `(p/255-0.5)/0.5`. `None`, gdy wiersz pusty/za mały.
fn preprocess_row(gray: &[u8], w: u32, h: u32) -> Option<Vec<f32>> {
    if w == 0 || h == 0 || gray.len() < (w * h) as usize {
        return None;
    }
    // Content-trim is fallback-protected: `trim_row_content` returns `None`
    // whenever it can't confidently tighten the row, so we resize the untouched
    // slice and are never worse than before.
    //
    // When it finds a digit bbox it keeps the digit geometry (position + scale)
    // exactly as the training full-row stretch expects and only replaces the
    // frame / row-divider / background margin OUTSIDE the bbox with the orange
    // field luma, then applies the SAME full-row stretch to 32×128. This is what
    // measurably helps on real crops: the mis-decoded margin (extra trailing
    // digits, e.g. "3257" -> "32577"/"302577") is erased while the bottom UN row
    // reads far more often. Rescaling/centering the digits instead (fill-width or
    // training-density) either adds a leading digit ("99" -> "399") or regresses
    // the 4-digit row, so we deliberately do NOT do it.
    let cleaned = adr_row_trim()
        .then(|| trim_row_content(gray, w, h))
        .flatten();
    let src: &[u8] = cleaned.as_deref().unwrap_or(&gray[..(w * h) as usize]);
    let resized = resize_gray(src, w, h, IMG_W, IMG_H).ok()?;
    Some(
        resized
            .iter()
            .map(|&p| (p as f32 / 255.0 - 0.5) / 0.5)
            .collect(),
    )
}

/// Otsu threshold of a grayscale buffer: the luma below which a pixel is "dark"
/// (digit ink / frame). Classic between-class-variance maximization over the
/// 256-bin histogram. Returns a mid value when the row is flat/degenerate.
fn otsu_threshold(gray: &[u8]) -> u8 {
    let mut hist = [0usize; 256];
    for &g in gray {
        hist[g as usize] += 1;
    }
    let total = gray.len();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let mut w_back = 0usize;
    let mut sum_back = 0.0f64;
    let mut best_var = -1.0f64;
    let mut best_t = 128u8;
    for t in 0..256usize {
        w_back += hist[t];
        if w_back == 0 {
            continue;
        }
        let w_fore = total - w_back;
        if w_fore == 0 {
            break;
        }
        sum_back += t as f64 * hist[t] as f64;
        let mean_back = sum_back / w_back as f64;
        let mean_fore = (sum_all - sum_back) / w_fore as f64;
        let between = w_back as f64 * w_fore as f64 * (mean_back - mean_fore).powi(2);
        if between > best_var {
            best_var = between;
            best_t = t as u8;
        }
    }
    best_t
}

/// Finds the content span `[lo, hi]` (inclusive) along one axis from its dark
/// profile (`profile[i]` = dark pixels in line `i`, `perp` = perpendicular
/// dimension). Solid full-extent runs sitting in the OUTER band of the axis are
/// dropped as placard frame / divider bars; the remaining ink runs bound the
/// digits. `None` when no ink is found at all.
fn find_content_span(profile: &[usize], perp: usize) -> Option<(usize, usize)> {
    let len = profile.len();
    if len == 0 || perp == 0 {
        return None;
    }
    let content_min = (((perp as f32) * ADR_TRIM_CONTENT_FRAC).ceil() as usize).max(1);
    let solid_min = (perp as f32) * ADR_TRIM_SOLID_FRAC;
    let outer = (len as f32) * ADR_TRIM_OUTER_FRAC;

    // Contiguous runs of ink lines, with each run's mean darkness.
    let mut runs: Vec<(usize, usize, f64)> = Vec::new();
    let mut i = 0usize;
    while i < len {
        if profile[i] >= content_min {
            let start = i;
            let mut sum = 0usize;
            while i < len && profile[i] >= content_min {
                sum += profile[i];
                i += 1;
            }
            let end = i - 1;
            let mean = sum as f64 / (end - start + 1) as f64;
            runs.push((start, end, mean));
        } else {
            i += 1;
        }
    }
    if runs.is_empty() {
        return None;
    }

    // Keep runs that are NOT solid outer-band bars (frame / divider).
    let kept: Vec<&(usize, usize, f64)> = runs
        .iter()
        .filter(|(s, e, mean)| {
            let center = (*s + *e) as f32 / 2.0;
            let is_outer = center < outer || center > (len as f32 - outer);
            !(*mean >= solid_min as f64 && is_outer)
        })
        .collect();
    // If every run looked like a bar, fall back to all runs (guards downstream
    // will keep the axis unchanged rather than produce a bogus crop).
    let use_runs: Vec<&(usize, usize, f64)> = if kept.is_empty() {
        runs.iter().collect()
    } else {
        kept
    };
    let lo = use_runs.iter().map(|r| r.0).min()?;
    let hi = use_runs.iter().map(|r| r.1).max()?;
    Some((lo, hi))
}

/// Cleans a grayscale row for the fixed resize: keeps the digit geometry intact
/// and replaces everything OUTSIDE the padded digit bbox (placard frame, row
/// divider, surrounding background) with the bright orange field luma. Returns
/// the cleaned `w*h` buffer, or `None` when it can't confidently locate a digit
/// bbox smaller than the whole row (nothing found, already tight, or an
/// implausibly small span) — the caller then keeps the original row unchanged.
fn trim_row_content(gray: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let (wu, hu) = (w as usize, h as usize);
    if wu == 0 || hu == 0 || gray.len() < wu * hu {
        return None;
    }
    let thresh = otsu_threshold(&gray[..wu * hu]);

    // Column and row dark-pixel profiles over the Otsu mask, plus the bright
    // (non-ink) mean = the orange field luma used to repaint the margin.
    let mut col_dark = vec![0usize; wu];
    let mut row_dark = vec![0usize; hu];
    let (mut bright_sum, mut bright_cnt) = (0u64, 0u64);
    for y in 0..hu {
        let base = y * wu;
        for x in 0..wu {
            let p = gray[base + x];
            if p < thresh {
                col_dark[x] += 1;
                row_dark[y] += 1;
            } else {
                bright_sum += p as u64;
                bright_cnt += 1;
            }
        }
    }
    let bg = if bright_cnt > 0 {
        (bright_sum / bright_cnt) as u8
    } else {
        200
    };

    // Horizontal (main gain) and vertical spans, each independently guarded so a
    // missing/weak axis simply stays full — never a bogus crop.
    let (mut x0, mut x1) = find_content_span(&col_dark, hu).unwrap_or((0, wu - 1));
    let (mut y0, mut y1) = find_content_span(&row_dark, wu).unwrap_or((0, hu - 1));

    // Pad around the digits using the digit-band height as the natural scale.
    let band_h = y1.saturating_sub(y0) + 1;
    let pad_x = ((band_h as f32) * ADR_TRIM_PAD_FRAC).round() as usize;
    let pad_y = ((band_h as f32) * (ADR_TRIM_PAD_FRAC * 0.5)).round() as usize;
    x0 = x0.saturating_sub(pad_x);
    x1 = (x1 + pad_x).min(wu - 1);
    y0 = y0.saturating_sub(pad_y);
    y1 = (y1 + pad_y).min(hu - 1);

    // Per-axis guards: revert an axis to full when the bbox is too small (stray
    // pixel) or already near-full (already tight, e.g. the bottom row).
    let mut cw = x1 - x0 + 1;
    let mut chh = y1 - y0 + 1;
    if cw < (((wu as f32) * ADR_TRIM_MIN_W_FRAC) as usize)
        || cw >= (((wu as f32) * ADR_TRIM_KEEP_W_FRAC) as usize)
    {
        x0 = 0;
        x1 = wu - 1;
        cw = wu;
    }
    if chh < (((hu as f32) * ADR_TRIM_MIN_H_FRAC) as usize)
        || chh >= (((hu as f32) * ADR_TRIM_KEEP_H_FRAC) as usize)
    {
        y0 = 0;
        y1 = hu - 1;
        chh = hu;
    }
    // Pure no-op — let the caller resize the original slice without a copy.
    if cw == wu && chh == hu {
        return None;
    }

    // Repaint the margin with the orange field luma, keeping the digit bbox as-is.
    let mut out = vec![bg; wu * hu];
    for y in y0..=y1 {
        let base = y * wu;
        out[base + x0..base + x1 + 1].copy_from_slice(&gray[base + x0..base + x1 + 1]);
    }
    Some(out)
}

/// Softmax-argmax jednego kroku (C klas): zwraca `(indeks_najlepszej, jej
/// prawdopodobieństwo)`. Softmax liczony numerycznie stabilnie (odejmujemy max).
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
    // best ma logit == max, więc exp(max-max)=1; prawdopodobieństwo = 1/sum.
    let prob = if sum > 0.0 { 1.0 / sum } else { 0.0 };
    (best, prob)
}

/// Grayscale luma BT.601 z cropu RGB24 (`0.299R + 0.587G + 0.114B`), jak
/// `ocr_plate::preprocess`. Zwraca `w*h` bajtów.
fn rgb_to_gray(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut gray = Vec::with_capacity((w * h) as usize);
    for px in rgb.chunks_exact(3) {
        let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
        gray.push(luma.round().clamp(0.0, 255.0) as u8);
    }
    gray
}

/// Bilinearny resize bufora grayscale (`sw`×`sh` → `dw`×`dh`) przez sprawdzony
/// `resize::resize_rgb`: rozwijamy każdy piksel do RGB (3× ten sam bajt),
/// skalujemy, po czym bierzemy kanał 0. Trzymamy jeden resizer w całym vision.
fn resize_gray(gray: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity(gray.len() * 3);
    for &g in gray {
        rgb.extend_from_slice(&[g, g, g]);
    }
    let resized =
        super::resize::resize_rgb(&rgb, sw, sh, dw, dh).map_err(|e| anyhow!("resize_rgb: {e}"))?;
    Ok(resized.iter().step_by(3).copied().collect())
}

/// Obrót grayscale o 90° przeciwnie do wskazówek zegara (jak `np.rot90`, k=1).
/// Dla wejścia `w`×`h` zwraca `(bufor, h, w)` — `out[i][j] = src[j][w-1-i]`.
fn rot90_ccw(src: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (wu, hu) = (w as usize, h as usize);
    let (new_w, new_h) = (hu, wu);
    let mut out = vec![0u8; new_w * new_h];
    for i in 0..new_h {
        for j in 0..new_w {
            out[i * new_w + j] = src[j * wu + (wu - 1 - i)];
        }
    }
    (out, new_w as u32, new_h as u32)
}

/// Podział grayscale na górny/dolny wiersz z 6% przerwą wokół linii środkowej
/// (`split_rows`, margin 0.06). Wiersze są ciągłymi zakresami bufora, więc
/// zwracamy pod-slice'y bez kopii razem z ich wysokościami. Szerokość obu = `w`.
fn split_rows(gray: &[u8], w: u32, h: u32) -> (&[u8], u32, &[u8], u32) {
    let wu = w as usize;
    let hu = h as usize;
    let mid = hu / 2;
    let gap = (h as f32 * SPLIT_MARGIN) as usize;
    let top_end = mid.saturating_sub(gap).max(1).min(hu);
    let bot_start = (mid + gap).min(hu.saturating_sub(1));
    let top = &gray[0..top_end * wu];
    let bot = &gray[bot_start * wu..hu * wu];
    (top, top_end as u32, bot, (hu - bot_start) as u32)
}

/// Leniwy singleton silnika: ładowany raz przy pierwszym udanym `from_dir`.
/// `Mutex<Option<...>>` (a nie zapamiętany `None`), żeby po deployu, który dopiero
/// dostarcza pliki modelu, kolejne wywołanie mogło je załadować bez restartu.
static ENGINE: OnceLock<Mutex<Option<Arc<AdrOcr>>>> = OnceLock::new();

/// Zwraca clone Arc-a do naszego silnika ADR OCR, ładując go leniwie z
/// `vision_models_dir()`. `None`, gdy modelu jeszcze nie ma — caller schodzi
/// wtedy na fallback PP-OCRv5.
pub fn get() -> Option<Arc<AdrOcr>> {
    let slot = ENGINE.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock();
    if let Some(engine) = guard.as_ref() {
        return Some(engine.clone());
    }
    let dir = crate::paths::vision_models_dir();
    match AdrOcr::from_dir(&dir) {
        Ok(engine) => {
            let arc = Arc::new(engine);
            *guard = Some(arc.clone());
            Some(arc)
        }
        Err(e) => {
            tracing::debug!("[adr-ocr] niedostępny (fallback do PP-OCRv5): {e:#}");
            None
        }
    }
}
