// =============================================================================
// Plik: macos_ffi.rs
// Opis: Wspoldzielone helpery do dlopen libMLXBridge.dylib przez wszystkie
//       silniki opierajace sie na Swift bridge'u (Whisper, Apple TTS, Kokoro
//       MLX). Wczesniej kazdy modul mial wlasna kopie locate_dylib +
//       ensure_metallib_next_to; tu unifikujemy zeby unikac driftu.
// =============================================================================

#![cfg(any(
    target_os = "macos",
    target_os = "ios",
    feature = "inference-mlx-whisper",
    feature = "inference-mlx-kokoro"
))]

use std::path::{Path, PathBuf};
use tracing::info;

/// Sprawdza czy obok dylibu jest skompilowany metallib (wymagany przez Cmlx).
fn dylib_has_metallib(p: &Path) -> bool {
    let dir = match p.parent() {
        Some(d) => d,
        None => return false,
    };
    dir.join("mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib")
        .exists()
        || dir.join("mlx.metallib").exists()
        || dir.join("Resources/mlx.metallib").exists()
}

/// Lokalizuje libMLXBridge.dylib w typowych miejscach: obok exe, w
/// ../Frameworks/, w `target/{debug,release}/` poszczegolnych crate'ow,
/// oraz w SwiftPM/.build. Preferuje lokalizacje "kompletna" (z metallibem).
pub fn locate_mlx_bridge_dylib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?.to_path_buf();
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(exe_dir.join("libMLXBridge.dylib"));
    candidates.push(exe_dir.join("../Frameworks/libMLXBridge.dylib"));

    let mut current = exe_dir.clone();
    for _ in 0..6 {
        for sub in [
            "tentaflow/target/release/libMLXBridge.dylib",
            "tentaflow/target/debug/libMLXBridge.dylib",
            "tentaflow-desktop/target/release/libMLXBridge.dylib",
            "tentaflow-desktop/target/debug/libMLXBridge.dylib",
            "tentaflow-desktop/macos/swift/MLXBridge/.build/arm64-apple-macosx/release/libMLXBridge.dylib",
        ] {
            candidates.push(current.join(sub));
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    if let Some(found) = candidates
        .iter()
        .find(|p| p.exists() && dylib_has_metallib(p))
    {
        return Some(found.clone());
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Wariant dla KokoroBridge — zrodlo metallibu w `KokoroBridge/.build/...`.
/// Kopiuje `default.metallib` jako `{dylib_dir}/mlx.metallib` (Cmlx fallback path 1).
pub fn ensure_kokoro_metallib_next_to(dylib: &Path) {
    let dir = match dylib.parent() {
        Some(d) => d,
        None => return,
    };
    let target = dir.join("mlx.metallib");
    if target.exists() {
        return;
    }
    let bundle_subpath = "mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib";
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest.clone());
    let candidates = [
        dir.join(bundle_subpath),
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/KokoroBridge/.build/arm64-apple-macosx/release/{}",
            bundle_subpath
        )),
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/KokoroBridge/build-xcode/Build/Products/Release/{}",
            bundle_subpath
        )),
        // Fallback: probuj MLXBridge'owy bundle (te same Metal kernels).
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/MLXBridge/.build/arm64-apple-macosx/release/{}",
            bundle_subpath
        )),
    ];
    for src in &candidates {
        if src.exists() {
            if let Err(e) = std::fs::copy(src, &target) {
                tracing::warn!(
                    "[macos_ffi-kokoro] copy {} -> {}: {}",
                    src.display(),
                    target.display(),
                    e
                );
            } else {
                info!(
                    "[macos_ffi-kokoro] mlx.metallib zainstalowane: {}",
                    target.display()
                );
            }
            return;
        }
    }
    tracing::warn!("[macos_ffi-kokoro] nie znaleziono default.metallib dla KokoroBridge");
}

/// Cmlx (Metal kernels mlx-swift) szuka metallibu w 3 miejscach. Kopiujemy
/// `default.metallib` jako `{dylib_dir}/mlx.metallib`. Idempotentne — short
/// circuit jak target juz istnieje. Zrodlo to skompilowany SwiftPM bundle
/// z `.build/release/` lub Xcode `build/Release/`.
pub fn ensure_mlx_metallib_next_to(dylib: &Path) {
    let dir = match dylib.parent() {
        Some(d) => d,
        None => return,
    };
    let target = dir.join("mlx.metallib");
    if target.exists() {
        return;
    }
    let bundle_subpath = "mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib";
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest.clone());
    let candidates = [
        dir.join(bundle_subpath),
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/MLXBridge/.build/arm64-apple-macosx/release/{}",
            bundle_subpath
        )),
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/MLXBridge/.build/arm64-apple-macosx/debug/{}",
            bundle_subpath
        )),
        workspace_root.join(format!(
            "tentaflow-desktop/macos/swift/MLXBridge/build-xcode/Build/Products/Release/{}",
            bundle_subpath
        )),
    ];
    for src in &candidates {
        if src.exists() {
            if let Err(e) = std::fs::copy(src, &target) {
                tracing::warn!(
                    "[macos_ffi] copy {} -> {}: {}",
                    src.display(),
                    target.display(),
                    e
                );
            } else {
                info!(
                    "[macos_ffi] mlx.metallib zainstalowane: {}",
                    target.display()
                );
            }
            return;
        }
    }
    tracing::warn!("[macos_ffi] nie znaleziono default.metallib w workspace");
}
