// =============================================================================
// Plik: stt/mlx_whisper.rs
// Opis: SttEngine delegujacy do mlx-swift przez libMLXBridge.dylib. Wczesniej
//       calosc (architektura, log-mel, tokenizer, decoder) zostala napisana
//       po stronie Swift w `tentaflow-desktop/macos/swift/MLXBridge/Sources/MLXBridge/Whisper/`.
//       Tutaj jedynie dlopen + cienkie wywolania extern "C".
//
//       Dylib `libMLXBridge.dylib` jest budowany przez build.rs `tentaflow/`
//       i kopiowany obok wynikowej binarki tentaflow. Brak dylibu == brak
//       Whispera MLX, ale to nie jest blad startup'u — fallback na whisper.cpp.
// =============================================================================

#![cfg(feature = "inference-mlx-whisper")]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use libloading::{Library, Symbol};
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::{
    SttEngine, SttModelInfo, TranscribeChunk, TranscribeParams, TranscribeResult, TranscribeSegment,
};

// =============================================================================
// FFI kontrakt — odpowiada @_cdecl symbolom w WhisperEngine.swift
// =============================================================================

type GetContextFn = unsafe extern "C" fn() -> *mut c_void;
type LoadModelFn = unsafe extern "C" fn(*const c_char, *mut c_void) -> i32;
type UnloadModelFn = unsafe extern "C" fn(*mut c_void);
type TranscribeFn = unsafe extern "C" fn(
    pcm_ptr: *const f32,
    n_samples: i32,
    language: *const c_char,
    context: *mut c_void,
) -> *mut c_char;

// libc free dla stringow alokowanych przez strdup po stronie Swift.
extern "C" {
    #[link_name = "free"]
    fn libc_free(ptr: *mut c_void);
}

/// Dlibrary + 4 symbole + Swift singleton context. `'static` Library bo zyje
/// do konca procesu (Box::leak po dlopen). Trzymamy SUROWE function pointery
/// (Copy) zamiast `Symbol<'static, _>`, zeby mozna bylo skopiowac do
/// `spawn_blocking` bez `move out of shared reference`.
struct Bridge {
    _lib: &'static Library,
    load_fn: LoadModelFn,
    unload_fn: UnloadModelFn,
    transcribe_fn: TranscribeFn,
    context: *mut c_void,
}

// SAFETY: Swift side ma DispatchSemaphore i singleton thread-safe.
unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

/// Folder cache dla zmergowanych snapshotow `mlx-community/whisper-* + openai
/// tokenizer`. Obie czesci pobieramy z HF Hub i kopiujemy do jednego katalogu
/// zeby `WhisperLoader.load(directory:)` widzial pelen zestaw plikow.
fn mlx_whisper_cache_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tentaflow")
        .join("models")
        .join("mlx-whisper");
    std::fs::create_dir_all(&base).ok();
    base
}

/// Mapa wariant modelu (`whisper-large-v3-turbo-4bit`) → repo z tokenizerem.
/// `mlx-community` wszystkie warianty Whispera bazuja na tym samym tokenizerze,
/// rozni sie tylko `n_vocab` (51865 dla -v2 i wczesniejszych, 51866 dla v3).
/// Dla wszystkich v3-* uzywamy openai/whisper-large-v3-turbo (51866 vocab,
/// nowy `<|nospeech|>` token); dla v2 i v1 — openai/whisper-large-v2.
fn tokenizer_repo_for(mlx_model_id: &str) -> &'static str {
    let lower = mlx_model_id.to_lowercase();
    if lower.contains("v3") || lower.contains("turbo") {
        "openai/whisper-large-v3-turbo"
    } else {
        "openai/whisper-large-v2"
    }
}

/// Pobiera (jezeli trzeba) i przygotowuje katalog z modelem MLX Whisper +
/// tokenizerem HF. `mlx_repo_id` to np. "mlx-community/whisper-large-v3-turbo-4bit"
/// — pobierzemy z niego `config.json` i `model.safetensors`. Tokenizer
/// dociagamy z `openai/whisper-large-v3-turbo` (wlasciwy dobierany przez
/// `tokenizer_repo_for`).
///
/// Zwraca sciezke do scalonego katalogu, ktora mozna podac do `MLXWhisper_loadModel`.
pub async fn prepare_model(mlx_repo_id: &str) -> Result<PathBuf> {
    use hf_hub::api::sync::Api;
    let target = mlx_whisper_cache_dir().join(
        mlx_repo_id
            .replace('/', "_")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
            .collect::<String>(),
    );
    std::fs::create_dir_all(&target).context("create mlx-whisper cache dir")?;

    // Sprawdzamy szybko: jezeli mamy `config.json`, `model.safetensors` i
    // `tokenizer.json` — uznajemy katalog za przygotowany. Kasowanie cache
    // recznie, jezeli ktos chce odswiezyc model.
    let already = target.join("config.json").exists()
        && target.join("model.safetensors").exists()
        && target.join("tokenizer.json").exists();
    if already {
        info!(
            "[mlx-whisper] uzywam istniejacego cache: {}",
            target.display()
        );
        return Ok(target);
    }

    let mlx_id = mlx_repo_id.to_string();
    let oai_id = tokenizer_repo_for(mlx_repo_id).to_string();
    let target_clone = target.clone();
    info!(
        "[mlx-whisper] pobieranie {} + tokenizer z {}",
        mlx_id, oai_id
    );

    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let api = Api::new().context("hf-hub Api::new")?;

        // Lista plikow do pobrania z kazdego repo. Tokeny + JSON-y maja staly
        // zestaw nazw — jezeli ktores nie istnieje, ignorujemy (nie wszystkie
        // repo zawieraja `added_tokens.json`).
        let mlx_files = ["config.json", "model.safetensors"];
        let oai_files = [
            "tokenizer.json",
            "tokenizer_config.json",
            "added_tokens.json",
            "special_tokens_map.json",
            "generation_config.json",
            "vocab.json",
            "merges.txt",
            "normalizer.json",
        ];

        let mlx_repo = api.model(mlx_id.clone());
        for f in mlx_files.iter() {
            let src = mlx_repo
                .get(f)
                .with_context(|| format!("download {}/{}", mlx_id, f))?;
            let dst = target_clone.join(f);
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        }

        let oai_repo = api.model(oai_id.clone());
        for f in oai_files.iter() {
            // Tokenizer files — niektore opcjonalne. Brak nie jest bledem.
            let src = match oai_repo.get(f) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let dst = target_clone.join(f);
            std::fs::copy(&src, &dst).with_context(|| {
                format!("copy tokenizer {} -> {}", src.display(), dst.display())
            })?;
        }
        Ok(())
    })
    .await
    .context("blocking task panic")?;
    result?;

    info!("[mlx-whisper] gotowy: {}", target.display());
    Ok(target)
}

// Helpery `locate_dylib` + `ensure_metallib_next_to` zostaly przeniesione do
// `crate::macos_ffi` zeby mogly z nich korzystac inne moduly Apple-specific
// (apple_tts, mlx_kokoro). Lokalne fn ponizej deleguja do wspoldzielonego.
fn locate_dylib() -> Option<PathBuf> {
    crate::macos_ffi::locate_mlx_bridge_dylib()
}
fn ensure_metallib_next_to(dylib: &std::path::Path) {
    crate::macos_ffi::ensure_mlx_metallib_next_to(dylib)
}

fn open_bridge() -> Result<Bridge> {
    let path = locate_dylib().context(
        "Nie znaleziono libMLXBridge.dylib — zbuduj projekt cargo build (build.rs odpala swift build)",
    )?;
    ensure_metallib_next_to(&path);
    info!("[mlx-whisper] dlopen {}", path.display());
    // SAFETY: dylib z naszego repo, nie ma kodu wykonywanego przy load.
    let lib = unsafe { Library::new(&path) }
        .with_context(|| format!("dlopen {} nieudane", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));

    // SAFETY: te symbole istnieja w libMLXBridge.dylib (zbudowanym z naszego
    // Swift kodu); jezeli nie, error propaguje wyzej i fallback na whisper.cpp.
    let (get_context_fn, load_fn, unload_fn, transcribe_fn): (
        Symbol<'static, GetContextFn>,
        Symbol<'static, LoadModelFn>,
        Symbol<'static, UnloadModelFn>,
        Symbol<'static, TranscribeFn>,
    ) = unsafe {
        (
            lib.get(b"MLXWhisper_getContext\0")
                .context("Brak symbolu MLXWhisper_getContext (Swift dylib bez Whispera?)")?,
            lib.get(b"MLXWhisper_loadModel\0")
                .context("Brak symbolu MLXWhisper_loadModel")?,
            lib.get(b"MLXWhisper_unloadModel\0")
                .context("Brak symbolu MLXWhisper_unloadModel")?,
            lib.get(b"MLXWhisper_transcribe\0")
                .context("Brak symbolu MLXWhisper_transcribe")?,
        )
    };
    let context = unsafe { get_context_fn() };
    if context.is_null() {
        anyhow::bail!("MLXWhisper_getContext zwrocil NULL");
    }
    // Symbol -> raw fn pointer. Symbol referencuje `_lib` (`'static`), wiec
    // pointer pozostaje wazny do konca procesu.
    Ok(Bridge {
        _lib: lib,
        load_fn: *load_fn,
        unload_fn: *unload_fn,
        transcribe_fn: *transcribe_fn,
        context,
    })
}

// =============================================================================
// SttEngine impl
// =============================================================================

pub struct MlxWhisperEngine {
    bridge: Mutex<Option<Bridge>>,
    info: Mutex<Option<SttModelInfo>>,
}

impl MlxWhisperEngine {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
            info: Mutex::new(None),
        }
    }

    fn ensure_bridge(&self) -> Result<()> {
        let mut guard = self.bridge.lock().unwrap();
        if guard.is_none() {
            *guard = Some(open_bridge()?);
        }
        Ok(())
    }
}

#[async_trait]
impl SttEngine for MlxWhisperEngine {
    fn backend_name(&self) -> &str {
        "mlx-whisper"
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["wav".to_string(), "pcm".to_string()]
    }

    async fn load_model(&self, model_path: &Path, _device: Option<&str>) -> Result<SttModelInfo> {
        self.ensure_bridge()?;
        // Akceptujemy dwa formaty `model_path`:
        //   1. Lokalna scieżka istniejaca na dysku — uzywamy bezposrednio.
        //   2. HF repo_id w formacie "owner/name" (zawiera `/` ale nie jest
        //      sciezka systemowa) — pobieramy + scalamy z openai tokenizerem,
        //      potem ladujemy ze scalonego cache.
        let resolved_path: PathBuf = if model_path.exists() {
            model_path.to_path_buf()
        } else {
            let s = model_path
                .to_str()
                .context("Sciezka modelu zawiera nieprawidlowe znaki UTF-8")?;
            if s.contains('/') && !s.starts_with('/') && !s.starts_with('.') {
                info!("[mlx-whisper] traktuje '{}' jako HF repo_id", s);
                prepare_model(s).await?
            } else {
                anyhow::bail!("Sciezka modelu nie istnieje: {}", s);
            }
        };
        let path_str = resolved_path
            .to_str()
            .context("Sciezka modelu zawiera nieprawidlowe znaki UTF-8")?
            .to_string();
        let (ctx_addr, load_fn) = {
            let guard = self.bridge.lock().unwrap();
            let b = guard.as_ref().expect("bridge ensured powyzej");
            (b.context as usize, b.load_fn)
        };
        // Swift load_model robi semafor + Task — moze blokowac sekundy. Idziemy
        // przez spawn_blocking zeby nie zatkac runtime'u tokio.
        let result = tokio::task::spawn_blocking(move || -> i32 {
            let c_path = CString::new(path_str.replace('\0', "_")).unwrap_or_default();
            unsafe { load_fn(c_path.as_ptr(), ctx_addr as *mut c_void) }
        })
        .await
        .context("Blad watku ladowania modelu MLX Whisper")?;
        if result < 0 {
            anyhow::bail!("MLXWhisper_loadModel zwrocil {}", result);
        }
        // size_bytes — sumujemy weights*.safetensors w katalogu modelu, zeby
        // dashboard pokazywal sensowna wartosc bez pytania Swift side.
        let size = walk_size(&resolved_path).unwrap_or(0);
        let info = SttModelInfo {
            name: resolved_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("mlx-whisper")
                .to_string(),
            path: resolved_path.to_string_lossy().to_string(),
            size_bytes: size,
            model_type: "whisper".to_string(),
            backend: "mlx-whisper".to_string(),
            loaded: true,
            device: "metal".to_string(),
        };
        *self.info.lock().unwrap() = Some(info.clone());
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        let bridge_ptr_opt = {
            let guard = self.bridge.lock().unwrap();
            guard.as_ref().map(|b| (b.context as usize, b.unload_fn))
        };
        if let Some((ctx, unload_fn)) = bridge_ptr_opt {
            tokio::task::spawn_blocking(move || {
                unsafe { unload_fn(ctx as *mut c_void) };
            })
            .await
            .ok();
        }
        *self.info.lock().unwrap() = None;
        Ok(())
    }

    fn model_info(&self) -> Option<SttModelInfo> {
        self.info.lock().unwrap().clone()
    }

    async fn transcribe(&self, params: TranscribeParams) -> Result<TranscribeResult> {
        let started = Instant::now();
        // Wejscie: PCM 16-bit mono 16 kHz w `audio_data` jako WAV — parsujemy
        // jak whisper.rs. Konwertujemy na f32 [-1, 1] bo Swift side przyjmuje
        // Float32 bezposrednio.
        let pcm = decode_wav_to_f32(&params.audio_data)?;
        let lang = params.language.unwrap_or_else(|| "en".to_string());
        let (ctx_addr, transcribe_fn) = {
            let guard = self.bridge.lock().unwrap();
            let b = guard
                .as_ref()
                .context("MLXWhisper engine: model nie zaladowany")?;
            (b.context as usize, b.transcribe_fn)
        };
        let pcm_clone = pcm.clone();
        let lang_clone = lang.clone();
        let raw_text = tokio::task::spawn_blocking(move || -> Option<String> {
            let lang_c = CString::new(lang_clone).ok()?;
            let n = pcm_clone.len() as i32;
            let ptr = unsafe {
                transcribe_fn(
                    pcm_clone.as_ptr(),
                    n,
                    lang_c.as_ptr(),
                    ctx_addr as *mut c_void,
                )
            };
            if ptr.is_null() {
                return None;
            }
            // SAFETY: Swift side wykonuje strdup() — zwalniamy przez libc free.
            let text = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { libc_free(ptr as *mut c_void) };
            Some(text)
        })
        .await
        .context("Blad watku transkrypcji MLX Whisper")?;
        let text = raw_text.unwrap_or_default();

        let duration_secs = pcm.len() as f64 / 16_000.0;
        debug!(
            "[mlx-whisper] transcribe {} s audio -> {} znakow w {} ms",
            duration_secs,
            text.len(),
            started.elapsed().as_millis()
        );
        // MVP: jeden segment obejmujacy cale wejscie. Word timestamps i
        // multi-segment sa robota dla follow-up'u (DTW na cross-attn).
        let segment = TranscribeSegment {
            id: 0,
            start: 0.0,
            end: duration_secs,
            text: text.clone(),
            no_speech_prob: 0.0,
            avg_logprob: 0.0,
            compression_ratio: 0.0,
            tokens: Vec::new(),
        };
        Ok(TranscribeResult {
            text,
            language: lang,
            duration_seconds: duration_secs,
            segments: vec![segment],
        })
    }

    async fn transcribe_stream(
        &self,
        params: TranscribeParams,
    ) -> Result<mpsc::Receiver<TranscribeChunk>> {
        // Streaming nie jest jeszcze zaimplementowany po stronie Swift —
        // emulujemy "single chunk on completion" zeby caller dostal interfejs
        // mpsc bez krytycznego brakujacego kawalka.
        let (tx, rx) = mpsc::channel(2);
        let result = self.transcribe(params).await?;
        let _ = tx
            .send(TranscribeChunk {
                text: result.text.clone(),
                is_final: true,
                segment: result.segments.first().cloned(),
            })
            .await;
        Ok(rx)
    }
}

/// Sumuje rozmiary wszystkich plikow w katalogu (rekursywnie). Uzywane do
/// raportowania `size_bytes` w SttModelInfo.
fn walk_size(dir: &Path) -> Option<u64> {
    if dir.is_file() {
        return std::fs::metadata(dir).ok().map(|m| m.len());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let p = entry.path();
        if p.is_file() {
            total += entry.metadata().ok().map(|m| m.len()).unwrap_or(0);
        } else if p.is_dir() {
            total += walk_size(&p).unwrap_or(0);
        }
    }
    Some(total)
}

/// Dekoder audio: uzywa wspoldzielonego `crate::stt::audio::decode_to_pcm_f32`
/// ktory akceptuje WAV / MP3 / OGG / raw PCM 16-bit LE 16 kHz mono. Bez tego
/// teams-bot dostawal "Nie-WAV input" bo wysyla raw PCM bez RIFF headera
/// (oszczedzajac na 44 B per chunk audio).
fn decode_wav_to_f32(audio: &[u8]) -> Result<Vec<f32>> {
    crate::stt::audio::decode_to_pcm_f32(audio).map_err(|e| anyhow::anyhow!("decode audio: {}", e))
}
