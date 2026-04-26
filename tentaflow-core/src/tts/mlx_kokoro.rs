// =============================================================================
// Plik: tts/mlx_kokoro.rs
// Opis: TtsEngine + auto-download dla Kokoro 82M przez libKokoroBridge.dylib.
//       Symbol kontrakt z Swift bridge'em (`KokoroBridge.swift`):
//         Kokoro_getContext()
//         Kokoro_loadModel(path) -> 0/-1
//         Kokoro_synthesize(text, voice, lang, speed, *out_sr, *out_n) -> *Float
//         Kokoro_freeBuffer(ptr)
//         Kokoro_unloadModel()
// =============================================================================

#![cfg(feature = "inference-mlx-kokoro")]

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use tracing::info;

use super::{SynthesizeParams, SynthesizeResult, TtsEngine, TtsModelInfo};

type GetContextFn = unsafe extern "C" fn() -> *mut c_void;
type LoadModelFn = unsafe extern "C" fn(*const c_char, *mut c_void) -> i32;
type UnloadModelFn = unsafe extern "C" fn(*mut c_void);
type SynthesizeFn = unsafe extern "C" fn(
    text: *const c_char,
    voice: *const c_char,
    language: *const c_char,
    speed: f32,
    out_sample_rate: *mut i32,
    out_num_samples: *mut i32,
    context: *mut c_void,
) -> *mut f32;
type FreeBufferFn = unsafe extern "C" fn(ptr: *mut f32);

struct Bridge {
    _lib: &'static Library,
    load_fn: LoadModelFn,
    unload_fn: UnloadModelFn,
    synthesize_fn: SynthesizeFn,
    free_buffer_fn: FreeBufferFn,
    context: *mut c_void,
}

unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

fn locate_kokoro_dylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?.to_path_buf();
    let mut candidates: Vec<PathBuf> = vec![
        exe_dir.join("libKokoroBridge.dylib"),
        exe_dir.join("../Frameworks/libKokoroBridge.dylib"),
    ];
    let mut current = exe_dir.clone();
    for _ in 0..6 {
        for sub in [
            "tentaflow/target/release/libKokoroBridge.dylib",
            "tentaflow/target/debug/libKokoroBridge.dylib",
            "tentaflow-desktop/macos/swift/KokoroBridge/.build/arm64-apple-macosx/release/libKokoroBridge.dylib",
            "tentaflow-desktop/macos/swift/KokoroBridge/.build/arm64-apple-macosx/debug/libKokoroBridge.dylib",
        ] {
            candidates.push(current.join(sub));
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

fn open_bridge() -> Result<Bridge> {
    let path = locate_kokoro_dylib()
        .context("Nie znaleziono libKokoroBridge.dylib — zbuduj projekt cargo build")?;
    crate::macos_ffi::ensure_kokoro_metallib_next_to(&path);
    info!("[mlx-kokoro] dlopen {}", path.display());
    // SAFETY: dylib z naszego repo.
    let lib = unsafe { Library::new(&path) }
        .with_context(|| format!("dlopen {} nieudane", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));
    let (get_ctx, load_fn, unload_fn, synthesize_fn, free_buffer_fn): (
        Symbol<'static, GetContextFn>,
        Symbol<'static, LoadModelFn>,
        Symbol<'static, UnloadModelFn>,
        Symbol<'static, SynthesizeFn>,
        Symbol<'static, FreeBufferFn>,
    ) = unsafe {
        (
            lib.get(b"Kokoro_getContext\0").context("Brak Kokoro_getContext")?,
            lib.get(b"Kokoro_loadModel\0").context("Brak Kokoro_loadModel")?,
            lib.get(b"Kokoro_unloadModel\0").context("Brak Kokoro_unloadModel")?,
            lib.get(b"Kokoro_synthesize\0").context("Brak Kokoro_synthesize")?,
            lib.get(b"Kokoro_freeBuffer\0").context("Brak Kokoro_freeBuffer")?,
        )
    };
    let context = unsafe { get_ctx() };
    if context.is_null() {
        anyhow::bail!("Kokoro_getContext zwrocil NULL");
    }
    Ok(Bridge {
        _lib: lib,
        load_fn: *load_fn,
        unload_fn: *unload_fn,
        synthesize_fn: *synthesize_fn,
        free_buffer_fn: *free_buffer_fn,
        context,
    })
}

// =============================================================================
// Auto-download Kokoro 82M MLX z HuggingFace
// =============================================================================

fn kokoro_cache_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tentaflow")
        .join("models")
        .join("mlx-kokoro");
    std::fs::create_dir_all(&base).ok();
    base
}

/// Pobiera wagi + kanoniczne voices z `mlx-community/Kokoro-82M-bf16`.
/// Caller (deploy handler) moze w configu wybrac ktore voices pobrac, default
/// pobiera ~12 najpopularniejszych zeby cache nie urosl do 100 MB.
pub async fn prepare_model(repo_id: &str) -> Result<PathBuf> {
    use hf_hub::api::sync::Api;
    let safe_name = repo_id
        .replace('/', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect::<String>();
    let target = kokoro_cache_dir().join(safe_name);
    std::fs::create_dir_all(&target).ok();
    std::fs::create_dir_all(target.join("voices")).ok();

    let already = target.join("kokoro-v1_0.safetensors").exists()
        && target.join("config.json").exists()
        && target.join("voices/af_heart.safetensors").exists();
    if already {
        info!("[mlx-kokoro] uzywam istniejacego cache: {}", target.display());
        return Ok(target);
    }

    let repo = repo_id.to_string();
    let target_clone = target.clone();
    info!("[mlx-kokoro] pobieranie {} → {}", repo, target.display());

    tokio::task::spawn_blocking(move || -> Result<()> {
        let api = Api::new().context("hf-hub Api::new")?;
        let r = api.model(repo.clone());

        // Wagi + config (~165 MB).
        for f in ["kokoro-v1_0.safetensors", "config.json"] {
            let src = r.get(f).with_context(|| format!("get {}/{}", repo, f))?;
            let dst = target_clone.join(f);
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        }
        // Standardowy zestaw voices: 6 zenskich US + 4 meskich US + 2 GB.
        // Po ~1 MB kazdy = 12 MB. Caller moze sie pobrac wiecej.
        let voices = [
            "af_heart", "af_alloy", "af_aoede", "af_bella", "af_jessica", "af_nova",
            "am_adam", "am_michael", "am_echo", "am_fenrir",
            "bf_alice", "bm_george",
        ];
        for v in voices {
            let fname = format!("voices/{}.safetensors", v);
            // 404 oznacza brak voice w tym repo — opcjonalne, nie failujemy.
            let src = match r.get(&fname) {
                Ok(p) => p,
                Err(e) => {
                    info!("[mlx-kokoro] pomijam voice {}: {}", v, e);
                    continue;
                }
            };
            let dst = target_clone.join(&fname);
            std::fs::copy(&src, &dst).ok();
        }
        Ok(())
    })
    .await
    .context("blocking task panic")??;
    Ok(target)
}

// =============================================================================
// TtsEngine impl
// =============================================================================

pub struct MlxKokoroEngine {
    bridge: Mutex<Option<Bridge>>,
    info: Mutex<Option<TtsModelInfo>>,
    /// Domyslny voice uzywany gdy params.speaker_id mapuje sie nieznanego.
    default_voice: Mutex<String>,
    language: Mutex<String>,
}

impl MlxKokoroEngine {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
            info: Mutex::new(None),
            default_voice: Mutex::new("af_heart".to_string()),
            language: Mutex::new("en-us".to_string()),
        }
    }

    pub fn set_default_voice(&self, name: impl Into<String>) {
        *self.default_voice.lock().unwrap() = name.into();
    }

    fn ensure_bridge(&self) -> Result<()> {
        let mut g = self.bridge.lock().unwrap();
        if g.is_none() {
            *g = Some(open_bridge()?);
        }
        Ok(())
    }
}

impl TtsEngine for MlxKokoroEngine {
    fn backend_name(&self) -> &str { "mlx-kokoro" }

    fn load_model(&mut self, model_dir: &Path) -> Result<TtsModelInfo> {
        self.ensure_bridge()?;
        let path_str = model_dir
            .to_str()
            .context("model_dir nie-UTF8")?
            .to_string();
        let (load_fn, ctx) = {
            let g = self.bridge.lock().unwrap();
            let b = g.as_ref().expect("bridge ensured");
            (b.load_fn, b.context as usize)
        };
        let c_path = CString::new(path_str.clone()).context("CString load_model")?;
        let result = unsafe { load_fn(c_path.as_ptr(), ctx as *mut c_void) };
        if result < 0 {
            anyhow::bail!("Kokoro_loadModel(.) zwrocil {}", result);
        }
        let info = TtsModelInfo {
            name: model_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("kokoro-82m")
                .to_string(),
            backend: "mlx-kokoro".to_string(),
            sample_rate: 24_000,
            speakers: 0,  // dynamiczne, voices listowane przez Kokoro_listVoices
        };
        *self.info.lock().unwrap() = Some(info.clone());
        info!("[mlx-kokoro] model zaladowany: {}", path_str);
        Ok(info)
    }

    fn synthesize(&self, params: SynthesizeParams) -> Result<SynthesizeResult> {
        self.ensure_bridge()?;
        let (synth_fn, free_fn, ctx) = {
            let g = self.bridge.lock().unwrap();
            let b = g.as_ref().expect("bridge ensured");
            (b.synthesize_fn, b.free_buffer_fn, b.context as usize)
        };
        let voice = self.default_voice.lock().unwrap().clone();
        let lang = self.language.lock().unwrap().clone();
        let c_text = CString::new(params.text.replace('\0', " "))?;
        let c_voice = CString::new(voice)?;
        let c_lang = CString::new(lang)?;
        let mut out_sr: i32 = 0;
        let mut out_n: i32 = 0;
        let buf = unsafe {
            synth_fn(
                c_text.as_ptr(),
                c_voice.as_ptr(),
                c_lang.as_ptr(),
                params.speed,
                &mut out_sr as *mut _,
                &mut out_n as *mut _,
                ctx as *mut c_void,
            )
        };
        if buf.is_null() || out_n <= 0 {
            anyhow::bail!("Kokoro synthesize zwrocil pusty bufor");
        }
        let slice = unsafe { std::slice::from_raw_parts(buf, out_n as usize) };
        let samples = slice.to_vec();
        unsafe { free_fn(buf) };
        Ok(SynthesizeResult {
            samples,
            sample_rate: out_sr as u32,
        })
    }

    fn model_info(&self) -> Option<&TtsModelInfo> { None }
}
