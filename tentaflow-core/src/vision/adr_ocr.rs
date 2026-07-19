// =============================================================================
// Plik: vision/adr_ocr.rs
// Opis: Nasz wytrenowany czytnik numerów ADR (mały CRNN, ~4 MB) uruchamiany
//       in-process przez tract-onnx (pure Rust, jak reszta vision). Czyta
//       pomarańczową planszę Kemler/UN: górny wiersz (kemler) i dolny (numer UN),
//       z wyszukiwaniem orientacji (tablice VID bywają obrócone o ~90°).
//       Algorytm 1:1 z `scripts/train-adr-ocr/eval.py`:
//         * preprocessing: grayscale → resize 32×128 (H×W) → (pix/255-0.5)/0.5,
//         * model: wejście [1,1,32,128], wyjście logity [1,T,C], C=11 (10 cyfr +
//           blank na indeksie 0), alfabet `0123456789`,
//         * CTC greedy decode (blank=0, kompresja powtórzeń), pewność = średnia
//           softmax-max po wybranych krokach,
//         * split górny/dolny wiersz z 6% przerwą wokół linii środkowej,
//         * orientation-search 0/90/180/270 z heurystyką długości.
//       Numer UN dociągamy do katalogu przez `adr::snap_adr`. Ten silnik jest
//       GŁÓWNYM czytnikiem ADR; PP-OCRv5 (`onnx_ocr`) jest fallbackiem, gdy nasz
//       model jest niedostępny lub nic nie odczyta (patrz `local_cv::ocr_adr_local`).
// =============================================================================

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use tract_onnx::prelude::*;
use tracing::warn;

type Runnable = RunnableModel<TypedFact, Box<dyn TypedOp>>;

/// Wysokość i szerokość wejścia modelu (H×W), zgodnie z `Reader._prep`.
const IMG_H: u32 = 32;
const IMG_W: u32 = 128;
/// Przerwa wokół linii środkowej przy podziale na wiersze (`split_rows`).
const SPLIT_MARGIN: f32 = 0.06;

/// Nazwy plików bundla w `vision_models_dir()`.
const MODEL_FILE: &str = "adr_ocr.onnx";
const ALPHABET_FILE: &str = "adr_ocr_alphabet.txt";

/// Ładuje model CRNN z USTALONYM wejściem `[1,1,32,128]` (NCHW f32). Model niesie
/// dynamiczne kształty (Shape/Gather/Reshape wokół LSTM), więc — jak w
/// `onnx_ocr::load_fixed_input` — czyścimy fakty wszystkich wyjść pośrednich i
/// pozwalamy tractowi wywnioskować kształty wyłącznie z konkretnego wejścia.
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
        InferenceFact::dt_shape(
            f32::datum_type(),
            tvec!(1, 1, IMG_H as i32, IMG_W as i32),
        ),
    )?;
    Ok(model.into_optimized()?.into_runnable()?)
}

pub struct AdrOcr {
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

        let model = load_fixed_input(&model_path)?;
        Ok(Self { model, alphabet })
    }

    /// Czyta pojedynczy wiersz grayscale (`w`×`h`, jeden bajt/piksel). Preprocessing
    /// (resize 32×128 + normalizacja) → forward → CTC greedy decode. Zwraca
    /// `(tekst, pewność)`; przy błędzie forwardu zwraca `("", 0.0)` z ostrzeżeniem,
    /// żeby nie wywracać wyszukiwania orientacji ani pętli klatek.
    fn read_row(&self, gray: &[u8], w: u32, h: u32) -> (String, f32) {
        if w == 0 || h == 0 || gray.len() < (w * h) as usize {
            return (String::new(), 0.0);
        }
        let resized = match resize_gray(gray, w, h, IMG_W, IMG_H) {
            Ok(r) => r,
            Err(e) => {
                warn!("[adr-ocr] resize wiersza: {e}");
                return (String::new(), 0.0);
            }
        };
        // Normalizacja (pix/255 - 0.5)/0.5, tensor [1,1,H,W] f32.
        let data: Vec<f32> = resized
            .iter()
            .map(|&p| (p as f32 / 255.0 - 0.5) / 0.5)
            .collect();
        let input: Tensor = match tract_ndarray::Array4::from_shape_vec(
            (1, 1, IMG_H as usize, IMG_W as usize),
            data,
        ) {
            Ok(a) => a.into(),
            Err(e) => {
                warn!("[adr-ocr] budowa tensora: {e}");
                return (String::new(), 0.0);
            }
        };

        let outputs = match self.model.run(tvec!(input.into())) {
            Ok(o) => o,
            Err(e) => {
                warn!("[adr-ocr] forward: {e}");
                return (String::new(), 0.0);
            }
        };
        // Wyjście [1,T,C]; C=11 (10 cyfr + blank na 0).
        let out = &outputs[0];
        let shape = out.shape().to_vec();
        if shape.len() != 3 {
            warn!("[adr-ocr] kształt wyjścia {:?}, oczekiwano (1,T,C)", shape);
            return (String::new(), 0.0);
        }
        let (t_steps, classes) = (shape[1], shape[2]);
        let logits = match out.view().as_slice::<f32>() {
            Ok(s) => s,
            Err(e) => {
                warn!("[adr-ocr] wyjście nie jest f32: {e}");
                return (String::new(), 0.0);
            }
        };
        self.ctc_greedy_decode(logits, t_steps, classes)
    }

    /// CTC greedy decode zgodny z `read_batch`: per krok softmax → argmax; znak
    /// `alphabet[v-1]` gdy `v≠0` i `v≠prev`; pewność = średnia softmax-max po
    /// wybranych (niepustych, nie-powtórzonych) krokach.
    fn ctc_greedy_decode(&self, logits: &[f32], t_steps: usize, classes: usize) -> (String, f32) {
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
    pub fn read_adr(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Option<(String, String)> {
        if cw == 0 || ch == 0 {
            return None;
        }
        let expected = (cw as usize) * (ch as usize) * 3;
        if crop_rgb.len() < expected {
            return None;
        }
        let gray = rgb_to_gray(&crop_rgb[..expected], cw, ch);

        let mut best: Option<(f32, String, String)> = None;
        // k=0 to oryginał; kolejne to np.rot90 zaaplikowane k razy (CCW).
        let mut rot = gray;
        let (mut rw, mut rh) = (cw, ch);
        for k in 0..4 {
            if k > 0 {
                let (r, nw, nh) = rot90_ccw(&rot, rw, rh);
                rot = r;
                rw = nw;
                rh = nh;
            }
            let (top, top_h, bot, bot_h) = split_rows(&rot, rw, rh);
            let (kemler, kc) = self.read_row(top, rw, top_h);
            let (un, uc) = self.read_row(bot, rw, bot_h);

            let mut score = kc + uc;
            if (2..=3).contains(&kemler.len()) {
                score += 0.15;
            }
            if un.len() == 4 {
                score += 0.25;
            }
            if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                best = Some((score, kemler, un));
            }
        }

        match best {
            Some((_, kemler, un)) if !kemler.is_empty() || !un.is_empty() => Some((kemler, un)),
            _ => None,
        }
    }
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
    let resized = super::resize::resize_rgb(&rgb, sw, sh, dw, dh)
        .map_err(|e| anyhow!("resize_rgb: {e}"))?;
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
