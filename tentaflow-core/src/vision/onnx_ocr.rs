// =============================================================================
// Plik: vision/onnx_ocr.rs
// Opis: OcrRunner oparty o PaddleOCR PP-OCRv5 (klasyczny pipeline det -> cls ->
//       rec). Backend inferencji wybierany cfg/feature — TAK SAMO jak
//       `ocr_plate`/`classifier_stan`/`adr_ocr`:
//         * `vision-ort` (ONNX Runtime, crate `ort`) → pula sesji ort
//           (TensorRT→CUDA→CPU) osobno per model (det/rec/cls). OCR biegnie na
//           GPU; forward NIE serializuje się na jednowątkowym egzekutorze CPU. To
//           ścieżka produkcyjna — WSZYSTKIE modele vision idą wtedy na GPU.
//         * inaczej → `tract-onnx` (pure Rust, CPU) na tych samych plikach ONNX.
//       Cross-platform na nie-Apple (Linux/Windows); na macOS/iOS OCR pokrywa
//       apple-ocr (Vision). Modele i slownik znakow sa pobierane deploy-time do
//       `vision_models_dir()` (mechanizm `ensure_onnx_ocr_bundle`).
//
//       Pipeline (referencja: PaddleOCR `tools/infer/predict_system.py`),
//       IDENTYCZNY dla obu backendów — zmienia się tylko warstwa inferencji:
//         1. Detekcja DB: obraz -> mapa prawdopodobienstwa tekstu -> kontury ->
//            obroty/wycinki linii tekstu (quad boxes).
//         2. Klasyfikacja kata (opcjonalna): wycinek 0/180 stopni -> ewentualny
//            obrot o 180 stopni.
//         3. Rozpoznanie CRNN/SVTR: wycinek -> sekwencja logitow -> CTC greedy
//            decode po slowniku znakow.
//       Wynik `OcrRunner::read` to konkatenacja rozpoznanych linii (sortowane
//       top->bottom, left->right) albo None gdy nic nie znaleziono.
//
//       Sesje trzymane sa leniwie pod `Mutex<Option<...>>` w `Arc<OnnxOcrEngine>`,
//       wiec jeden runner jest dzielony przez VisionDispatcher i camera-enrich
//       bez re-loadu modeli miedzy requestami.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use image::RgbImage;
use parking_lot::Mutex;
use tracing::info;

#[cfg(not(feature = "vision-ort"))]
use tract_onnx::prelude::*;

use super::resize::resize_rgb_image;
use super::OcrRunner;

#[cfg(not(feature = "vision-ort"))]
type Runnable = RunnableModel<TypedFact, Box<dyn TypedOp>>;

/// Backend-specific per-model inference handle. Under `vision-ort` it is
/// an ort `SessionPool` (TensorRT→CUDA→CPU, GPU); otherwise a tract `Runnable`
/// (pure Rust, CPU). The whole orchestration (`detect`/`maybe_rotate`/`recognize`)
/// is backend-agnostic — only the `*_forward` helpers differ per backend.
#[cfg(feature = "vision-ort")]
type OcrModel = crate::vision::ort_common::SessionPool;
#[cfg(not(feature = "vision-ort"))]
type OcrModel = Runnable;

/// Rozmiar puli sesji ort dla PP-OCRv5 (wspólny dla det/rec/cls) z
/// `[vision] ppocr_sessions`. Domyślnie 2 = kilka cropów OCR-uje się równolegle
/// na GPU bez nadmiernego zajęcia VRAM (każda sesja to pełna kopia modelu).
#[cfg(feature = "vision-ort")]
fn ppocr_pool_size() -> usize {
    crate::vision::ort_common::pool_size(crate::vision::settings::get().ppocr_sessions)
}

/// Laduje model ONNX PP-OCRv5 z USTALONYM rozmiarem wejscia (NCHW f32) na tract.
///
/// PP-OCRv5 (eksport z Paddle2ONNX) ma w pelni dynamiczne wejscie `[?,3,?,?]`
/// i niesie `value_info` dla KAZDEGO posredniego tensora z symbolicznym
/// wymiarem batcha (`DynamicDimension.0`). Gdy ustawimy konkretne wejscie
/// `[1,3,H,W]`, analizator HIR tracta probuje zunifikowac WYLICZONY ksztalt
/// wyjscia pierwszego konwolutu (`1,16,...`) z zapisanym w `value_info`
/// (`DynamicDimension.0,16,...`) i pada na `Conv.0 ConvHir` — symboliczny batch
/// nie unifikuje sie z `1`. Czyscimy wiec wszystkie fakty wyjsc posrednich
/// (kasujac symboliczne podpowiedzi z `value_info`) i pozwalamy tractowi
/// wywnioskowac ksztalty wylacznie z konkretnego wejscia.
#[cfg(not(feature = "vision-ort"))]
fn load_fixed_input(path: &Path, h: u32, w: u32) -> Result<Arc<Runnable>> {
    let mut model = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("tract: PP-OCRv5 ONNX z {}", path.display()))?;

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
        InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, h as i32, w as i32)),
    )?;
    Ok(model.into_optimized()?.into_runnable()?)
}

/// Nazwy plikow bundla PP-OCRv5 w `vision_models_dir()`. `cls` jest opcjonalny —
/// bez niego pomijamy korekte kata. `dict` jest wymagany dla dekodowania rec.
const DET_FILE: &str = "ppocrv5_det.onnx";
const REC_FILE: &str = "ppocrv5_rec.onnx";
const CLS_FILE: &str = "ppocrv5_cls.onnx";
const DICT_FILE: &str = "ppocrv5_dict.txt";

/// Detekcja DB: bok obrazu zaokraglany w dol do wielokrotnosci 32 i ograniczany
/// do tego limitu (standardowy `limit_side_len` PP-OCRv5 mobile). Staly rozmiar
/// wejscia pozwala zoptymalizowac graf tract raz przy ladowaniu.
const DET_LIMIT_SIDE: u32 = 960;
/// Prog binaryzacji mapy prawdopodobienstwa tekstu (PaddleOCR `det_db_thresh`).
const DET_BIN_THRESH: f32 = 0.3;
/// Minimalna srednia pewnosc piksela w pudelku, zeby uznac region za tekst
/// (PaddleOCR `det_db_box_thresh`).
const DET_BOX_THRESH: f32 = 0.6;
/// Rozszerzenie pudelka (PaddleOCR `det_db_unclip_ratio`) aproksymowane przez
/// stale powiekszenie bbox o ten ulamek wysokosci/szerokosci z kazdej strony.
const DET_EXPAND_RATIO: f32 = 0.5;
/// Najmniejszy bok regionu (w pikselach oryginalu), ponizej ktorego pudelko jest
/// odrzucane jako szum.
const DET_MIN_SIDE: f32 = 3.0;

/// Wysokosc wejscia rozpoznania (PP-OCRv5 mobile rec: 3x48xW).
const REC_INPUT_H: u32 = 48;
/// Maksymalna szerokosc wejscia rec po przeskalowaniu do wysokosci 48; szersze
/// wycinki sa skalowane do tej szerokosci, wezsze dopelniane zerami.
const REC_INPUT_W: u32 = 320;
/// Minimalna srednia pewnosc CTC, zeby zachowac rozpoznana linie.
const REC_MIN_CONF: f32 = 0.5;

/// Wejscie klasyfikatora kata (PP-OCRv5 cls: 3x48x192).
const CLS_INPUT_H: u32 = 48;
const CLS_INPUT_W: u32 = 192;
/// Prog pewnosci, powyzej ktorego klasa "180" wymusza obrot wycinka.
const CLS_THRESH: f32 = 0.9;

/// Wykryta linia tekstu w pikselach oryginalu (osiowo-rownolegle bbox po
/// post-processingu DB). Quad jest aproksymowany prostokatem — PP-OCRv5 mobile
/// na typowych dokumentach/tablicach ma niewielki skos, a rec i tak skaluje
/// wycinek do staloprzecinkowego pasa.
#[derive(Debug, Clone, Copy)]
struct TextBox {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

struct DetSession {
    /// Sesja detekcji: pula ort (GPU) albo tract `Runnable` (CPU) — patrz [`OcrModel`].
    model: Arc<OcrModel>,
    /// Statyczny rozmiar wejscia wyznaczony z `DET_LIMIT_SIDE`.
    input_side: u32,
}

pub struct OnnxOcrEngine {
    det_path: PathBuf,
    rec_path: PathBuf,
    cls_path: Option<PathBuf>,
    dict: Vec<String>,
    // Sesje budowane leniwie przy pierwszym `read`, potem wspoldzielone przez
    // `Arc`. `Mutex<Option<...>>` zamiast OnceCell — get_or_try_init nie jest
    // stabilne na std::OnceLock, a budowa sesji moze sie nie powiesc (I/O).
    det: Mutex<Option<Arc<DetSession>>>,
    rec: Mutex<Option<Arc<OcrModel>>>,
    cls: Mutex<Option<Option<Arc<OcrModel>>>>,
}

impl OnnxOcrEngine {
    /// Buduje silnik na podstawie plikow bundla z `vision_models_dir()`. Sesje
    /// NIE sa jeszcze tworzone — laduja sie leniwie przy pierwszym `read`.
    /// Slownik znakow jest wczytywany od razu (maly plik, walidacja
    /// kompletnosci bundla zanim deploy oznaczy usluge RUNNING).
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let det_path = dir.join(DET_FILE);
        let rec_path = dir.join(REC_FILE);
        let cls_path = dir.join(CLS_FILE);
        let dict_path = dir.join(DICT_FILE);

        if !det_path.exists() {
            return Err(anyhow!(
                "onnx-ocr: brak modelu detekcji {} (bundle PP-OCRv5 nie pobrany)",
                det_path.display()
            ));
        }
        if !rec_path.exists() {
            return Err(anyhow!(
                "onnx-ocr: brak modelu rozpoznania {} (bundle PP-OCRv5 nie pobrany)",
                rec_path.display()
            ));
        }
        let dict = load_dictionary(&dict_path)
            .with_context(|| format!("onnx-ocr: slownik {}", dict_path.display()))?;

        Ok(Self {
            det_path,
            rec_path,
            cls_path: cls_path.exists().then_some(cls_path),
            dict,
            det: Mutex::new(None),
            rec: Mutex::new(None),
            cls: Mutex::new(None),
        })
    }

    /// Katalog bundla (rodzic plików ONNX) — baza dla per-model engine-cache TRT.
    #[cfg(feature = "vision-ort")]
    fn model_dir(&self) -> &Path {
        self.det_path.parent().unwrap_or_else(|| Path::new("."))
    }

    fn det_session(&self) -> Result<Arc<DetSession>> {
        let mut slot = self.det.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let side = DET_LIMIT_SIDE;
        let model = self.build_det_model(side)?;
        let session = Arc::new(DetSession {
            model,
            input_side: side,
        });
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Ort: pula sesji na `ppocrv5_det.onnx`. Wejscie jest STALE `[1,3,side,side]`
    /// (batch 1, ustalony bok z `DET_LIMIT_SIDE`), wiec bez profilu TRT (`None`) —
    /// TensorRT buduje JEDEN engine dla tego kształtu leniwie na pierwszym forward
    /// (jeden engine per distinct shape). OCR zostaje w FP32 (`ocr_fp16()`).
    #[cfg(feature = "vision-ort")]
    fn build_det_model(&self, _side: u32) -> Result<Arc<OcrModel>> {
        crate::vision::ort_common::ensure_ort_dylib();
        let n = ppocr_pool_size();
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            &self.det_path,
            &self.model_dir().join("trt-cache-ppocr-det"),
            None,
            n,
            crate::vision::ort_common::ocr_fp16(),
        )?;
        Ok(Arc::new(pool))
    }

    #[cfg(not(feature = "vision-ort"))]
    fn build_det_model(&self, side: u32) -> Result<Arc<OcrModel>> {
        load_fixed_input(&self.det_path, side, side)
            .with_context(|| format!("tract: PP-OCRv5 det z {}", self.det_path.display()))
    }

    fn rec_session(&self) -> Result<Arc<OcrModel>> {
        let mut slot = self.rec.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let session = self.build_rec_model()?;
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Ort: pula sesji na `ppocrv5_rec.onnx`. Wejscie STALE `[1,3,48,REC_INPUT_W]`
    /// po padzie (batch 1, ustalony HxW) → bez profilu TRT (`None`). FP32.
    #[cfg(feature = "vision-ort")]
    fn build_rec_model(&self) -> Result<Arc<OcrModel>> {
        crate::vision::ort_common::ensure_ort_dylib();
        let n = ppocr_pool_size();
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            &self.rec_path,
            &self.model_dir().join("trt-cache-ppocr-rec"),
            None,
            n,
            crate::vision::ort_common::ocr_fp16(),
        )?;
        Ok(Arc::new(pool))
    }

    #[cfg(not(feature = "vision-ort"))]
    fn build_rec_model(&self) -> Result<Arc<OcrModel>> {
        load_fixed_input(&self.rec_path, REC_INPUT_H, REC_INPUT_W)
            .with_context(|| format!("tract: PP-OCRv5 rec z {}", self.rec_path.display()))
    }

    fn cls_session(&self) -> Result<Option<Arc<OcrModel>>> {
        let mut slot = self.cls.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let built = self.build_cls_model()?;
        *slot = Some(built.clone());
        Ok(built)
    }

    /// Ort: opcjonalna pula sesji na `ppocrv5_cls.onnx`. Wejscie STALE
    /// `[1,3,48,192]` (batch 1, ustalony HxW) → bez profilu TRT (`None`). FP32.
    #[cfg(feature = "vision-ort")]
    fn build_cls_model(&self) -> Result<Option<Arc<OcrModel>>> {
        let Some(path) = self.cls_path.as_ref() else {
            return Ok(None);
        };
        crate::vision::ort_common::ensure_ort_dylib();
        let n = ppocr_pool_size();
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            path,
            &self.model_dir().join("trt-cache-ppocr-cls"),
            None,
            n,
            crate::vision::ort_common::ocr_fp16(),
        )?;
        Ok(Some(Arc::new(pool)))
    }

    #[cfg(not(feature = "vision-ort"))]
    fn build_cls_model(&self) -> Result<Option<Arc<OcrModel>>> {
        match self.cls_path.as_ref() {
            None => Ok(None),
            Some(path) => {
                let model = load_fixed_input(path, CLS_INPUT_H, CLS_INPUT_W)
                    .with_context(|| format!("tract: PP-OCRv5 cls z {}", path.display()))?;
                Ok(Some(model))
            }
        }
    }

    /// Pelny pipeline na obrazku RGB: det -> (cls) -> rec -> linie tekstu
    /// posortowane top->bottom, left->right. Baza dla `read` (konkatenacja) i
    /// dla odczytu ADR (`read_lines`).
    fn run_pipeline(&self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<String>> {
        let expected = width as usize * height as usize * 3;
        if rgb.len() < expected {
            return Err(anyhow!(
                "onnx-ocr: bufor RGB za maly ({} < {}x{}x3={})",
                rgb.len(),
                width,
                height,
                expected
            ));
        }
        let img: RgbImage = RgbImage::from_raw(width, height, rgb[..expected].to_vec())
            .ok_or_else(|| anyhow!("onnx-ocr: budowa RgbImage z bufora"))?;

        let mut boxes = self.detect(&img)?;
        if boxes.is_empty() {
            return Ok(Vec::new());
        }
        // Sortuj linie top->bottom, a w obrebie jednej linii left->right.
        // Kluczem jest numer pasma wyliczony PRZED sortowaniem: porownywanie
        // "raz po y, raz po x" zaleznie od pary nie jest porzadkiem totalnym
        // (A<B po x, B<C po y, a C<A) i sortowanie panikuje.
        let band_height = boxes
            .iter()
            .map(|b| (b.y2 - b.y1).abs())
            .fold(0.0_f32, f32::max)
            .max(1.0);
        let band_of = |b: &TextBox| -> i64 {
            let band = b.y1 / (band_height * 0.5);
            if band.is_finite() {
                band.floor() as i64
            } else {
                i64::MAX
            }
        };
        boxes.sort_by(|a, b| {
            band_of(a)
                .cmp(&band_of(b))
                .then_with(|| a.x1.total_cmp(&b.x1))
        });

        let mut lines: Vec<String> = Vec::with_capacity(boxes.len());
        for b in &boxes {
            let crop = crop_box(&img, b);
            if crop.width() == 0 || crop.height() == 0 {
                continue;
            }
            let crop = self.maybe_rotate(crop)?;
            if let Some(text) = self.recognize(&crop)? {
                let t = text.trim().to_string();
                if !t.is_empty() {
                    lines.push(t);
                }
            }
        }

        Ok(lines)
    }

    /// Detekcja DB. Skaluje obraz do statycznego boku det, uruchamia model,
    /// binaryzuje mape prawdopodobienstwa i wyciaga prostokatne regiony tekstu.
    fn detect(&self, img: &RgbImage) -> Result<Vec<TextBox>> {
        let det = self.det_session()?;
        let side = det.input_side;
        let (ow, oh) = img.dimensions();

        // Skala zachowujaca aspekt do kwadratu `side x side` (bez paddingu —
        // dopelniamy zerami do pelnego kwadratu, mapa wyjsciowa ma ten sam
        // rozmiar co wejscie dla DB PP-OCRv5).
        let scale = (side as f32 / ow as f32).min(side as f32 / oh as f32);
        let nw = ((ow as f32 * scale).round() as u32).clamp(1, side);
        let nh = ((oh as f32 * scale).round() as u32).clamp(1, side);
        let resized =
            resize_rgb_image(img, nw, nh).map_err(|e| anyhow!("onnx-ocr: det resize: {e}"))?;

        // NCHW znormalizowane jak PaddleOCR: (x/255 - mean) / std, mean/std
        // ImageNet. Padding (poza nw x nh) zostaje zerem.
        let nchw = det_input_nchw(&resized, side);
        let prob = self.det_forward(&det, nchw, side)?;
        // DB zwraca (1,1,side,side) — mapa prawdopodobienstwa.
        if prob.len() < (side * side) as usize {
            return Err(anyhow!(
                "onnx-ocr: det output ma {} elementow, oczekiwano {}",
                prob.len(),
                side * side
            ));
        }

        let regions = extract_boxes(&prob, side, side);
        // Odwzoruj regiony z przestrzeni det (side x side, tekst w nw x nh) na
        // oryginalny obraz przez `1/scale`, przytnij do granic obrazu.
        let inv = 1.0 / scale;
        let mut out = Vec::with_capacity(regions.len());
        for r in regions {
            let bw = (r.x2 - r.x1) * inv;
            let bh = (r.y2 - r.y1) * inv;
            if bw.min(bh) < DET_MIN_SIDE {
                continue;
            }
            // Unclip: powieksz pudelko o ulamek mniejszego boku z kazdej strony.
            let ex = bw.min(bh) * DET_EXPAND_RATIO;
            let x1 = ((r.x1 * inv) - ex).clamp(0.0, ow as f32);
            let y1 = ((r.y1 * inv) - ex).clamp(0.0, oh as f32);
            let x2 = ((r.x2 * inv) + ex).clamp(0.0, ow as f32);
            let y2 = ((r.y2 * inv) + ex).clamp(0.0, oh as f32);
            if x2 > x1 && y2 > y1 {
                out.push(TextBox { x1, y1, x2, y2 });
            }
        }
        Ok(out)
    }

    /// Forward detekcji DB → płaska mapa prawdopodobienstwa (owned f32). Ort:
    /// pojedynczy `session.run` na puli (GPU); tract: `model.run` (CPU).
    #[cfg(feature = "vision-ort")]
    fn det_forward(&self, det: &DetSession, nchw: Vec<f32>, side: u32) -> Result<Vec<f32>> {
        let input = ndarray::Array4::from_shape_vec((1, 3, side as usize, side as usize), nchw)
            .map_err(|e| anyhow!("onnx-ocr: det nchw shape: {e}"))?;
        det.model.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("onnx-ocr: det Value::from_array: {e}"))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: det model has no inputs"))?;
            let output_name = session
                .outputs()
                .first()
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: det model has no outputs"))?;
            let outputs = session
                .run(ort::inputs! { input_name => value })
                .map_err(|e| anyhow!("onnx-ocr: det session.run: {e}"))?;
            // DB emituje (1,1,side,side) — bierzemy płaski slice mapy.
            let (_shape, prob) = outputs[output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("onnx-ocr: det extract prob: {e}"))?;
            Ok(prob.to_vec())
        })
    }

    #[cfg(not(feature = "vision-ort"))]
    fn det_forward(&self, det: &DetSession, nchw: Vec<f32>, side: u32) -> Result<Vec<f32>> {
        let input: Tensor =
            tract_ndarray::Array4::from_shape_vec((1, 3, side as usize, side as usize), nchw)
                .context("onnx-ocr: det nchw shape")?
                .into();
        let outputs = det
            .model
            .run(tvec!(input.into()))
            .context("onnx-ocr: det forward")?;
        let prob = outputs[0]
            .view()
            .as_slice::<f32>()
            .context("onnx-ocr: det output nie jest f32")?;
        Ok(prob.to_vec())
    }

    /// Korekta kata: jezeli klasyfikator jest dostepny i pewnie wskazuje 180
    /// stopni, obraca wycinek. Bez modelu cls zwraca wycinek bez zmian.
    fn maybe_rotate(&self, crop: RgbImage) -> Result<RgbImage> {
        let Some(logits) = self.cls_forward(&crop)? else {
            return Ok(crop);
        };
        // PP-OCRv5 cls: 2 klasy [0, 180]. Softmax i sprawdzenie progu na "180".
        if logits.len() >= 2 {
            let probs = softmax(&logits[..2]);
            if probs[1] >= CLS_THRESH {
                return Ok(image::imageops::rotate180(&crop));
            }
        }
        Ok(crop)
    }

    /// Forward klasyfikatora kata na wycinku → logity (owned f32), albo `None` gdy
    /// modelu cls nie ma w bundlu. Ort: pula sesji (GPU); tract: `model.run` (CPU).
    #[cfg(feature = "vision-ort")]
    fn cls_forward(&self, crop: &RgbImage) -> Result<Option<Vec<f32>>> {
        let Some(pool) = self.cls_session()? else {
            return Ok(None);
        };
        let resized = resize_rgb_image(crop, CLS_INPUT_W, CLS_INPUT_H)
            .map_err(|e| anyhow!("onnx-ocr: cls resize: {e}"))?;
        let nchw = rec_cls_input_nchw(&resized, CLS_INPUT_W, CLS_INPUT_H, CLS_INPUT_W);
        let input = ndarray::Array4::from_shape_vec(
            (1, 3, CLS_INPUT_H as usize, CLS_INPUT_W as usize),
            nchw,
        )
        .map_err(|e| anyhow!("onnx-ocr: cls nchw shape: {e}"))?;
        let logits = pool.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("onnx-ocr: cls Value::from_array: {e}"))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: cls model has no inputs"))?;
            let output_name = session
                .outputs()
                .first()
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: cls model has no outputs"))?;
            let outputs = session
                .run(ort::inputs! { input_name => value })
                .map_err(|e| anyhow!("onnx-ocr: cls session.run: {e}"))?;
            let (_shape, logits) = outputs[output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("onnx-ocr: cls extract logits: {e}"))?;
            Ok(logits.to_vec())
        })?;
        Ok(Some(logits))
    }

    #[cfg(not(feature = "vision-ort"))]
    fn cls_forward(&self, crop: &RgbImage) -> Result<Option<Vec<f32>>> {
        let Some(cls) = self.cls_session()? else {
            return Ok(None);
        };
        let resized = resize_rgb_image(crop, CLS_INPUT_W, CLS_INPUT_H)
            .map_err(|e| anyhow!("onnx-ocr: cls resize: {e}"))?;
        let nchw = rec_cls_input_nchw(&resized, CLS_INPUT_W, CLS_INPUT_H, CLS_INPUT_W);
        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, CLS_INPUT_H as usize, CLS_INPUT_W as usize),
            nchw,
        )
        .context("onnx-ocr: cls nchw shape")?
        .into();
        let outputs = cls
            .run(tvec!(input.into()))
            .context("onnx-ocr: cls forward")?;
        let logits = outputs[0]
            .view()
            .as_slice::<f32>()
            .context("onnx-ocr: cls output nie jest f32")?;
        Ok(Some(logits.to_vec()))
    }

    /// Rozpoznanie linii: skaluje wycinek do 48xW (max 320, dopelnienie zerami),
    /// uruchamia rec, CTC greedy decode po slowniku.
    fn recognize(&self, crop: &RgbImage) -> Result<Option<String>> {
        let (cw, ch) = crop.dimensions();
        if cw == 0 || ch == 0 {
            return Ok(None);
        }
        // Skala do wysokosci 48 zachowujaca aspekt, szerokosc ograniczona do 320.
        let ratio = cw as f32 / ch as f32;
        let target_w = ((REC_INPUT_H as f32 * ratio).round() as u32).clamp(1, REC_INPUT_W);
        let resized = resize_rgb_image(crop, target_w, REC_INPUT_H)
            .map_err(|e| anyhow!("onnx-ocr: rec resize: {e}"))?;
        let nchw = rec_cls_input_nchw(&resized, target_w, REC_INPUT_H, REC_INPUT_W);
        let (data, t_steps, classes) = self.rec_forward(nchw)?;

        Ok(ctc_greedy_decode(
            &data,
            t_steps,
            classes,
            &self.dict,
            REC_MIN_CONF,
        ))
    }

    /// Forward rozpoznania rec → `(logits, T, C)` z wyjscia (1,T,C). Ort: pula
    /// sesji (GPU); tract: `model.run` (CPU). C = liczba klas (slownik + blank na 0).
    #[cfg(feature = "vision-ort")]
    fn rec_forward(&self, nchw: Vec<f32>) -> Result<(Vec<f32>, usize, usize)> {
        let pool = self.rec_session()?;
        let input = ndarray::Array4::from_shape_vec(
            (1, 3, REC_INPUT_H as usize, REC_INPUT_W as usize),
            nchw,
        )
        .map_err(|e| anyhow!("onnx-ocr: rec nchw shape: {e}"))?;
        pool.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("onnx-ocr: rec Value::from_array: {e}"))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: rec model has no inputs"))?;
            let output_name = session
                .outputs()
                .first()
                .map(|o| o.name().to_string())
                .ok_or_else(|| anyhow!("onnx-ocr: rec model has no outputs"))?;
            let outputs = session
                .run(ort::inputs! { input_name => value })
                .map_err(|e| anyhow!("onnx-ocr: rec session.run: {e}"))?;
            let (shape, data) = outputs[output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("onnx-ocr: rec extract logits: {e}"))?;
            // PP-OCRv5 rec: (1, T, C) gdzie C = liczba klas (slownik + blank na 0).
            if shape.len() != 3 {
                return Err(anyhow!(
                    "onnx-ocr: rec output ksztalt {shape:?}, oczekiwano (1,T,C)"
                ));
            }
            Ok((data.to_vec(), shape[1] as usize, shape[2] as usize))
        })
    }

    #[cfg(not(feature = "vision-ort"))]
    fn rec_forward(&self, nchw: Vec<f32>) -> Result<(Vec<f32>, usize, usize)> {
        let rec = self.rec_session()?;
        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, REC_INPUT_H as usize, REC_INPUT_W as usize),
            nchw,
        )
        .context("onnx-ocr: rec nchw shape")?
        .into();
        let outputs = rec
            .run(tvec!(input.into()))
            .context("onnx-ocr: rec forward")?;
        // PP-OCRv5 rec: (1, T, C) gdzie C = liczba klas (slownik + blank na 0).
        let out = &outputs[0];
        let shape = out.shape().to_vec();
        if shape.len() != 3 {
            return Err(anyhow!(
                "onnx-ocr: rec output ksztalt {:?}, oczekiwano (1,T,C)",
                shape
            ));
        }
        let data = out
            .view()
            .as_slice::<f32>()
            .context("onnx-ocr: rec output nie jest f32")?;
        Ok((data.to_vec(), shape[1], shape[2]))
    }
}

impl OcrRunner for OnnxOcrEngine {
    fn read_lines(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        self.run_pipeline(crop_rgb, cw, ch)
    }

    fn read(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        let lines = self.run_pipeline(crop_rgb, cw, ch)?;
        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lines.join("\n")))
        }
    }
}

/// Wczytuje slownik znakow PP-OCRv5: jeden znak na linie. Indeks 0 jest
/// zarezerwowany dla CTC blank, wiec klasa `i` (i>=1) mapuje na `dict[i-1]`.
/// Ostatnia klasa to spacja (PaddleOCR `use_space_char`). Mapowanie indeks->znak
/// liczymy w `ctc_greedy_decode`.
fn load_dictionary(path: &Path) -> Result<Vec<String>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut chars: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    if chars.is_empty() {
        return Err(anyhow!("slownik pusty"));
    }
    // PaddleOCR dolacza znak spacji na koncu (use_space_char=true).
    chars.push(" ".to_string());
    Ok(chars)
}

/// CTC greedy decode: per krok argmax, kompresja powtorzen i usuniecie blanku
/// (indeks 0). Zwraca None gdy srednia pewnosc < `min_conf` albo tekst pusty.
fn ctc_greedy_decode(
    data: &[f32],
    t_steps: usize,
    classes: usize,
    dict: &[String],
    min_conf: f32,
) -> Option<String> {
    let mut text = String::new();
    let mut conf_sum = 0.0f32;
    let mut conf_n = 0usize;
    let mut last_idx = usize::MAX;
    for t in 0..t_steps {
        let row = &data[t * classes..(t + 1) * classes];
        let (mut best, mut best_v) = (0usize, f32::NEG_INFINITY);
        for (i, &v) in row.iter().enumerate() {
            if v > best_v {
                best_v = v;
                best = i;
            }
        }
        // Blank (0) lub powtorzenie -> pomijamy (klasyczny CTC collapse).
        if best != 0 && best != last_idx {
            // klasa i -> dict[i-1] (0 zarezerwowane dla blank).
            if let Some(ch) = dict.get(best - 1) {
                text.push_str(ch);
                conf_sum += best_v;
                conf_n += 1;
            }
        }
        last_idx = best;
    }
    if conf_n == 0 {
        return None;
    }
    let avg = conf_sum / conf_n as f32;
    if avg < min_conf || text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Numerycznie stabilny softmax na malym wektorze.
fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
    let mut sum = 0.0f32;
    let exps: Vec<f32> = logits
        .iter()
        .map(|&l| {
            let e = (l - max).exp();
            sum += e;
            e
        })
        .collect();
    exps.into_iter().map(|e| e / sum).collect()
}

/// Wycina osiowo-rownolegly region z obrazu (zacisniety do granic).
fn crop_box(img: &RgbImage, b: &TextBox) -> RgbImage {
    let (iw, ih) = img.dimensions();
    let x1 = b.x1.max(0.0) as u32;
    let y1 = b.y1.max(0.0) as u32;
    let x2 = (b.x2.ceil() as u32).min(iw);
    let y2 = (b.y2.ceil() as u32).min(ih);
    if x2 <= x1 || y2 <= y1 {
        return RgbImage::new(0, 0);
    }
    image::imageops::crop_imm(img, x1, y1, x2 - x1, y2 - y1).to_image()
}

/// Wejscie detekcji DB: NCHW f32 znormalizowane mean/std ImageNet (skala 0..1),
/// dopelnione zerami do `side x side`. `img` ma rozmiar nw x nh (nw,nh <= side).
fn det_input_nchw(img: &RgbImage, side: u32) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let plane = (side * side) as usize;
    let mut buf = vec![0f32; plane * 3];
    let (w, h) = img.dimensions();
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            let idx = (y * side + x) as usize;
            for c in 0..3 {
                let v = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
                buf[c * plane + idx] = v;
            }
        }
    }
    buf
}

/// Wejscie rec/cls: NCHW f32 znormalizowane `(x/255 - 0.5) / 0.5` (PaddleOCR
/// rec/cls), wpisane do bufora `canvas_w x h`, reszta (poza img_w) zerami.
fn rec_cls_input_nchw(img: &RgbImage, img_w: u32, h: u32, canvas_w: u32) -> Vec<f32> {
    let plane = (canvas_w * h) as usize;
    let mut buf = vec![0f32; plane * 3];
    let w = img_w.min(img.dimensions().0).min(canvas_w);
    for y in 0..h.min(img.dimensions().1) {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            let idx = (y * canvas_w + x) as usize;
            for c in 0..3 {
                let v = (p[c] as f32 / 255.0 - 0.5) / 0.5;
                buf[c * plane + idx] = v;
            }
        }
    }
    buf
}

/// Post-processing DB: binaryzacja mapy prawdopodobienstwa progiem
/// `DET_BIN_THRESH`, etykietowanie spojnych komponentow (4-sasiedztwo) i
/// wyznaczenie ich bounding-boxow z filtrem sredniej pewnosci `DET_BOX_THRESH`.
/// Zwraca prostokaty w przestrzeni mapy (`w x h`).
fn extract_boxes(prob: &[f32], w: u32, h: u32) -> Vec<TextBox> {
    let wn = w as usize;
    let hn = h as usize;
    let mut visited = vec![false; wn * hn];
    let mut boxes = Vec::new();
    let is_text = |i: usize| prob[i] >= DET_BIN_THRESH;

    let mut stack: Vec<usize> = Vec::new();
    for start in 0..wn * hn {
        if visited[start] || !is_text(start) {
            continue;
        }
        // Flood fill spojnego komponentu (4-sasiedztwo).
        stack.clear();
        stack.push(start);
        visited[start] = true;
        let (mut min_x, mut min_y) = (usize::MAX, usize::MAX);
        let (mut max_x, mut max_y) = (0usize, 0usize);
        let mut conf_sum = 0.0f32;
        let mut count = 0usize;
        while let Some(idx) = stack.pop() {
            let x = idx % wn;
            let y = idx / wn;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            conf_sum += prob[idx];
            count += 1;
            // Sasiedzi.
            if x + 1 < wn && !visited[idx + 1] && is_text(idx + 1) {
                visited[idx + 1] = true;
                stack.push(idx + 1);
            }
            if x > 0 && !visited[idx - 1] && is_text(idx - 1) {
                visited[idx - 1] = true;
                stack.push(idx - 1);
            }
            if y + 1 < hn && !visited[idx + wn] && is_text(idx + wn) {
                visited[idx + wn] = true;
                stack.push(idx + wn);
            }
            if y > 0 && !visited[idx - wn] && is_text(idx - wn) {
                visited[idx - wn] = true;
                stack.push(idx - wn);
            }
        }
        if count == 0 {
            continue;
        }
        // Filtr pewnosci pudelka (PaddleOCR det_db_box_thresh).
        if conf_sum / (count as f32) < DET_BOX_THRESH {
            continue;
        }
        boxes.push(TextBox {
            x1: min_x as f32,
            y1: min_y as f32,
            x2: (max_x + 1) as f32,
            y2: (max_y + 1) as f32,
        });
    }
    boxes
}

/// Globalna instancja silnika OCR — leniwie tworzona przy rejestracji, dzielona
/// przez `Arc` z VisionDispatcherem. `OnceLock` zeby kolejne deploye tego samego
/// serwisu nie re-budowaly sesji.
static ENGINE: OnceLock<Arc<OnnxOcrEngine>> = OnceLock::new();

/// Laduje silnik PP-OCRv5 z `vision_models_dir()` i rejestruje go jako globalny
/// in-process OCR runner przez `super::set_ocr_runner`. Wolane przez deploy
/// embedded (`onnx-ocr`) PO pobraniu bundla. Brak modeli/slownika zglasza blad
/// od razu (przed oznaczeniem uslugi RUNNING).
pub fn register_as_ocr_runner() -> Result<()> {
    let engine = match ENGINE.get() {
        Some(e) => e.clone(),
        None => {
            let dir = crate::paths::vision_models_dir();
            let built = Arc::new(OnnxOcrEngine::from_dir(&dir)?);
            // Pierwszy zwyciezca ustawia globalny silnik; ewentualny rownolegly
            // deploy reuzywa juz ustawionego.
            let _ = ENGINE.set(built);
            ENGINE.get().expect("ENGINE just set").clone()
        }
    };
    super::set_ocr_runner(engine);
    #[cfg(feature = "vision-ort")]
    let backend = "ort TensorRT→CUDA→CPU";
    #[cfg(not(feature = "vision-ort"))]
    let backend = "tract CPU";
    info!("[onnx-ocr] zarejestrowany jako in-process OCR runner (PP-OCRv5, {backend})");
    Ok(())
}

/// Wyrejestrowuje silnik (rollback / stop service).
pub fn unregister_as_ocr_runner() {
    super::clear_ocr_runner();
}
