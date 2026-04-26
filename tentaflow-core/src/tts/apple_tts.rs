// =============================================================================
// Plik: tts/apple_tts.rs
// Opis: TtsEngine korzystajacy z natywnego Apple AVSpeechSynthesizer przez
//       libMLXBridge.dylib (Swift cdecl `MLXAppleTTS_*`). Aktywny tylko na
//       macOS — iOS tez moze, ale tam nie ma Rusta uzywajacego tego trait'u.
// =============================================================================

#![cfg(feature = "inference-apple-tts")]

use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use tracing::info;

use super::{SynthesizeParams, SynthesizeResult, TtsEngine, TtsModelInfo};

type ListVoicesFn = unsafe extern "C" fn() -> *mut c_char;
type SynthesizeFn = unsafe extern "C" fn(
    text: *const c_char,
    voice_id: *const c_char,
    language: *const c_char,
    rate: f32,
    out_sample_rate: *mut i32,
    out_num_samples: *mut i32,
) -> *mut f32;
type FreeBufferFn = unsafe extern "C" fn(ptr: *mut f32);

extern "C" {
    #[link_name = "free"]
    fn libc_free(ptr: *mut c_void);
}

struct Bridge {
    _lib: &'static Library,
    list_voices: ListVoicesFn,
    synthesize: SynthesizeFn,
    free_buffer: FreeBufferFn,
}

unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

fn open_bridge() -> Result<Bridge> {
    let path = crate::macos_ffi::locate_mlx_bridge_dylib()
        .context("Nie znaleziono libMLXBridge.dylib (Apple TTS)")?;
    crate::macos_ffi::ensure_mlx_metallib_next_to(&path);
    let lib = unsafe { Library::new(&path) }
        .with_context(|| format!("dlopen {} nieudane", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));
    let (lv, syn, fb): (
        Symbol<'static, ListVoicesFn>,
        Symbol<'static, SynthesizeFn>,
        Symbol<'static, FreeBufferFn>,
    ) = unsafe {
        (
            lib.get(b"MLXAppleTTS_listVoices\0")
                .context("Brak symbolu MLXAppleTTS_listVoices (zaktualizuj libMLXBridge.dylib)")?,
            lib.get(b"MLXAppleTTS_synthesize\0")
                .context("Brak symbolu MLXAppleTTS_synthesize")?,
            lib.get(b"MLXAppleTTS_freeBuffer\0")
                .context("Brak symbolu MLXAppleTTS_freeBuffer")?,
        )
    };
    Ok(Bridge {
        _lib: lib,
        list_voices: *lv,
        synthesize: *syn,
        free_buffer: *fb,
    })
}

pub struct AppleTtsEngine {
    bridge: Mutex<Option<Bridge>>,
    info: Mutex<Option<TtsModelInfo>>,
    /// Zapisane preferencje glosu — Apple nie ma ladowania modelu, tylko
    /// wybiera glos przy syntezie. `voice_id` ustawiony przez load_model().
    voice_id: Mutex<Option<String>>,
    language: Mutex<String>,
}

impl AppleTtsEngine {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
            info: Mutex::new(None),
            voice_id: Mutex::new(None),
            language: Mutex::new("en-US".to_string()),
        }
    }

    fn ensure_bridge(&self) -> Result<()> {
        let mut g = self.bridge.lock().unwrap();
        if g.is_none() {
            *g = Some(open_bridge()?);
        }
        Ok(())
    }

    /// Lista dostepnych glosow systemowych jako Vec<JSON> (caller serializuje).
    pub fn list_voices(&self) -> Result<String> {
        self.ensure_bridge()?;
        let list_fn = {
            let g = self.bridge.lock().unwrap();
            g.as_ref().expect("bridge ensured").list_voices
        };
        let ptr = unsafe { list_fn() };
        if ptr.is_null() {
            return Ok("[]".to_string());
        }
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { libc_free(ptr as *mut c_void) };
        Ok(s)
    }
}

impl TtsEngine for AppleTtsEngine {
    fn backend_name(&self) -> &str {
        "apple-tts"
    }

    /// Apple TTS nie ma plikow modelu — `model_dir` traktujemy jako sciezke
    /// "konfiguracji" (ignorowane), albo (nizej priorytet) odczytujemy
    /// `voice_id` z nazwy pliku jezeli to wskazuje glos. W praktyce voice
    /// jest wybierany przy `synthesize` via param.
    fn load_model(&mut self, _model_dir: &Path) -> Result<TtsModelInfo> {
        self.ensure_bridge()?;
        let info = TtsModelInfo {
            name: "apple-tts".to_string(),
            backend: "apple-tts".to_string(),
            sample_rate: 22_050,  // typowy dla Apple compact voices
            speakers: 1,
        };
        *self.info.lock().unwrap() = Some(info.clone());
        info!("[apple-tts] gotowy");
        Ok(info)
    }

    fn synthesize(&self, params: SynthesizeParams) -> Result<SynthesizeResult> {
        self.ensure_bridge()?;
        let synth_fn = {
            let g = self.bridge.lock().unwrap();
            g.as_ref().expect("bridge ensured").synthesize
        };
        let free_fn = {
            let g = self.bridge.lock().unwrap();
            g.as_ref().expect("bridge ensured").free_buffer
        };
        let voice = self.voice_id.lock().unwrap().clone();
        let lang = self.language.lock().unwrap().clone();
        let c_text = CString::new(params.text.replace('\0', " "))?;
        let c_voice = voice.map(|v| CString::new(v).unwrap());
        let c_lang = CString::new(lang)?;
        // Apple `rate`: 0.5 = default. Mapowanie z `params.speed` (1.0 = default)
        // na zakres 0..1: speed=1.0 -> rate=0.5, speed=2.0 -> rate=0.7,
        // speed=0.5 -> rate=0.25. Linijka w przyblizeniu.
        let rate = (params.speed * 0.5).clamp(0.0, 1.0);
        let mut sample_rate: i32 = 0;
        let mut num_samples: i32 = 0;
        let buf_ptr = unsafe {
            synth_fn(
                c_text.as_ptr(),
                c_voice.as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null()),
                c_lang.as_ptr(),
                rate,
                &mut sample_rate as *mut _,
                &mut num_samples as *mut _,
            )
        };
        if buf_ptr.is_null() || num_samples <= 0 {
            anyhow::bail!("Apple TTS synthesize zwrocil pusty bufor");
        }
        let slice = unsafe { std::slice::from_raw_parts(buf_ptr, num_samples as usize) };
        let samples = slice.to_vec();
        unsafe { free_fn(buf_ptr) };
        Ok(SynthesizeResult {
            samples,
            sample_rate: sample_rate as u32,
        })
    }

    fn model_info(&self) -> Option<&TtsModelInfo> {
        // self.info to Mutex — trait wymaga `&TtsModelInfo` ze static lifetime
        // wzgl. self. Niemozliwe przez MutexGuard. Zwracamy None i caller
        // (Router/api) korzysta z `cloned()` info wedlug potrzeby.
        None
    }
}
