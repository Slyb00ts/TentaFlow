// =============================================================================
// Plik: tts/supertonic.rs
// Opis: Embedded TtsEngine dla Supertonic (Supertone/supertonic-3) przez ONNX
//       Runtime (`ort`, load-dynamic). Pipeline flow-matching: tokenizer ->
//       duration_predictor -> text_encoder -> sample_noisy_latent ->
//       vector_estimator (petla `total_step`) -> vocoder. Multilingual (31
//       jezykow + `na`), wybor glosu przez voice_styles/*.json. Desktop:
//       Linux/macOS/Windows. iOS jest osobna faza (tu nieobslugiwany).
// =============================================================================

#![cfg(feature = "inference-supertonic")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use ndarray::{Array, Array3};
use ort::session::Session;
use ort::value::Value;
use rand_distr::{Distribution, Normal};
use regex::Regex;
use serde::Deserialize;
use tracing::info;
use unicode_normalization::UnicodeNormalization;

use super::{SynthesizeParams, SynthesizeResult, TtsEngine, TtsModelInfo};

/// Lista jezykow wspieranych przez Supertonic. Tekst jest owijany tagiem
/// `<lang>...</lang>`; lang spoza listy jest odrzucany (model nie ma embeddingu).
const AVAILABLE_LANGS: &[&str] = &[
    "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr", "hu",
    "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk", "vi", "na",
];

/// Domyslny jezyk gdy request nie poda `language` (multilingual model wymaga
/// taga). Spojny z `DEFAULT_VOICE` ponizej.
const DEFAULT_LANG: &str = "en";

/// Domyslny voice preset uzywany gdy ani request, ani voice_hint nie wskaze
/// glosu i `load_model` nie znajdzie konkretnego pliku.
const DEFAULT_VOICE: &str = "M1";

/// Liczba krokow solvera flow-matching (vector_estimator). Wyzej = lepsza
/// jakosc, wolniej; 10 daje wyrazniejsza mowe niz referencyjne 8.
const TOTAL_STEP: usize = 10;

/// Domyslny mnoznik tempa (duration /= speed). Wyzszy = szybsza mowa.
const DEFAULT_SPEED: f32 = 1.2;

/// Cisza wstawiana miedzy chunkami (sekundy) przy laczeniu dlugiego tekstu.
const SILENCE_DURATION: f32 = 0.3;

/// Maksymalna dlugosc chunku w znakach. ko/ja maja gestszy zapis -> krotszy
/// limit (120), reszta 300.
const MAX_CHUNK_LEN_DEFAULT: usize = 300;
const MAX_CHUNK_LEN_CJK: usize = 120;

const ABBREVIATIONS: &[&str] = &[
    "Dr.", "Mr.", "Mrs.", "Ms.", "Prof.", "Sr.", "Jr.", "St.", "Ave.", "Rd.", "Blvd.", "Dept.",
    "Inc.", "Ltd.", "Co.", "Corp.", "etc.", "vs.", "i.e.", "e.g.", "Ph.D.",
];

fn is_valid_lang(lang: &str) -> bool {
    AVAILABLE_LANGS.contains(&lang)
}

// =============================================================================
// Konfiguracja (tts.json) i voice style (voice_styles/*.json)
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
struct Config {
    ae: AeConfig,
    ttl: TtlConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct AeConfig {
    sample_rate: i32,
    base_chunk_size: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct TtlConfig {
    chunk_compress_factor: i32,
    latent_dim: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct VoiceStyleData {
    style_ttl: StyleComponent,
    style_dp: StyleComponent,
}

#[derive(Debug, Clone, Deserialize)]
struct StyleComponent {
    data: Vec<Vec<Vec<f32>>>,
    dims: Vec<usize>,
}

/// Voice style spłaszczony do dwoch tensorow gotowych jako wejscia ONNX.
struct Style {
    ttl: Array3<f32>,
    dp: Array3<f32>,
}

/// Wczytuje pojedynczy voice_style JSON i spłaszcza `data` (zagniezdzone
/// `Vec<Vec<Vec<f32>>>`) do `Array3` o ksztalcie z `dims`. `bsz=1` — embedded
/// engine syntezuje pojedynczy tekst na raz.
fn load_voice_style(path: &Path) -> Result<Style> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let data: VoiceStyleData =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;

    let ttl = flatten_style(&data.style_ttl)
        .with_context(|| format!("style_ttl w {}", path.display()))?;
    let dp =
        flatten_style(&data.style_dp).with_context(|| format!("style_dp w {}", path.display()))?;
    Ok(Style { ttl, dp })
}

fn flatten_style(c: &StyleComponent) -> Result<Array3<f32>> {
    if c.dims.len() != 3 {
        bail!("oczekiwano 3 wymiarow, otrzymano {:?}", c.dims);
    }
    let (d0, d1, d2) = (c.dims[0], c.dims[1], c.dims[2]);
    let mut flat = Vec::with_capacity(d0 * d1 * d2);
    for batch in &c.data {
        for row in batch {
            for &v in row {
                flat.push(v);
            }
        }
    }
    if flat.len() != d0 * d1 * d2 {
        bail!(
            "rozmiar danych {} != iloczyn dims {}x{}x{}",
            flat.len(),
            d0,
            d1,
            d2
        );
    }
    Array3::from_shape_vec((d0, d1, d2), flat).context("Array3::from_shape_vec")
}

// =============================================================================
// Tokenizer (czysty Rust)
// =============================================================================

/// Indekser unicode: plaska tablica dlugosci 65536, `indexer[codepoint]` ->
/// token_id (lub -1 gdy poza tablica / brak mapowania).
struct UnicodeProcessor {
    indexer: Vec<i64>,
}

impl UnicodeProcessor {
    fn new(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let indexer: Vec<i64> =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok(Self { indexer })
    }

    /// Preprocess + mapowanie codepoint -> token_id. Zwraca `text_ids`
    /// (`[len]`, bsz=1 owijane przez caller) oraz `text_mask` `[1,1,len]`.
    fn call(&self, text: &str, lang: &str) -> Result<(Vec<i64>, Array3<f32>)> {
        let processed = preprocess_text(text, lang)?;
        let len = processed.chars().count();

        let mut ids = vec![0i64; len];
        for (j, ch) in processed.chars().enumerate() {
            let cp = ch as usize;
            ids[j] = if cp < self.indexer.len() {
                self.indexer[cp]
            } else {
                -1
            };
        }
        let mask = length_to_mask(len, len);
        Ok((ids, mask))
    }
}

/// Normalizacja tekstu zgodna z referencyjnym pipeline'em: NFKD, usuniecie
/// emoji, normalizacja myslnikow/cudzyslowow, `@ -> " at "`, scalanie spacji,
/// dolozenie kropki gdy brak koncowej interpunkcji, owiniecie tagiem jezyka.
fn preprocess_text(text: &str, lang: &str) -> Result<String> {
    let mut text: String = text.nfkd().collect();

    let emoji_pattern = Regex::new(
        r"[\x{1F600}-\x{1F64F}\x{1F300}-\x{1F5FF}\x{1F680}-\x{1F6FF}\x{1F700}-\x{1F77F}\x{1F780}-\x{1F7FF}\x{1F800}-\x{1F8FF}\x{1F900}-\x{1F9FF}\x{1FA00}-\x{1FA6F}\x{1FA70}-\x{1FAFF}\x{2600}-\x{26FF}\x{2700}-\x{27BF}\x{1F1E6}-\x{1F1FF}]+",
    )
    .expect("staly regex emoji");
    text = emoji_pattern.replace_all(&text, "").to_string();

    let replacements = [
        ("\u{2013}", "-"),
        ("\u{2011}", "-"),
        ("\u{2014}", "-"),
        ("_", " "),
        ("\u{201C}", "\""),
        ("\u{201D}", "\""),
        ("\u{2018}", "'"),
        ("\u{2019}", "'"),
        ("\u{00B4}", "'"),
        ("`", "'"),
        ("[", " "),
        ("]", " "),
        ("|", " "),
        ("/", " "),
        ("#", " "),
        ("\u{2192}", " "),
        ("\u{2190}", " "),
    ];
    for (from, to) in &replacements {
        text = text.replace(from, to);
    }

    for symbol in &["\u{2665}", "\u{2606}", "\u{2661}", "\u{00A9}", "\\"] {
        text = text.replace(symbol, "");
    }

    for (from, to) in &[
        ("@", " at "),
        ("e.g.,", "for example, "),
        ("i.e.,", "that is, "),
    ] {
        text = text.replace(from, to);
    }

    text = Regex::new(r" ,")
        .unwrap()
        .replace_all(&text, ",")
        .to_string();
    text = Regex::new(r" \.")
        .unwrap()
        .replace_all(&text, ".")
        .to_string();
    text = Regex::new(r" !")
        .unwrap()
        .replace_all(&text, "!")
        .to_string();
    text = Regex::new(r" \?")
        .unwrap()
        .replace_all(&text, "?")
        .to_string();
    text = Regex::new(r" ;")
        .unwrap()
        .replace_all(&text, ";")
        .to_string();
    text = Regex::new(r" :")
        .unwrap()
        .replace_all(&text, ":")
        .to_string();
    text = Regex::new(r" '")
        .unwrap()
        .replace_all(&text, "'")
        .to_string();

    while text.contains("\"\"") {
        text = text.replace("\"\"", "\"");
    }
    while text.contains("''") {
        text = text.replace("''", "'");
    }
    while text.contains("``") {
        text = text.replace("``", "`");
    }

    text = Regex::new(r"\s+")
        .unwrap()
        .replace_all(&text, " ")
        .to_string();
    text = text.trim().to_string();

    if !text.is_empty() {
        let ends_with_punct =
            Regex::new(r#"[.!?;:,'"\u{201C}\u{201D}\u{2018}\u{2019})\]}…。」』】〉》›»]$"#)
                .unwrap();
        if !ends_with_punct.is_match(&text) {
            text.push('.');
        }
    }

    if !is_valid_lang(lang) {
        bail!("nieobslugiwany jezyk '{lang}' dla Supertonic (dostepne: {AVAILABLE_LANGS:?})");
    }

    Ok(format!("<{lang}>{text}</{lang}>"))
}

/// Maska dlugosci `[1,1,max_len]` — 1.0 dla pierwszych `len` pozycji.
fn length_to_mask(len: usize, max_len: usize) -> Array3<f32> {
    let mut mask = Array3::<f32>::zeros((1, 1, max_len));
    for j in 0..len.min(max_len) {
        mask[[0, 0, j]] = 1.0;
    }
    mask
}

// =============================================================================
// Chunking dlugiego tekstu
// =============================================================================

/// Dzieli tekst na chunki <= `max_len` znakow: akapity -> zdania -> przecinki
/// -> slowa. Zachowuje granice zdan (pomijajac skroty z `ABBREVIATIONS`).
fn chunk_text(text: &str, max_len: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![String::new()];
    }

    let para_re = Regex::new(r"\n\s*\n").unwrap();
    let mut chunks = Vec::new();

    for para in para_re.split(text) {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if para.len() <= max_len {
            chunks.push(para.to_string());
            continue;
        }

        let mut current = String::new();
        for sentence in split_sentences(para) {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }
            let sentence_len = sentence.len();
            if sentence_len > max_len {
                if !current.is_empty() {
                    chunks.push(std::mem::take(&mut current).trim().to_string());
                }
                split_oversized_sentence(sentence, max_len, &mut current, &mut chunks);
                continue;
            }
            if current.len() + sentence_len + 1 > max_len && !current.is_empty() {
                chunks.push(std::mem::take(&mut current).trim().to_string());
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(sentence);
        }
        if !current.is_empty() {
            chunks.push(current.trim().to_string());
        }
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

/// Zdanie dluzsze niz `max_len`: tnie po przecinkach, a zbyt dlugie czesci po
/// spacjach. Mutuje `current` (bufor laczacy krotkie czesci) i `chunks`.
fn split_oversized_sentence(
    sentence: &str,
    max_len: usize,
    current: &mut String,
    chunks: &mut Vec<String>,
) {
    for part in sentence.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.len() > max_len {
            let mut word_chunk = String::new();
            for word in part.split_whitespace() {
                if word_chunk.len() + word.len() + 1 > max_len && !word_chunk.is_empty() {
                    chunks.push(std::mem::take(&mut word_chunk).trim().to_string());
                }
                if !word_chunk.is_empty() {
                    word_chunk.push(' ');
                }
                word_chunk.push_str(word);
            }
            if !word_chunk.is_empty() {
                chunks.push(word_chunk.trim().to_string());
            }
        } else {
            if current.len() + part.len() + 2 > max_len && !current.is_empty() {
                chunks.push(std::mem::take(current).trim().to_string());
            }
            if !current.is_empty() {
                current.push_str(", ");
            }
            current.push_str(part);
        }
    }
}

/// Dzieli akapit na zdania po `[.!?]\s+`. Regex Rust nie ma lookbehind, wiec
/// granice konczace sie skrotem (np. "Dr.") sa scalane z nastepnym zdaniem.
fn split_sentences(text: &str) -> Vec<String> {
    let re = Regex::new(r"([.!?])\s+").unwrap();
    let matches: Vec<_> = re.find_iter(text).collect();
    if matches.is_empty() {
        return vec![text.to_string()];
    }

    let mut sentences = Vec::new();
    let mut last_end = 0;
    for m in matches {
        let before_punc = text[last_end..m.start()].trim();
        let punct = &text[m.start()..m.start() + 1];
        let is_abbrev = ABBREVIATIONS
            .iter()
            .any(|abbrev| format!("{before_punc}{punct}").ends_with(abbrev));
        if !is_abbrev {
            sentences.push(text[last_end..m.end()].to_string());
            last_end = m.end();
        }
    }
    if last_end < text.len() {
        sentences.push(text[last_end..].to_string());
    }
    if sentences.is_empty() {
        vec![text.to_string()]
    } else {
        sentences
    }
}

// =============================================================================
// Latent (czysty Rust)
// =============================================================================

/// Losuje `noisy_latent [1,latent_dim_val,latent_len]` ~ Normal(0,1) i maskuje.
/// `latent_dim_val = latent_dim * chunk_compress`, `chunk_size = base_chunk_size
/// * chunk_compress`, `latent_len = ceil(duration*sample_rate / chunk_size)`.
fn sample_noisy_latent(
    duration: f32,
    sample_rate: i32,
    base_chunk_size: i32,
    chunk_compress: i32,
    latent_dim: i32,
) -> (Array3<f32>, Array3<f32>) {
    let wav_len = (duration * sample_rate as f32) as usize;
    let chunk_size = (base_chunk_size * chunk_compress) as usize;
    let latent_len = wav_len.div_ceil(chunk_size);
    let latent_dim_val = (latent_dim * chunk_compress) as usize;

    let latent_mask = length_to_mask(latent_len, latent_len);

    let normal = Normal::new(0.0f32, 1.0f32).expect("Normal(0,1) zawsze poprawny");
    let mut rng = rand::rng();

    let mut noisy = Array3::<f32>::zeros((1, latent_dim_val, latent_len));
    for d in 0..latent_dim_val {
        for t in 0..latent_len {
            noisy[[0, d, t]] = normal.sample(&mut rng) * latent_mask[[0, 0, t]];
        }
    }
    (noisy, latent_mask)
}

// =============================================================================
// Sesje ONNX + stan silnika
// =============================================================================

/// Zaladowane sesje ONNX + tokenizer + config + voice style. Trzymane razem,
/// bo `Session::run` wymaga `&mut`, a `synthesize(&self,...)` — calosc jest
/// pod jednym Mutexem (synteza i tak jest sekwencyjna na jeden engine).
struct Loaded {
    cfg: Config,
    processor: UnicodeProcessor,
    dp: Session,
    text_enc: Session,
    vector_est: Session,
    vocoder: Session,
    style: Style,
    /// Aktualnie zaladowany voice preset (np. `M1`). Pozwala wykryc, czy
    /// `synthesize` zazadalo innego glosu i przeladowac tylko `style`.
    voice: String,
}

impl Loaded {
    /// Pelny pipeline dla jednego chunku tekstu. Zwraca (wav, duration_sec).
    fn infer_chunk(&mut self, text: &str, lang: &str, speed: f32) -> Result<(Vec<f32>, f32)> {
        let (text_ids, text_mask) = self.processor.call(text, lang)?;
        let len = text_ids.len();
        let text_ids_arr =
            Array::from_shape_vec((1, len), text_ids).context("Array text_ids [1,len]")?;

        let sample_rate = self.cfg.ae.sample_rate;

        // 1. duration_predictor.
        let duration = {
            let text_ids_value = Value::from_array(text_ids_arr.clone())?;
            let style_dp_value = Value::from_array(self.style.dp.clone())?;
            let text_mask_value = Value::from_array(text_mask.clone())?;
            let out = self.dp.run(ort::inputs! {
                "text_ids" => &text_ids_value,
                "style_dp" => &style_dp_value,
                "text_mask" => &text_mask_value,
            })?;
            let (_, data) = out["duration"].try_extract_tensor::<f32>()?;
            let raw = *data.first().ok_or_else(|| anyhow!("puste duration"))?;
            raw / speed
        };

        // 2. text_encoder.
        let style_ttl_value = Value::from_array(self.style.ttl.clone())?;
        let text_emb = {
            let text_ids_value = Value::from_array(text_ids_arr.clone())?;
            let text_mask_value = Value::from_array(text_mask.clone())?;
            let out = self.text_enc.run(ort::inputs! {
                "text_ids" => &text_ids_value,
                "style_ttl" => &style_ttl_value,
                "text_mask" => &text_mask_value,
            })?;
            let (shape, data) = out["text_emb"].try_extract_tensor::<f32>()?;
            Array3::from_shape_vec(
                (shape[0] as usize, shape[1] as usize, shape[2] as usize),
                data.to_vec(),
            )
            .context("Array3 text_emb")?
        };

        // 3. sample_noisy_latent.
        let (mut xt, latent_mask) = sample_noisy_latent(
            duration,
            sample_rate,
            self.cfg.ae.base_chunk_size,
            self.cfg.ttl.chunk_compress_factor,
            self.cfg.ttl.latent_dim,
        );

        // 4. vector_estimator — petla flow-matching. Solver jest w grafie:
        //    kazdy krok przyjmuje xt + indeks kroku i zwraca nowy xt.
        let total_step_arr = Array::from_elem(1, TOTAL_STEP as f32);
        for step in 0..TOTAL_STEP {
            let current_step_arr = Array::from_elem(1, step as f32);
            let xt_value = Value::from_array(xt.clone())?;
            let text_emb_value = Value::from_array(text_emb.clone())?;
            let latent_mask_value = Value::from_array(latent_mask.clone())?;
            let text_mask_value = Value::from_array(text_mask.clone())?;
            let current_step_value = Value::from_array(current_step_arr)?;
            let total_step_value = Value::from_array(total_step_arr.clone())?;

            let out = self.vector_est.run(ort::inputs! {
                "noisy_latent" => &xt_value,
                "text_emb" => &text_emb_value,
                "style_ttl" => &style_ttl_value,
                "latent_mask" => &latent_mask_value,
                "text_mask" => &text_mask_value,
                "current_step" => &current_step_value,
                "total_step" => &total_step_value,
            })?;
            let (shape, data) = out["denoised_latent"].try_extract_tensor::<f32>()?;
            xt = Array3::from_shape_vec(
                (shape[0] as usize, shape[1] as usize, shape[2] as usize),
                data.to_vec(),
            )
            .context("Array3 denoised_latent")?;
        }

        // 5. vocoder.
        let wav = {
            let latent_value = Value::from_array(xt)?;
            let out = self.vocoder.run(ort::inputs! {
                "latent" => &latent_value,
            })?;
            let (_, data) = out["wav_tts"].try_extract_tensor::<f32>()?;
            data.to_vec()
        };

        // Przytnij do duration*sample_rate (vocoder dopelnia do pelnego chunku).
        let wav_len = (sample_rate as f32 * duration) as usize;
        let trimmed = wav[..wav_len.min(wav.len())].to_vec();
        Ok((trimmed, duration))
    }

    /// Synteza calego tekstu: chunking -> per-chunk infer -> sklejenie z cisza.
    fn synthesize(&mut self, text: &str, lang: &str, speed: f32) -> Result<Vec<f32>> {
        let max_len = if lang == "ko" || lang == "ja" {
            MAX_CHUNK_LEN_CJK
        } else {
            MAX_CHUNK_LEN_DEFAULT
        };
        let chunks = chunk_text(text, max_len);
        let sample_rate = self.cfg.ae.sample_rate;

        let mut wav_cat: Vec<f32> = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let (wav, _dur) = self.infer_chunk(chunk, lang, speed)?;
            if i > 0 {
                let silence_len = (SILENCE_DURATION * sample_rate as f32) as usize;
                wav_cat.extend(std::iter::repeat_n(0.0f32, silence_len));
            }
            wav_cat.extend_from_slice(&wav);
        }
        Ok(wav_cat)
    }
}

// =============================================================================
// Auto-download z HuggingFace (Supertone/supertonic-3)
// =============================================================================

fn supertonic_cache_dir() -> PathBuf {
    let base = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tentaflow")
        .join("models")
        .join("supertonic");
    std::fs::create_dir_all(&base).ok();
    base
}

/// Pliki modelu w repo (sciezka HF -> nazwa lokalna w korzeniu cache).
const MODEL_FILES: &[(&str, &str)] = &[
    ("onnx/text_encoder.onnx", "text_encoder.onnx"),
    ("onnx/duration_predictor.onnx", "duration_predictor.onnx"),
    ("onnx/vector_estimator.onnx", "vector_estimator.onnx"),
    ("onnx/vocoder.onnx", "vocoder.onnx"),
    ("onnx/tts.json", "tts.json"),
    ("onnx/unicode_indexer.json", "unicode_indexer.json"),
];

/// Voice presety pobierane do `voice_styles/` (5 zenskich + 5 meskich).
const VOICE_PRESETS: &[&str] = &["F1", "F2", "F3", "F4", "F5", "M1", "M2", "M3", "M4", "M5"];

/// Pobiera model + voice_styles z `Supertone/supertonic-3` do
/// `models/supertonic/<repo_sanitized>/`. Idempotentne — jezeli komplet plikow
/// juz istnieje, zwraca natychmiast. Uzywa `download_with_progress`.
pub async fn prepare_model(repo_id: &str) -> Result<PathBuf> {
    let safe_name = repo_id
        .replace('/', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect::<String>();
    let target = supertonic_cache_dir().join(&safe_name);
    std::fs::create_dir_all(target.join("voice_styles")).ok();

    let complete = MODEL_FILES
        .iter()
        .all(|(_, local)| target.join(local).exists())
        && target.join("voice_styles/M1.json").exists();
    if complete {
        info!(
            "[supertonic] uzywam istniejacego cache: {}",
            target.display()
        );
        return Ok(target);
    }

    info!(
        "[supertonic] pobieranie {} -> {}",
        repo_id,
        target.display()
    );
    let base = format!("https://huggingface.co/{repo_id}/resolve/main");

    for (remote, local) in MODEL_FILES {
        let url = format!("{base}/{remote}");
        let dest = target.join(local);
        crate::services::model_download::download_with_progress(&url, &dest, local, None)
            .await
            .with_context(|| format!("pobieranie {remote}"))?;
    }

    for voice in VOICE_PRESETS {
        let remote = format!("voice_styles/{voice}.json");
        let url = format!("{base}/{remote}");
        let dest = target.join(&remote);
        // 404 = brak presetu w repo, nie failujemy (komplet glosow opcjonalny).
        if let Err(e) =
            crate::services::model_download::download_with_progress(&url, &dest, &remote, None)
                .await
        {
            info!("[supertonic] pomijam voice {}: {}", voice, e);
        }
    }

    Ok(target)
}

// =============================================================================
// TtsEngine impl
// =============================================================================

pub struct SupertonicTtsEngine {
    loaded: Mutex<Option<Loaded>>,
    info: Mutex<Option<TtsModelInfo>>,
    /// Katalog modelu (zapisany przy `load_model`) — potrzebny do zmiany glosu
    /// w runtime (dostep do `voice_styles/`).
    model_dir: Mutex<Option<PathBuf>>,
    /// Voice preset z presetu deployu (np. `M1`). `load_model` wybiera plik
    /// `voice_styles/<hint>.json`; bez dopasowania spada na DEFAULT_VOICE lub
    /// pierwszy plik z dysku.
    voice_hint: Mutex<Option<String>>,
}

impl Default for SupertonicTtsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SupertonicTtsEngine {
    pub fn new() -> Self {
        Self {
            loaded: Mutex::new(None),
            info: Mutex::new(None),
            model_dir: Mutex::new(None),
            voice_hint: Mutex::new(None),
        }
    }

    /// Ustawia podpowiedz voice (preset id) przed `load_model`. Z `vits-piper`-
    /// stylu hint wyciagamy stem `M1`/`F2`, jezeli zawarty w stringu.
    pub fn set_voice_hint(&self, hint: Option<&str>) {
        *self.voice_hint.lock().unwrap() = hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
}

/// Wybiera plik voice_style pasujacy do podpowiedzi. Preferuje dokladne dopaso-
/// wanie `<hint>.json`, potem voice ktorego id zawiera sie w hincie (preset
/// moze byc opakowany, np. `supertonic-M1`), potem DEFAULT_VOICE, na koncu
/// pierwszy `.json` z katalogu.
fn pick_voice_style(dir: &Path, hint: Option<&str>) -> Option<PathBuf> {
    let styles_dir = dir.join("voice_styles");
    let available: Vec<PathBuf> = std::fs::read_dir(&styles_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    if available.is_empty() {
        return None;
    }

    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).map(str::to_string);

    if let Some(hint) = hint {
        let hint_lc = hint.to_ascii_lowercase();
        // Dokladne dopasowanie nazwy pliku (np. `M1`).
        if let Some(exact) = available.iter().find(|p| {
            stem(p)
                .map(|s| s.eq_ignore_ascii_case(hint))
                .unwrap_or(false)
        }) {
            return Some(exact.clone());
        }
        // Preset opakowany (np. `supertonic-3-m1`) — szukamy voice id jako
        // sufiksu/podciagu. Porownanie case-insensitive (pliki to `M1`/`F2`,
        // preset id bywa lowercase). Dluzsze stemy najpierw, by `m10` nie
        // zlapal `m1` (tu nie wystepuja, ale rezerwa na przyszle voices).
        let mut by_len: Vec<&PathBuf> = available.iter().collect();
        by_len.sort_by_key(|p| std::cmp::Reverse(stem(p).map(|s| s.len()).unwrap_or(0)));
        if let Some(contained) = by_len.into_iter().find(|p| {
            stem(p)
                .map(|s| hint_lc.contains(&s.to_ascii_lowercase()))
                .unwrap_or(false)
        }) {
            return Some(contained.clone());
        }
    }
    if let Some(default) = available
        .iter()
        .find(|p| stem(p).as_deref() == Some(DEFAULT_VOICE))
    {
        return Some(default.clone());
    }
    available.into_iter().next()
}

/// `ort` runs in `load-dynamic` mode (to coexist with sherpa-rs), so it needs to
/// locate the system `libonnxruntime` at runtime. We probe `ORT_DYLIB_PATH` once
/// (binary dir then standard locations) so a deploy needs no env wiring. The
/// camera-CV path used to do this; it now lives here, the only remaining ort user.
fn ensure_ort_dylib() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }
        let libname = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else {
            "libonnxruntime.so"
        };
        let mut search_dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                search_dirs.push(dir.to_path_buf());
                search_dirs.push(dir.join("lib"));
            }
        }
        // Katalogi z linkera (`LD_LIBRARY_PATH` / `DYLD_*`) — deploy uruchamia
        // binarke z `native-libs/<platform>/lib-dynamic` na tej liscie, gdzie
        // lezy (czesto wylacznie wersjonowany) libonnxruntime.
        let linker_var = if cfg!(target_os = "macos") {
            "DYLD_LIBRARY_PATH"
        } else if cfg!(target_os = "windows") {
            "PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        if let Some(paths) = std::env::var_os(linker_var) {
            for dir in std::env::split_paths(&paths) {
                search_dirs.push(dir);
            }
        }
        for base in [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/opt/onnxruntime/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
        ] {
            search_dirs.push(std::path::PathBuf::from(base));
        }

        // Najpierw dokladna nazwa (`libonnxruntime.so`), potem wersjonowana
        // (`libonnxruntime.so.1.22.0`) — native-libs dostarcza tylko wersjonowany
        // plik, wiec bez tego fallbacku ort load-dynamic nie znajduje runtime'u.
        let found = search_dirs.iter().map(|d| d.join(libname)).find(|p| p.exists())
            .or_else(|| find_versioned_dylib(&search_dirs, libname));
        if let Some(found) = found {
            std::env::set_var("ORT_DYLIB_PATH", &found);
            tracing::info!("[supertonic] ORT_DYLIB_PATH -> {}", found.display());
        } else {
            tracing::warn!(
                "[supertonic] nie znaleziono {libname} (ani wersjonowanego) — ort load-dynamic zawiedzie"
            );
        }
    });
}

/// Szuka wersjonowanego `<libname>.<ver>` (np. `libonnxruntime.so.1.22.0`) w
/// podanych katalogach — `ensure_ort_dylib` probuje go gdy brak dokladnej nazwy.
fn find_versioned_dylib(dirs: &[std::path::PathBuf], libname: &str) -> Option<std::path::PathBuf> {
    let prefix = format!("{libname}.");
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
            {
                return Some(entry.path());
            }
        }
    }
    None
}

impl SupertonicTtsEngine {
    /// Buduje czterosesyjny pipeline z katalogu modelu i wczytuje voice style.
    /// Wydzielone z `load_model`, bo uzywane tez przy zmianie glosu w runtime.
    fn build_loaded(&self, model_dir: &Path) -> Result<(Loaded, String)> {
        ensure_ort_dylib();
        let cfg_bytes = std::fs::read(model_dir.join("tts.json"))
            .with_context(|| format!("brak tts.json w {}", model_dir.display()))?;
        let cfg: Config = serde_json::from_slice(&cfg_bytes).context("parse tts.json")?;

        let processor = UnicodeProcessor::new(&model_dir.join("unicode_indexer.json"))?;

        let session = |name: &str| -> Result<Session> {
            let path = model_dir.join(name);
            Session::builder()
                .context("Session::builder")?
                .commit_from_file(&path)
                .with_context(|| format!("commit ONNX {}", path.display()))
        };
        let dp = session("duration_predictor.onnx")?;
        let text_enc = session("text_encoder.onnx")?;
        let vector_est = session("vector_estimator.onnx")?;
        let vocoder = session("vocoder.onnx")?;

        let hint = self.voice_hint.lock().unwrap().clone();
        let style_path = pick_voice_style(model_dir, hint.as_deref())
            .ok_or_else(|| anyhow!("brak voice_styles/*.json w {}", model_dir.display()))?;
        let voice_name = style_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(DEFAULT_VOICE)
            .to_string();
        let style = load_voice_style(&style_path)?;

        Ok((
            Loaded {
                cfg,
                processor,
                dp,
                text_enc,
                vector_est,
                vocoder,
                style,
                voice: voice_name.clone(),
            },
            voice_name,
        ))
    }
}

impl TtsEngine for SupertonicTtsEngine {
    fn backend_name(&self) -> &str {
        "supertonic"
    }

    fn load_model(&mut self, model_dir: &Path) -> Result<TtsModelInfo> {
        let (loaded, voice_name) = self.build_loaded(model_dir)?;
        let info = TtsModelInfo {
            name: format!("supertonic-{voice_name}"),
            backend: "supertonic".to_string(),
            sample_rate: loaded.cfg.ae.sample_rate as u32,
            speakers: VOICE_PRESETS.len() as u32,
        };
        *self.loaded.lock().unwrap() = Some(loaded);
        *self.info.lock().unwrap() = Some(info.clone());
        *self.model_dir.lock().unwrap() = Some(model_dir.to_path_buf());
        info!("[supertonic] model zaladowany (voice {voice_name})");
        Ok(info)
    }

    fn synthesize(&self, params: SynthesizeParams) -> Result<SynthesizeResult> {
        let lang = params.language.as_deref().unwrap_or(DEFAULT_LANG);
        let speed = if params.speed > 0.0 {
            params.speed
        } else {
            DEFAULT_SPEED
        };

        let mut guard = self.loaded.lock().unwrap();
        let loaded = guard.as_mut().ok_or_else(|| anyhow!("model not loaded"))?;

        // Zmiana glosu w locie: gdy request wskaze inny voice niz zaladowany,
        // przeladuj tylko `style` (sesje ONNX sa niezalezne od glosu).
        if let Some(req_voice) = params.voice.as_deref().filter(|v| !v.is_empty()) {
            if req_voice != loaded.voice {
                let dir = self.model_dir.lock().unwrap().clone();
                if let Some(dir) = dir {
                    if let Some(path) = pick_voice_style(&dir, Some(req_voice)) {
                        loaded.style = load_voice_style(&path)?;
                        loaded.voice = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(req_voice)
                            .to_string();
                    }
                }
            }
        }

        let samples = loaded.synthesize(&params.text, lang, speed)?;
        let sample_rate = loaded.cfg.ae.sample_rate as u32;
        Ok(SynthesizeResult {
            samples,
            sample_rate,
        })
    }

    fn model_info(&self) -> Option<&TtsModelInfo> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_wraps_lang_tag_and_adds_period() {
        let out = preprocess_text("Witaj swiecie", "pl").unwrap();
        assert!(out.starts_with("<pl>"), "tag jezyka: {out}");
        assert!(out.ends_with("</pl>"));
        assert!(out.contains("."), "kropka dolozona: {out}");
    }

    #[test]
    fn preprocess_rejects_unknown_lang() {
        assert!(preprocess_text("hi", "xx").is_err());
    }

    #[test]
    fn preprocess_replaces_at_and_strips_emoji() {
        let out = preprocess_text("mail@x \u{1F600}", "en").unwrap();
        assert!(out.contains(" at "), "@ -> ' at ': {out}");
        assert!(!out.contains('\u{1F600}'), "emoji usuniete: {out}");
    }

    #[test]
    fn chunk_text_splits_long_paragraphs() {
        let long = "Zdanie pierwsze. ".repeat(40);
        let chunks = chunk_text(&long, 300);
        assert!(chunks.len() > 1, "dlugi tekst dzielony na chunki");
        assert!(chunks.iter().all(|c| c.len() <= 320));
    }

    #[test]
    fn length_to_mask_marks_prefix() {
        let m = length_to_mask(3, 5);
        assert_eq!(m[[0, 0, 0]], 1.0);
        assert_eq!(m[[0, 0, 2]], 1.0);
        assert_eq!(m[[0, 0, 3]], 0.0);
    }

    #[test]
    fn sample_noisy_latent_shapes_and_mask() {
        let (noisy, mask) = sample_noisy_latent(0.5, 44100, 512, 6, 24);
        assert_eq!(noisy.shape()[1], 144, "latent_dim_val = 24*6");
        let latent_len = noisy.shape()[2];
        assert_eq!(mask.shape()[2], latent_len);
        // Zamaskowane pozycje (wszystkie tu aktywne) => brak zerowania w prefiksie.
        assert!(latent_len > 0);
    }
}
