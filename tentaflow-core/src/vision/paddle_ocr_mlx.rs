// =============================================================================
// Plik: vision/paddle_ocr_mlx.rs
// Opis: DocumentParser oparty na PaddleOCR-VL (MLX) przez libMLXBridge.dylib
//       (Swift cdecl `MLXPaddleOCR_*`). Parsuje obraz strony do markdownu ze
//       strukturą (tekst + tabele + wzory). Aktywny zawsze na macOS/iOS (gating
//       target_os, BEZ feature flag) — mirror `apple_ocr.rs`: dlopen na macOS,
//       rejestracja wskaznikow ze Swift na iOS. Deploy `paddle-ocr-mlx` ładuje
//       model i rejestruje silnik przez `super::set_document_parser`, dzięki
//       czemu `documents`/`vision_parse` działają jak każdy inny serwis parse.
// =============================================================================

#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use libloading::Library;
#[cfg(target_os = "macos")]
use libloading::Symbol;
use tracing::info;

use super::DocumentParser;

/// `MLXPaddleOCR_load(modelPath)` -> 0 OK / <0 blad.
type LoadFn = unsafe extern "C" fn(model_path: *const c_char) -> i32;
/// `MLXPaddleOCR_unload()`.
type UnloadFn = unsafe extern "C" fn();
/// `MLXPaddleOCR_recognize(bytes, len, task)` -> C string (malloc/strdup;
/// zwalniany przez libc free). NULL przy bledzie.
type RecognizeFn =
    unsafe extern "C" fn(bytes: *const u8, len: i32, task: *const c_char) -> *mut c_char;

extern "C" {
    #[link_name = "free"]
    fn libc_free(ptr: *mut c_void);
}

struct Bridge {
    // macOS trzyma `Library` zywa (dlopen libMLXBridge.dylib). iOS rejestruje
    // wskazniki przy starcie (tentaflow_register_paddle_ocr), tam _lib = None.
    _lib: Option<&'static Library>,
    load: LoadFn,
    unload: UnloadFn,
    recognize: RecognizeFn,
}

unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

#[cfg(target_os = "macos")]
fn open_bridge() -> Result<Bridge> {
    let path = crate::macos_ffi::locate_mlx_bridge_dylib()
        .context("Nie znaleziono libMLXBridge.dylib (PaddleOCR-VL)")?;
    crate::macos_ffi::ensure_mlx_metallib_next_to(&path);
    let lib = unsafe { Library::new(&path) }
        .with_context(|| format!("dlopen {} nieudane", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));
    let (load, unload, recognize): (
        Symbol<'static, LoadFn>,
        Symbol<'static, UnloadFn>,
        Symbol<'static, RecognizeFn>,
    ) = unsafe {
        (
            lib.get(b"MLXPaddleOCR_load\0")
                .context("Brak symbolu MLXPaddleOCR_load (zaktualizuj libMLXBridge.dylib)")?,
            lib.get(b"MLXPaddleOCR_unload\0")
                .context("Brak symbolu MLXPaddleOCR_unload")?,
            lib.get(b"MLXPaddleOCR_recognize\0")
                .context("Brak symbolu MLXPaddleOCR_recognize")?,
        )
    };
    Ok(Bridge {
        _lib: Some(lib),
        load: *load,
        unload: *unload,
        recognize: *recognize,
    })
}

#[cfg(target_os = "ios")]
fn open_bridge() -> Result<Bridge> {
    let reg = PADDLE_OCR_REG.get().context(
        "PaddleOCR-VL nie zarejestrowany ze Swift — tentaflow_register_paddle_ocr \
         nie zostalo wywolane przy starcie aplikacji",
    )?;
    Ok(Bridge {
        _lib: None,
        load: reg.load,
        unload: reg.unload,
        recognize: reg.recognize,
    })
}

#[cfg(target_os = "ios")]
struct PaddleOcrRegistration {
    load: LoadFn,
    unload: UnloadFn,
    recognize: RecognizeFn,
}
#[cfg(target_os = "ios")]
unsafe impl Send for PaddleOcrRegistration {}
#[cfg(target_os = "ios")]
unsafe impl Sync for PaddleOcrRegistration {}
#[cfg(target_os = "ios")]
static PADDLE_OCR_REG: std::sync::OnceLock<PaddleOcrRegistration> = std::sync::OnceLock::new();

/// Rejestruje wskazniki cdecl PaddleOCR-VL ze strony Swift (iOS). Mirror
/// `tentaflow_register_apple_ocr`.
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn tentaflow_register_paddle_ocr(
    load: LoadFn,
    unload: UnloadFn,
    recognize: RecognizeFn,
) {
    let _ = PADDLE_OCR_REG.set(PaddleOcrRegistration {
        load,
        unload,
        recognize,
    });
    tracing::info!("[paddle-ocr] Swift callbacks zarejestrowane");
}

/// Domyslne zadanie dla powierzchni `documents` parse: pelnostronicowy OCR z
/// zachowaniem ukladu (markdown). Tabele/wzory jako osobne zadania (table/
/// formula) sa dostepne przez recognize, ale parse-surface nie niesie taska.
const DEFAULT_TASK: &str = "ocr";

pub struct PaddleOcrMlxEngine {
    bridge: Mutex<Option<Bridge>>,
}

impl PaddleOcrMlxEngine {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
        }
    }

    /// Otwiera most i ładuje model z katalogu (HF safetensors MLX). Wolane raz
    /// przy deployu — brak dylibu / błąd ładowania zgłaszany od razu.
    pub fn load(&self, model_path: &Path) -> Result<()> {
        let path_str = model_path
            .to_str()
            .context("PaddleOCR: ścieżka modelu nie jest poprawnym UTF-8")?;
        let c_path = CString::new(path_str)?;
        let mut g = self.bridge.lock().unwrap();
        if g.is_none() {
            *g = Some(open_bridge()?);
        }
        let load_fn = g.as_ref().expect("bridge ensured").load;
        let code = unsafe { load_fn(c_path.as_ptr()) };
        if code < 0 {
            anyhow::bail!("PaddleOCR: ładowanie modelu nieudane (kod {code})");
        }
        Ok(())
    }

    fn recognize(&self, image_bytes: &[u8], task: &str) -> Result<String> {
        let recognize_fn = {
            let g = self.bridge.lock().unwrap();
            g.as_ref()
                .context("PaddleOCR: most nie zainicjalizowany (deploy nie wołał load?)")?
                .recognize
        };
        let c_task = CString::new(task)?;
        let ptr = unsafe {
            recognize_fn(
                image_bytes.as_ptr(),
                image_bytes.len() as i32,
                c_task.as_ptr(),
            )
        };
        if ptr.is_null() {
            anyhow::bail!("PaddleOCR: recognize zwrócił NULL");
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { libc_free(ptr as *mut c_void) };
        Ok(text)
    }
}

impl Default for PaddleOcrMlxEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PaddleOcrMlxEngine {
    fn drop(&mut self) {
        if let Some(b) = self.bridge.lock().unwrap().as_ref() {
            unsafe { (b.unload)() };
        }
    }
}

impl DocumentParser for PaddleOcrMlxEngine {
    fn parse(&self, image_bytes: &[u8], _mime: &str) -> Result<String> {
        self.recognize(image_bytes, DEFAULT_TASK)
    }
}

/// Ładuje model PaddleOCR-VL z katalogu i rejestruje silnik jako globalny
/// in-process DocumentParser (`super::set_document_parser`). Wolane przez deploy
/// embedded `paddle-ocr-mlx`.
pub fn register_as_document_parser(model_path: &Path) -> Result<()> {
    let engine = PaddleOcrMlxEngine::new();
    engine
        .load(model_path)
        .context("init paddle-ocr-mlx (brak libMLXBridge.dylib lub model?)")?;
    super::set_document_parser(std::sync::Arc::new(engine));
    info!("[paddle-ocr-mlx] zarejestrowany jako in-process DocumentParser");
    Ok(())
}

/// Wyrejestrowuje parser (rollback / stop service).
pub fn unregister_as_document_parser() {
    super::clear_document_parser();
}
