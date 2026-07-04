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
use tracing::{info, warn};

use crate::paths;
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_plate::Model;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Odczyt 2-liniowej tablicy ADR (pomarańczowa plansza, np. „<kemler>/<UN>"): górny
    /// rząd to numer rozpoznawczy zagrożenia (kemler, 2-3 cyfry), dolny to numer
    /// UN (3-4 cyfry). Model `PlateOcr` (trenowany na białych, ~7-znakowych
    /// tablicach) nie radzi sobie z tym formatem, więc ADR czytamy DEDYKOWANYM
    /// torem opartym o systemowy Tesseract (`-l pol`, whitelist cyfr) — patrz
    /// [`read_adr_tesseract`]. Metoda jest cienkim opakowaniem, aby wołający
    /// (`vision_analysis::enrich`) mógł nadal używać `guard.read_adr(...)`.
    ///
    /// Wszystkie błędy (brak Tesseracta, błąd zapisu PNG, niezerowy status)
    /// zamieniamy na `None` — OCR ADR jest wzbogaceniem opcjonalnym i nie może
    /// wywrócić pętli analizy klatek.
    pub fn read_adr(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        match read_adr_tesseract(crop_rgb, cw, ch) {
            Ok(wynik) => Ok(wynik),
            Err(e) => {
                warn!("[ocr_plate] odczyt ADR (tesseract) nie powiódł się: {e:#}");
                Ok(None)
            }
        }
    }

    /// Wspólny rdzeń OCR: preprocessing (upscale + resize + grayscale) → forward →
    /// argmax per slot → surowy string modelu (BEZ walidacji formatu). Używany
    /// przez [`Self::read`] (z walidacją PL) oraz [`Self::read_adr`] (walidacja
    /// per linia). Zwraca `None`, gdy model odczytał same znaki wypełnienia.
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
        let shape = (1usize, self.img_h as usize, self.img_w as usize, 1usize);
        let input = ndarray::Array4::from_shape_vec(shape, gray.to_vec())
            .map_err(|e| anyhow!("ocr_plate: build tensor {shape:?}: {e}"))?;
        let value = ort::value::Value::from_array(input)
            .map_err(|e| anyhow!("ocr_plate: Value::from_array: {e}"))?;

        let mut session = self.pool.checkout()?;
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
        let expected = self.slots * self.vocab;
        if shape.len() != 2 || shape[0] != 1 || shape[1] as usize != expected {
            bail!(
                "ocr_plate: output shape {shape:?} != [1, {expected}] (slots*vocab)"
            );
        }
        if logits.len() != expected {
            bail!(
                "ocr_plate: logits len {} != slots*vocab {expected}",
                logits.len()
            );
        }
        let logits = logits.to_vec();
        drop(outputs);
        drop(session);
        Ok(logits)
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

/// Zostawia w napisie wyłącznie cyfry ASCII — Tesseract z whitelistą czasem
/// przepuszcza białe znaki/nowe linie, a numery kemler/UN są czysto cyfrowe.
fn filtruj_cyfry(tekst: &str) -> String {
    tekst.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Dedykowany czytnik tablicy ADR (2-liniowa pomarańczowa plansza). Recepta
/// (zwalidowana eksperymentalnie): grayscale → binaryzacja Otsu → podział na
/// górną (kemler) i dolną (UN) połowę → każda połowa jako PNG → systemowy
/// Tesseract (`-l pol --psm 8`, whitelist cyfr) → parsowanie cyfr → dociągnięcie
/// wyniku do najbliższej znanej pary ADR ([`snap_adr`]). Zwraca
/// `"<kemler>/<un> <opis>"` tylko gdy odczyt daje się dopasować do listy,
/// inaczej `None`.
fn read_adr_tesseract(crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
    if cw < 8 || ch < 16 {
        return Ok(None);
    }
    let w = cw as usize;
    let h = ch as usize;
    if crop_rgb.len() < w * h * 3 {
        return Ok(None);
    }

    // RGB24 → grayscale (luma BT.601).
    let mut gray = Vec::with_capacity(w * h);
    for px in crop_rgb.chunks_exact(3).take(w * h) {
        let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
        gray.push(luma.round().clamp(0.0, 255.0) as u8);
    }

    // Binaryzacja progiem Otsu. Cyfry na planszy ADR są ciemne na jasnym
    // (pomarańczowym) tle, więc piksele powyżej progu = tło (białe = 255),
    // poniżej/równe = cyfry (czarne = 0).
    let prog = prog_otsu(&gray);
    let bin: Vec<u8> = gray
        .iter()
        .map(|&v| if u32::from(v) > prog { 255 } else { 0 })
        .collect();

    // Podział poziomy na dwie połowy z marginesem ~6% wokół linii działowej,
    // aby pozioma listwa środkowa planszy nie wchodziła w żaden z rzędów cyfr.
    // Snap odbywa się po numerze UN (dolny wiersz — patrz [`snap_adr`]), więc
    // OCR-ujemy wyłącznie dolną połowę; kemler pochodzi z trafionego wpisu listy.
    let mid = h / 2;
    let margin = ((h as f32) * 0.06).round() as usize;
    let bot_y = (mid + margin).min(h);
    if bot_y >= h {
        return Ok(None);
    }

    let dolny = ocr_polowa(&bin, w, bot_y, h)?;

    Ok(snap_adr(&dolny))
}

/// Klasyczny próg Otsu na 256-koszykowym histogramie: maksymalizuje wariancję
/// międzyklasową (tło vs. pierwszy plan). Zwraca próg w zakresie 0..255.
fn prog_otsu(gray: &[u8]) -> u32 {
    let mut hist = [0u64; 256];
    for &v in gray {
        hist[v as usize] += 1;
    }
    let total = gray.len() as f64;
    if total == 0.0 {
        return 0;
    }
    // Suma ważona wszystkich intensywności (do średniej pierwszego planu).
    let suma_calkowita: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();

    let mut waga_tla = 0.0f64;
    let mut suma_tla = 0.0f64;
    let mut max_var = -1.0f64;
    let mut prog = 0u32;
    for t in 0..256usize {
        waga_tla += hist[t] as f64;
        if waga_tla == 0.0 {
            continue;
        }
        let waga_planu = total - waga_tla;
        if waga_planu == 0.0 {
            break;
        }
        suma_tla += t as f64 * hist[t] as f64;
        let srednia_tla = suma_tla / waga_tla;
        let srednia_planu = (suma_calkowita - suma_tla) / waga_planu;
        let var = waga_tla * waga_planu * (srednia_tla - srednia_planu).powi(2);
        if var > max_var {
            max_var = var;
            prog = t as u32;
        }
    }
    prog
}

/// OCR pojedynczej połowy planszy: wycina wiersze `[y0, y1)` z bufora binarnego,
/// otacza je ~20px białego marginesu (Tesseract preferuje przestrzeń wokół
/// tekstu), zapisuje jako PNG do katalogu tymczasowego i uruchamia Tesseract.
/// Plik PNG jest usuwany po zakończeniu ([`TempFile`]). Zwraca same cyfry.
fn ocr_polowa(bin: &[u8], w: usize, y0: usize, y1: usize) -> Result<String> {
    const PAD: usize = 20;
    let rh = y1 - y0;
    let ow = w + 2 * PAD;
    let oh = rh + 2 * PAD;

    // Białe płótno + wklejenie wierszy połowy z marginesem.
    let mut buf = vec![255u8; ow * oh];
    for y in 0..rh {
        let src = &bin[(y0 + y) * w..(y0 + y + 1) * w];
        let dst = (y + PAD) * ow + PAD;
        buf[dst..dst + w].copy_from_slice(src);
    }

    let img = image::GrayImage::from_raw(ow as u32, oh as u32, buf)
        .ok_or_else(|| anyhow!("nie udało się zbudować obrazu PNG połowy ADR"))?;
    let sciezka = temp_png_path();
    img.save(&sciezka)
        .with_context(|| format!("zapis PNG {}", sciezka.display()))?;
    // Sprzątanie pliku niezależnie od dalszego wyniku.
    let _guard = TempFile(sciezka.clone());

    let stdout = uruchom_tesseract(&sciezka)?;
    Ok(filtruj_cyfry(&stdout))
}

/// Uruchamia systemowy Tesseract na pliku PNG i zwraca jego `stdout`. W systemie
/// brakuje langpacka `eng`, ale jest `pol` — stąd wymuszone `-l pol`. `--psm 8`
/// traktuje obraz jak pojedyncze słowo, a whitelist ogranicza wynik do cyfr.
fn uruchom_tesseract(png: &Path) -> Result<String> {
    let out = Command::new("tesseract")
        .arg(png)
        .arg("stdout")
        .args([
            "-l",
            "pol",
            "--psm",
            "8",
            "-c",
            "tessedit_char_whitelist=0123456789",
        ])
        .output()
        .with_context(|| "uruchomienie tesseract nie powiodło się (czy jest zainstalowany?)")?;
    if !out.status.success() {
        bail!(
            "tesseract zakończył się błędem: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Buduje unikatową ścieżkę pliku PNG w katalogu tymczasowym systemu. Unikatowość
/// zapewnia PID procesu + monotoniczny licznik (bezpieczny między wątkami).
fn temp_png_path() -> PathBuf {
    static LICZNIK: AtomicU64 = AtomicU64::new(0);
    let n = LICZNIK.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("tentaflow_adr_{pid}_{n}.png"))
}

/// Strażnik czasu życia pliku tymczasowego — usuwa go przy wyjściu z zakresu,
/// także w razie wcześniejszego błędu (`?`) w [`ocr_polowa`].
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Kształt `adr-list.json` — lista dozwolonych pozycji ADR.
#[derive(Debug, Deserialize)]
struct ListaAdr {
    pary: Vec<ParaAdr>,
}

/// Pojedyncza pozycja ADR: górny wiersz planszy (kemler), dolny wiersz (numer
/// UN) oraz opis ładunku (do prezentacji we froncie).
#[derive(Debug, Deserialize)]
struct ParaAdr {
    kemler: String,
    un: String,
    opis: String,
}

/// Maksymalna odległość edycyjna dolnego wiersza (numeru UN) do wpisu z listy,
/// dopuszczana przy snapie. Powyżej niej odczyt uznajemy za zbyt niepewny i
/// zwracamy `None` (bez zgadywania). UN to 4 cyfry, które Tesseract czyta
/// najpewniej, dlatego próg jest ciasny.
const MAX_ODLEGLOSC_UN: usize = 1;

/// Wczytuje listę dozwolonych pozycji ADR z `<vision_models_dir>/adr-list.json`
/// (katalog `.runtime/`, gitignorowany). W kodzie źródłowym NIE ma żadnej listy
/// wbudowanej — gdy pliku brak, jest pusty lub niepoprawny, zwracamy pustą listę,
/// a [`snap_adr`] nie zwróci wtedy żadnego dopasowania (ADR nie jest pokazywany).
/// Każdy wpis niesie kemler, numer UN oraz opis ładunku (do prezentacji).
fn wczytaj_liste_adr() -> Vec<(String, String, String)> {
    let sciezka = paths::vision_models_dir().join("adr-list.json");
    match std::fs::read(&sciezka) {
        Ok(bytes) => match serde_json::from_slice::<ListaAdr>(&bytes) {
            Ok(lista) => lista
                .pary
                .into_iter()
                .map(|p| (p.kemler, p.un, p.opis))
                .collect(),
            Err(e) => {
                warn!(
                    "[ocr_plate] {} istnieje, ale nie udało się go sparsować ({e}) — lista ADR pusta",
                    sciezka.display()
                );
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Dociąga surowy odczyt Tesseracta do najbliższej znanej pozycji ADR. Ponieważ
/// numery UN są w liście unikalne i rozłączne, a Tesseract czyta dolny wiersz
/// (4 cyfry) najpewniej, dopasowujemy GŁÓWNIE po UN: wybieramy wpis o minimalnej
/// odległości Levenshteina UN do odczytu dolnego wiersza. Górny wiersz (kemler)
/// bywa mylony (np. „301" zamiast „30"), więc kemler bierzemy z TRAFIONEGO wpisu,
/// nie z OCR. Gdy najlepsze dopasowanie przekracza [`MAX_ODLEGLOSC_UN`] — zwraca
/// `None`. Zwraca `"<kemler>/<un> <opis>"` z listy (opis po separatorze-spacji,
/// żeby front mógł go odczepić bez żadnych danych wbudowanych po swojej stronie).
/// Gdy lista pusta (brak pliku `adr-list.json`) — zawsze `None`.
fn snap_adr(dolny: &str) -> Option<String> {
    if dolny.is_empty() {
        return None;
    }
    let lista = wczytaj_liste_adr();
    if lista.is_empty() {
        return None;
    }
    let mut najlepsza: Option<(usize, &(String, String, String))> = None;
    for para in &lista {
        let dist = levenshtein(dolny, &para.1);
        let lepsza = match najlepsza {
            Some((d, _)) => dist < d,
            None => true,
        };
        if lepsza {
            najlepsza = Some((dist, para));
        }
    }
    match najlepsza {
        Some((dist, (kemler, un, opis))) if dist <= MAX_ODLEGLOSC_UN => {
            if opis.is_empty() {
                Some(format!("{kemler}/{un}"))
            } else {
                Some(format!("{kemler}/{un} {opis}"))
            }
        }
        _ => None,
    }
}

/// Odległość edycyjna Levenshteina (wstawienia/usunięcia/podmiany) między dwoma
/// napisami — na bajtach ASCII, bo numery ADR to wyłącznie cyfry.
fn levenshtein(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Jednowierszowa tablica DP (poprzedni wiersz odległości).
    let mut poprzedni: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut lewo_gora = poprzedni[0];
        poprzedni[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let koszt = usize::from(ca != cb);
            let nowa = (poprzedni[j + 1] + 1)
                .min(poprzedni[j] + 1)
                .min(lewo_gora + koszt);
            lewo_gora = poprzedni[j + 1];
            poprzedni[j + 1] = nowa;
        }
    }
    poprzedni[b.len()]
}
