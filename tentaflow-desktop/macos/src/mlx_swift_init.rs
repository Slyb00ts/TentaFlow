// =============================================================================
// Plik: mlx_swift_init.rs
// Opis: Bootstrap MLXBridge.dylib — laduje libMLXBridge.dylib przy starcie
//       desktop bin, wyciaga symbole C-ABI (MLXBridge_loadModel, ...) i
//       rejestruje je w tentaflow_register_mlx_swift z tentaflow-core.
//
//       Po pomyslnej rejestracji `MlxSwiftEngine::is_available()` zwraca
//       true i kazde load_model / generate na MLX silniku idzie przez
//       Swift MLXLLM (proven path z iOS gdzie Bielik 4-bit gada bez bełkotu)
//       zamiast przez broken `mlx-models` (Rust crate).
// =============================================================================

#![cfg(all(target_os = "macos", feature = "mlx-swift-bridge"))]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::PathBuf;

use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use tracing::{info, warn};

/// Sygnatury Swift @_cdecl funkcji — musza pasowac do MLXBridge.swift.
type LoadModelFn = unsafe extern "C" fn(*const c_char, *mut c_void) -> i32;
type UnloadModelFn = unsafe extern "C" fn(*mut c_void);
type GenerateFn = unsafe extern "C" fn(
    prompt: *const c_char,
    max_tokens: i32,
    temperature: f32,
    top_p: f32,
    token_callback: extern "C" fn(*const c_char, bool, *mut c_void),
    callback_context: *mut c_void,
    context: *mut c_void,
) -> i32;
type ModelInfoFn = unsafe extern "C" fn(*mut c_void) -> *mut c_char;
type GetContextFn = unsafe extern "C" fn() -> *mut c_void;

// tentaflow-core eksportuje to po wlaczeniu feature inference-mlx.
unsafe extern "C" {
    fn tentaflow_register_mlx_swift(
        load_fn: LoadModelFn,
        unload_fn: UnloadModelFn,
        generate_fn: GenerateFn,
        model_info_fn: ModelInfoFn,
        context: *mut c_void,
    );
}

/// Lokalizuje libMLXBridge.dylib w typowych miejscach. Zwraca pierwsza
/// istniejaca lokalizacje albo None gdy nie znaleziono — wtedy startup
/// kontynuuje bez Swift bridge (degradacja do mlx-models / CPU).
fn locate_dylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let candidates = [
        // Obok wynikowego binarki — kopiuje tam build.rs.
        exe_dir.join("libMLXBridge.dylib"),
        // .build SwiftPM (gdy ktos buduje recznie).
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("swift/MLXBridge/.build/arm64-apple-macosx/release/libMLXBridge.dylib"),
        // Zainstalowany w Frameworks/ bundle (release packaging).
        exe_dir.join("../Frameworks/libMLXBridge.dylib"),
    ];

    candidates.into_iter().find(|p| p.exists())
}

/// Laduje libMLXBridge.dylib i rejestruje 4 callbacki w tentaflow-core.
/// Wolane raz przy starcie desktop bin. Jezeli zwraca Err, MlxSwiftEngine
/// nie bedzie dzialal — fallback na inne backendy (llama.cpp, CPU).
pub fn init() -> Result<()> {
    let dylib_path = locate_dylib().context(
        "Nie znaleziono libMLXBridge.dylib — sprawdz czy `cargo build` wywolal swift build, \
         albo skopiuj dylib obok binarki tentaflow-desktop.",
    )?;

    info!("[mlx-swift] Ladowanie {}", dylib_path.display());

    // SAFETY: Library::new dla zaufanego dylib zbudowanego z kodu w repozytorium.
    // Library `leak`-uje (Box::leak na Library) bo wszystkie function pointers
    // musza zyc do konca aplikacji.
    let lib = unsafe { Library::new(&dylib_path) }
        .with_context(|| format!("dlopen {} nieudane", dylib_path.display()))?;
    let lib: &'static Library = Box::leak(Box::new(lib));

    let (load_fn, unload_fn, generate_fn, model_info_fn, get_context_fn): (
        Symbol<'static, LoadModelFn>,
        Symbol<'static, UnloadModelFn>,
        Symbol<'static, GenerateFn>,
        Symbol<'static, ModelInfoFn>,
        Symbol<'static, GetContextFn>,
    ) = unsafe {
        (
            lib.get(b"MLXBridge_loadModel\0")
                .context("Brak symbolu MLXBridge_loadModel w dylib")?,
            lib.get(b"MLXBridge_unloadModel\0")
                .context("Brak symbolu MLXBridge_unloadModel")?,
            lib.get(b"MLXBridge_generate\0")
                .context("Brak symbolu MLXBridge_generate")?,
            lib.get(b"MLXBridge_modelInfo\0")
                .context("Brak symbolu MLXBridge_modelInfo")?,
            lib.get(b"MLXBridge_getContext\0")
                .context("Brak symbolu MLXBridge_getContext")?,
        )
    };

    // SAFETY: Wskazniki funkcji `Symbol` zostaja przypisane do `'static Library`
    // — pointer pozostaje wazny do konca programu.
    let context = unsafe { get_context_fn() };
    if context.is_null() {
        warn!("[mlx-swift] MLXBridge_getContext zwrocil NULL");
        anyhow::bail!("Swift singleton context jest NULL");
    }

    unsafe {
        tentaflow_register_mlx_swift(*load_fn, *unload_fn, *generate_fn, *model_info_fn, context);
    }

    info!("[mlx-swift] Bridge zarejestrowany — MLX inferencja idzie przez mlx-swift");
    Ok(())
}
