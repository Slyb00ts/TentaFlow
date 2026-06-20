// =============================================================================
// Plik: vision/onnx_ocr.rs
// Opis: OcrRunner oparty o PaddleOCR PP-OCRv5 (klasyczny pipeline det -> cls ->
//       rec) uruchamiany in-process przez tract-onnx (pure Rust ONNX, ta sama
//       infrastruktura co reszta silnikow vision — bez ABI hell i bez `ort`).
//       Cross-platform na nie-Apple (Linux/Windows); na macOS/iOS OCR pokrywa
//       apple-ocr (Vision). Modele i slownik znakow sa pobierane deploy-time do
//       `vision_models_dir()` (mechanizm `ensure_onnx_ocr_bundle`).
//
//       Pipeline (referencja: PaddleOCR `tools/infer/predict_system.py`):
//         1. Detekcja DB: obraz -> mapa prawdopodobienstwa tekstu -> kontury ->
//            obroty/wycinki linii tekstu (quad boxes).
//         2. Klasyfikacja kata (opcjonalna): wycinek 0/180 stopni -> ewentualny
//            obrot o 180 stopni.
//         3. Rozpoznanie CRNN/SVTR: wycinek -> sekwencja logitow -> CTC greedy
//            decode po slowniku znakow.
//       Wynik `OcrRunner::read` to konkatenacja rozpoznanych linii (sortowane
//       top->bottom, left->right) albo None gdy nic nie znaleziono.
//
//       Sesje tract trzymane sa leniwie pod `OnceCell` w `Arc<OnnxOcrEngine>`,
//       wiec jeden runner jest dzielony przez VisionDispatcher i camera-enrich
//       bez re-loadu modeli miedzy requestami.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use image::RgbImage;
use parking_lot::Mutex;
use tract_onnx::prelude::*;
use tracing::info;

use super::resize::resize_rgb_image;
use super::OcrRunner;

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

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
    model: Runnable,
    /// Statyczny rozmiar wejscia wyznaczony z `DET_LIMIT_SIDE`.
    input_side: u32,
}

pub struct OnnxOcrEngine {
    det_path: PathBuf,
    rec_path: PathBuf,
    cls_path: Option<PathBuf>,
    dict: Vec<String>,
    // Sesje tract budowane leniwie przy pierwszym `read`, potem wspoldzielone
    // przez `Arc`. `Mutex<Option<...>>` zamiast OnceCell — get_or_try_init nie
    // jest stabilne na std::OnceLock, a budowa sesji moze sie nie powiesc (I/O).
    det: Mutex<Option<Arc<DetSession>>>,
    rec: Mutex<Option<Arc<Runnable>>>,
    cls: Mutex<Option<Arc<Option<Runnable>>>>,
}

impl OnnxOcrEngine {
    /// Buduje silnik na podstawie plikow bundla z `vision_models_dir()`. Sesje
    /// tract NIE sa jeszcze tworzone — laduja sie leniwie przy pierwszym
    /// `read`. Slownik znakow jest wczytywany od razu (maly plik, walidacja
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

    fn det_session(&self) -> Result<Arc<DetSession>> {
        let mut slot = self.det.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let side = DET_LIMIT_SIDE;
        let model = tract_onnx::onnx()
            .model_for_path(&self.det_path)
            .with_context(|| format!("tract: PP-OCRv5 det z {}", self.det_path.display()))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, side as i32, side as i32)),
            )?
            .into_optimized()?
            .into_runnable()?;
        let session = Arc::new(DetSession {
            model,
            input_side: side,
        });
        *slot = Some(session.clone());
        Ok(session)
    }

    fn rec_session(&self) -> Result<Arc<Runnable>> {
        let mut slot = self.rec.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let model = tract_onnx::onnx()
            .model_for_path(&self.rec_path)
            .with_context(|| format!("tract: PP-OCRv5 rec z {}", self.rec_path.display()))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec!(1, 3, REC_INPUT_H as i32, REC_INPUT_W as i32),
                ),
            )?
            .into_optimized()?
            .into_runnable()?;
        let session = Arc::new(model);
        *slot = Some(session.clone());
        Ok(session)
    }

    fn cls_session(&self) -> Result<Arc<Option<Runnable>>> {
        let mut slot = self.cls.lock();
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let built = match self.cls_path.as_ref() {
            None => None,
            Some(path) => {
                let model = tract_onnx::onnx()
                    .model_for_path(path)
                    .with_context(|| format!("tract: PP-OCRv5 cls z {}", path.display()))?
                    .with_input_fact(
                        0,
                        InferenceFact::dt_shape(
                            f32::datum_type(),
                            tvec!(1, 3, CLS_INPUT_H as i32, CLS_INPUT_W as i32),
                        ),
                    )?
                    .into_optimized()?
                    .into_runnable()?;
                Some(model)
            }
        };
        let session = Arc::new(built);
        *slot = Some(session.clone());
        Ok(session)
    }

    /// Pelny pipeline na obrazku RGB: det -> (cls) -> rec -> konkatenacja linii.
    fn run_pipeline(&self, rgb: &[u8], width: u32, height: u32) -> Result<Option<String>> {
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
            return Ok(None);
        }
        // Sortuj linie top->bottom, a w obrebie podobnej wysokosci left->right.
        boxes.sort_by(|a, b| {
            let dy = a.y1 - b.y1;
            if dy.abs() > (a.y2 - a.y1).max(b.y2 - b.y1) * 0.5 {
                a.y1.partial_cmp(&b.y1).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.x1.partial_cmp(&b.x1).unwrap_or(std::cmp::Ordering::Equal)
            }
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

        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lines.join("\n")))
        }
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
        let resized = resize_rgb_image(img, nw, nh)
            .map_err(|e| anyhow!("onnx-ocr: det resize: {e}"))?;

        // NCHW znormalizowane jak PaddleOCR: (x/255 - mean) / std, mean/std
        // ImageNet. Padding (poza nw x nh) zostaje zerem.
        let nchw = det_input_nchw(&resized, side);
        let input: Tensor =
            tract_ndarray::Array4::from_shape_vec((1, 3, side as usize, side as usize), nchw)
                .context("onnx-ocr: det nchw shape")?
                .into();

        let outputs = det
            .model
            .run(tvec!(input.into()))
            .context("onnx-ocr: det forward")?;
        let prob = outputs[0]
            .as_slice::<f32>()
            .context("onnx-ocr: det output nie jest f32")?;
        // DB zwraca (1,1,side,side) — mapa prawdopodobienstwa.
        if prob.len() < (side * side) as usize {
            return Err(anyhow!(
                "onnx-ocr: det output ma {} elementow, oczekiwano {}",
                prob.len(),
                side * side
            ));
        }

        let regions = extract_boxes(prob, side, side);
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

    /// Korekta kata: jezeli klasyfikator jest dostepny i pewnie wskazuje 180
    /// stopni, obraca wycinek. Bez modelu cls zwraca wycinek bez zmian.
    fn maybe_rotate(&self, crop: RgbImage) -> Result<RgbImage> {
        let cls_arc = self.cls_session()?;
        let Some(cls) = cls_arc.as_ref() else {
            return Ok(crop);
        };
        let resized = resize_rgb_image(&crop, CLS_INPUT_W, CLS_INPUT_H)
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
            .as_slice::<f32>()
            .context("onnx-ocr: cls output nie jest f32")?;
        // PP-OCRv5 cls: 2 klasy [0, 180]. Softmax i sprawdzenie progu na "180".
        if logits.len() >= 2 {
            let probs = softmax(&logits[..2]);
            if probs[1] >= CLS_THRESH {
                return Ok(image::imageops::rotate180(&crop));
            }
        }
        Ok(crop)
    }

    /// Rozpoznanie linii: skaluje wycinek do 48xW (max 320, dopelnienie zerami),
    /// uruchamia rec, CTC greedy decode po slowniku.
    fn recognize(&self, crop: &RgbImage) -> Result<Option<String>> {
        let rec = self.rec_session()?;
        let (cw, ch) = crop.dimensions();
        if cw == 0 || ch == 0 {
            return Ok(None);
        }
        // Skala do wysokosci 48 zachowujaca aspekt, szerokosc ograniczona do 320.
        let ratio = cw as f32 / ch as f32;
        let target_w = ((REC_INPUT_H as f32 * ratio).round() as u32)
            .clamp(1, REC_INPUT_W);
        let resized = resize_rgb_image(crop, target_w, REC_INPUT_H)
            .map_err(|e| anyhow!("onnx-ocr: rec resize: {e}"))?;
        let nchw = rec_cls_input_nchw(&resized, target_w, REC_INPUT_H, REC_INPUT_W);
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
        let t_steps = shape[1];
        let classes = shape[2];
        let data = out
            .as_slice::<f32>()
            .context("onnx-ocr: rec output nie jest f32")?;

        Ok(ctc_greedy_decode(
            data,
            t_steps,
            classes,
            &self.dict,
            REC_MIN_CONF,
        ))
    }
}

impl OcrRunner for OnnxOcrEngine {
    fn read(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        self.run_pipeline(crop_rgb, cw, ch)
    }
}

/// Wczytuje slownik znakow PP-OCRv5: jeden znak na linie. Indeks 0 jest
/// zarezerwowany dla CTC blank, wiec klasa `i` (i>=1) mapuje na `dict[i-1]`.
/// Ostatnia klasa to spacja (PaddleOCR `use_space_char`). Mapowanie indeks->znak
/// liczymy w `ctc_greedy_decode`.
fn load_dictionary(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
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
/// serwisu nie re-budowaly sesji tract.
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
            ENGINE
                .get()
                .expect("ENGINE just set")
                .clone()
        }
    };
    super::set_ocr_runner(engine);
    info!("[onnx-ocr] zarejestrowany jako in-process OCR runner (PP-OCRv5, tract)");
    Ok(())
}

/// Wyrejestrowuje silnik (rollback / stop service).
pub fn unregister_as_ocr_runner() {
    super::clear_ocr_runner();
}
