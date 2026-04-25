// =============================================================================
// Plik: mlx_swift_init.rs
// Opis: Bootstrap libMLXBridge.dylib przy starcie tentaflow binary (macOS).
//       Po pomyslnej rejestracji `MlxSwiftEngine` przejmuje rolę domyslnego
//       backendu MLX (mlx-models pozostaje fallbackiem gdy bridge niedostepny).
// =============================================================================

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::PathBuf;

use anyhow::{Context, Result};
use libloading::{Library, Symbol};
use tracing::{info, warn};

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

unsafe extern "C" {
    fn tentaflow_register_mlx_swift(
        load_fn: LoadModelFn,
        unload_fn: UnloadModelFn,
        generate_fn: GenerateFn,
        model_info_fn: ModelInfoFn,
        context: *mut c_void,
    );
}

fn locate_dylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let candidates = [
        exe_dir.join("libMLXBridge.dylib"),
        // Reuzywany Swift Package z tentaflow-desktop/macos.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .join("tentaflow-desktop/macos/swift/MLXBridge/.build/arm64-apple-macosx/release/libMLXBridge.dylib"),
        exe_dir.join("../Frameworks/libMLXBridge.dylib"),
    ];

    candidates.into_iter().find(|p| p.exists())
}

/// Laduje bridge dylib i rejestruje callbacki w tentaflow-core. Wolane przy
/// starcie main(). Bledy logowane jako warn — fallback na inne backendy.
pub fn init() -> Result<()> {
    let dylib_path = locate_dylib()
        .context("Nie znaleziono libMLXBridge.dylib (build.rs powinien go skopiowac do target/release/)")?;

    info!("[mlx-swift] Ladowanie {}", dylib_path.display());

    // SAFETY: trusted dylib z naszego repozytorium.
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
                .context("Brak symbolu MLXBridge_loadModel")?,
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
