// =============================================================================
// Plik: vision/apple_ocr.rs
// Opis: OcrRunner korzystajacy z natywnego Apple Vision (`VNRecognizeTextRequest`)
//       przez libMLXBridge.dylib (Swift cdecl `MLXAppleOCR_*`). Aktywny tylko na
//       macOS/iOS. Mirror `tts/apple_tts.rs`: dlopen na macOS, rejestracja
//       wskaznikow ze Swift na iOS.
//
//       Vision przyjmuje zakodowane bajty obrazu (PNG/JPG), a nie surowy RGB —
//       runner enkoduje crop RGB24 do PNG przed wywolaniem FFI, zeby pasowac do
//       kontraktu `OcrRunner` uzywanego przez VisionDispatcher i camera enrich.
// =============================================================================

#![cfg(any(target_os = "macos", target_os = "ios"))]

use std::ffi::{c_char, CString};
use std::sync::Mutex;

use anyhow::{Context, Result};
use libloading::Library;
#[cfg(target_os = "macos")]
use libloading::Symbol;
use serde::Deserialize;
use tracing::info;

use super::OcrRunner;

/// `MLXAppleOCR_recognize(bytes, len, langs, useLanguageCorrection, out_len)`
/// -> JSON string (malloc'd przez strdup). NULL przy bledzie.
type RecognizeFn = unsafe extern "C" fn(
    bytes: *const u8,
    len: i32,
    langs: *const c_char,
    use_language_correction: i32,
    out_len: *mut i32,
) -> *mut c_char;
/// `MLXAppleOCR_freeString(ptr)` — zwalnia JSON z `recognize` (NULL-safe).
type FreeStringFn = unsafe extern "C" fn(ptr: *mut c_char);

struct Bridge {
    // macOS trzyma `Library` zywa przez caly czas (dlopen libMLXBridge.dylib).
    // iOS nie ma dylib do dlopen — Swift rejestruje wskazniki przy starcie
    // (tentaflow_register_apple_ocr), wiec tam _lib = None.
    _lib: Option<&'static Library>,
    recognize: RecognizeFn,
    free_string: FreeStringFn,
}

unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

#[cfg(target_os = "macos")]
fn open_bridge() -> Result<Bridge> {
    let path = crate::macos_ffi::locate_mlx_bridge_dylib()
        .context("Nie znaleziono libMLXBridge.dylib (Apple OCR)")?;
    crate::macos_ffi::ensure_mlx_metallib_next_to(&path);
    let lib = unsafe { Library::new(&path) }
        .with_context(|| format!("dlopen {} nieudane", path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));
    let (rec, fs): (Symbol<'static, RecognizeFn>, Symbol<'static, FreeStringFn>) = unsafe {
        (
            lib.get(b"MLXAppleOCR_recognize\0")
                .context("Brak symbolu MLXAppleOCR_recognize (zaktualizuj libMLXBridge.dylib)")?,
            lib.get(b"MLXAppleOCR_freeString\0")
                .context("Brak symbolu MLXAppleOCR_freeString")?,
        )
    };
    Ok(Bridge {
        _lib: Some(lib),
        recognize: *rec,
        free_string: *fs,
    })
}

// iOS: brak libMLXBridge.dylib. Symbole Vision sa wkompilowane w binarke
// aplikacji (AppleOcrEngine.swift), a Swift przekazuje ich wskazniki przez
// `tentaflow_register_apple_ocr` przy starcie (AppDelegate).
#[cfg(target_os = "ios")]
fn open_bridge() -> Result<Bridge> {
    let reg = APPLE_OCR_REG.get().context(
        "Apple OCR nie zarejestrowany ze Swift — tentaflow_register_apple_ocr \
         nie zostalo wywolane przy starcie aplikacji",
    )?;
    Ok(Bridge {
        _lib: None,
        recognize: reg.recognize,
        free_string: reg.free_string,
    })
}

#[cfg(target_os = "ios")]
struct AppleOcrRegistration {
    recognize: RecognizeFn,
    free_string: FreeStringFn,
}

#[cfg(target_os = "ios")]
unsafe impl Send for AppleOcrRegistration {}
#[cfg(target_os = "ios")]
unsafe impl Sync for AppleOcrRegistration {}

#[cfg(target_os = "ios")]
static APPLE_OCR_REG: std::sync::OnceLock<AppleOcrRegistration> = std::sync::OnceLock::new();

/// Rejestruje wskazniki cdecl Apple OCR ze strony Swift (iOS). Wywolywane raz
/// z AppDelegate przed `tentaflow_mobile_start()`. Mirror
/// `tentaflow_register_apple_tts`.
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn tentaflow_register_apple_ocr(
    recognize: RecognizeFn,
    free_string: FreeStringFn,
) {
    let _ = APPLE_OCR_REG.set(AppleOcrRegistration {
        recognize,
        free_string,
    });
    tracing::info!("[apple-ocr] Swift callbacks zarejestrowane");
}

/// Ksztalt JSON zwracanego przez `MLXAppleOCR_recognize`. Czytamy `text`; `lines`
/// (boxy/pewnosc) sa dostepne dla bogatszych konsumentow, ale `OcrRunner::read`
/// zwraca tylko polaczony tekst.
#[derive(Debug, Deserialize)]
struct OcrJson {
    text: String,
}

/// Domyslne jezyki rozpoznawania. Vision sam dobiera model per-jezyk; podajemy
/// pl+en bo to typowy zestaw deploymentu. Korekta jezykowa wlaczona — lepsza dla
/// prozy/dokumentow (glowny przypadek apple-ocr to OCR dokumentow, nie tablic).
const DEFAULT_LANGUAGES: &str = "pl-PL,en-US";
const DEFAULT_USE_LANGUAGE_CORRECTION: bool = true;

pub struct AppleOcrEngine {
    bridge: Mutex<Option<Bridge>>,
    languages: String,
    use_language_correction: bool,
}

impl AppleOcrEngine {
    pub fn new() -> Self {
        Self {
            bridge: Mutex::new(None),
            languages: DEFAULT_LANGUAGES.to_string(),
            use_language_correction: DEFAULT_USE_LANGUAGE_CORRECTION,
        }
    }

    /// Inicjalizuje most (dlopen). Wolane raz przy deployu, zeby brak
    /// libMLXBridge.dylib zglosil sie od razu, a nie przy pierwszym frame.
    pub fn ensure_bridge(&self) -> Result<()> {
        let mut g = self.bridge.lock().unwrap();
        if g.is_none() {
            *g = Some(open_bridge()?);
        }
        Ok(())
    }

    /// Surowy odczyt: enkoduje crop RGB24 do PNG i woła Vision. Wspoldzielony
    /// przez `OcrRunner::read` (camera crop) i deploy smoke.
    fn recognize_rgb(&self, rgb: &[u8], width: u32, height: u32) -> Result<Option<String>> {
        self.ensure_bridge()?;
        let png = encode_png(rgb, width, height)?;
        let (recognize, free_string) = {
            let g = self.bridge.lock().unwrap();
            let b = g.as_ref().expect("bridge ensured");
            (b.recognize, b.free_string)
        };
        let c_langs = CString::new(self.languages.clone())?;
        let mut out_len: i32 = 0;
        let ptr = unsafe {
            recognize(
                png.as_ptr(),
                png.len() as i32,
                c_langs.as_ptr(),
                self.use_language_correction as i32,
                &mut out_len as *mut _,
            )
        };
        if ptr.is_null() {
            return Ok(None);
        }
        let json = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { free_string(ptr) };

        let parsed: OcrJson =
            serde_json::from_str(&json).context("parse Apple OCR JSON")?;
        let text = parsed.text.trim().to_string();
        if text.is_empty() {
            Ok(None)
        } else {
            Ok(Some(text))
        }
    }
}

impl Default for AppleOcrEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrRunner for AppleOcrEngine {
    fn read(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Option<String>> {
        self.recognize_rgb(crop_rgb, cw, ch)
    }
}

/// Enkoduje tightly-packed RGB24 (`width*height*3`) do PNG w pamieci. Vision
/// (CGImageSource) dekoduje formaty obrazow, nie surowe bufory pikseli — wiec
/// musimy zakodowac crop, zanim przekazemy bajty do FFI.
fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let expected = width as usize * height as usize * 3;
    if rgb.len() < expected {
        anyhow::bail!(
            "apple-ocr: bufor RGB za maly ({} < {}x{}x3={})",
            rgb.len(),
            width,
            height,
            expected
        );
    }
    let img: image::RgbImage =
        image::ImageBuffer::from_raw(width, height, rgb[..expected].to_vec())
            .context("apple-ocr: budowa RgbImage z bufora")?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .context("apple-ocr: enkodowanie PNG")?;
    Ok(out.into_inner())
}

/// Laduje silnik OCR i rejestruje go jako globalny in-process runner przez
/// `super::set_ocr_runner`. Wolane przez deploy embedded (`apple-ocr`). Brak
/// libMLXBridge.dylib zglasza blad od razu (przed oznaczeniem RUNNING).
pub fn register_as_ocr_runner() -> Result<()> {
    let engine = AppleOcrEngine::new();
    engine
        .ensure_bridge()
        .context("init apple-ocr (brak libMLXBridge.dylib?)")?;
    super::set_ocr_runner(std::sync::Arc::new(engine));
    info!("[apple-ocr] zarejestrowany jako in-process OCR runner");
    Ok(())
}

/// Wyrejestrowuje silnik (rollback / stop service).
pub fn unregister_as_ocr_runner() {
    super::clear_ocr_runner();
}
