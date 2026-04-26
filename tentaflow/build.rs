// =============================================================================
// Plik: build.rs
// Opis: Buduje skladniki natywne dystrybuowane razem z `tentaflow`:
//        1. macOS: swift/MLXBridge przez `xcodebuild` (Metal shadery → metallib)
//        2. Wszystkie platformy: tentaflow-meeting (sidecar Teams) z
//           `tentaflow-containers/agents/native/teams-bot/`. Binarka laduje
//           obok `tentaflow` w target/<profile>/, deploy.native runtime=binary
//           jej szuka tam.
// =============================================================================

use std::path::PathBuf;
use std::process::Command;

fn main() {
    build_mlx_bridge();
    build_meeting_bot();
}

// ----- MLX Swift bridge (macOS only) -----------------------------------------
fn build_mlx_bridge() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package_dir = manifest_dir
        .parent()
        .expect("tentaflow-core/.. musi istniec")
        .join("tentaflow-desktop/macos/swift/MLXBridge");
    let package_swift = package_dir.join("Package.swift");

    if !package_swift.exists() {
        println!(
            "cargo:warning=tentaflow: brak {}, omijam Swift bridge build",
            package_swift.display()
        );
        return;
    }

    println!(
        "cargo:rerun-if-changed={}/Package.swift",
        package_dir.display()
    );
    println!("cargo:rerun-if-changed={}/Sources", package_dir.display());

    let cargo_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let xcode_arch = match cargo_arch.as_str() {
        "aarch64" => "arm64",
        other => other,
    };

    let xcode_build_dir = package_dir.join("build-xcode");
    let xcode_status = Command::new("xcodebuild")
        .args([
            "-scheme",
            "MLXBridge",
            "-destination",
            &format!("platform=macOS,arch={}", xcode_arch),
            "-configuration",
            "Release",
            "-derivedDataPath",
        ])
        .arg(&xcode_build_dir)
        .arg("build")
        .current_dir(&package_dir)
        .status();
    if !matches!(xcode_status, Ok(s) if s.success()) {
        println!(
            "cargo:warning=tentaflow: xcodebuild nieudane — Swift MLX bridge nie zbudowany"
        );
        return;
    }

    let products = xcode_build_dir.join("Build/Products/Release");
    let framework_binary = products.join("PackageFrameworks/MLXBridge.framework/Versions/A/MLXBridge");
    let metallib_bundle = products.join("mlx-swift_Cmlx.bundle");

    if !framework_binary.exists() {
        println!(
            "cargo:warning=tentaflow: xcodebuild OK ale brak {}",
            framework_binary.display()
        );
        return;
    }
    if !metallib_bundle.exists() {
        println!(
            "cargo:warning=tentaflow: brak {} — bez metallib MLX nie zadziala",
            metallib_bundle.display()
        );
        return;
    }

    let target_dir = cargo_target_dir();
    let dylib_dest = target_dir.join("libMLXBridge.dylib");
    if let Err(e) = std::fs::copy(&framework_binary, &dylib_dest) {
        println!("cargo:warning=tentaflow: copy dylib nieudane: {}", e);
        return;
    }

    let _ = Command::new("install_name_tool")
        .args(["-id", "@rpath/libMLXBridge.dylib"])
        .arg(&dylib_dest)
        .status();

    let bundle_dest = target_dir.join("mlx-swift_Cmlx.bundle");
    let _ = std::fs::remove_dir_all(&bundle_dest);
    if let Err(e) = copy_dir_recursive(&metallib_bundle, &bundle_dest) {
        println!(
            "cargo:warning=tentaflow: copy mlx-swift_Cmlx.bundle nieudane: {}",
            e
        );
        return;
    }

    println!(
        "cargo:warning=tentaflow: MLXBridge gotowy ({} + bundle)",
        dylib_dest.display()
    );

    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        target_dir.display()
    );
}

// ----- Meeting bot (all platforms) -------------------------------------------
//
// Buduje binarke `tentaflow-meeting` z `tentaflow-containers/agents/native/teams-bot/`
// i kopiuje obok glownej binarki tentaflow. Dzieki temu deploy.native runtime=binary
// znajduje gotowa binarke przy starcie sesji bota — bez osobnego cargo build.
//
// Inner cargo uzywa wlasnego target dir w `<bot_dir>/target/`, zeby nie kolidowac
// z lockiem `tentaflow/target/`.
fn build_meeting_bot() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bot_dir = manifest_dir
        .parent()
        .expect("tentaflow/.. musi istniec")
        .join("tentaflow-containers/agents/native/teams-bot");
    let bot_manifest = bot_dir.join("Cargo.toml");

    if !bot_manifest.exists() {
        println!(
            "cargo:warning=tentaflow: brak {}, pomijam build meeting-bot",
            bot_manifest.display()
        );
        return;
    }

    println!("cargo:rerun-if-changed={}/Cargo.toml", bot_dir.display());
    println!("cargo:rerun-if-changed={}/src", bot_dir.display());

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let mut cmd = Command::new(env!("CARGO"));
    cmd.arg("build")
        .arg("--bin")
        .arg("tentaflow-meeting")
        .arg("--manifest-path")
        .arg(&bot_manifest);
    if profile == "release" {
        cmd.arg("--release");
    }
    // Wycisz CARGO env zarazone przez parent build (RUSTFLAGS, TARGET, itd. moga
    // wymusic re-budowanie wszystkich deps z dziwnymi flagami).
    cmd.env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");

    let status = cmd.status();
    let bin_name = if cfg!(target_os = "windows") {
        "tentaflow-meeting.exe"
    } else {
        "tentaflow-meeting"
    };
    if !matches!(status, Ok(s) if s.success()) {
        println!(
            "cargo:warning=tentaflow: cargo build tentaflow-meeting nieudane — bot native nie bedzie dzialal"
        );
        return;
    }

    let inner_target = bot_dir.join("target").join(&profile);
    let src_bin = inner_target.join(bin_name);
    if !src_bin.exists() {
        println!(
            "cargo:warning=tentaflow: tentaflow-meeting zbudowany ale brak {} — sprawdz cargo build output",
            src_bin.display()
        );
        return;
    }

    let target_dir = cargo_target_dir();
    let dest_bin = target_dir.join(bin_name);
    if let Err(e) = std::fs::copy(&src_bin, &dest_bin) {
        println!(
            "cargo:warning=tentaflow: copy {} -> {} nieudane: {}",
            src_bin.display(),
            dest_bin.display(),
            e
        );
        return;
    }

    // Kopiuj model Silero VAD obok binarki — bot na native szuka go w
    // `current_exe()/silero_vad.onnx` jako fallback do `/opt/models/...`.
    let vad_src = bot_dir.join("models").join("silero_vad.onnx");
    if vad_src.exists() {
        let vad_dest = target_dir.join("silero_vad.onnx");
        if let Err(e) = std::fs::copy(&vad_src, &vad_dest) {
            println!(
                "cargo:warning=tentaflow: copy silero_vad.onnx nieudane: {}",
                e
            );
        }
    } else {
        // Brak modelu w repo — bot przejdzie na fallback RMS, builder dostaje warning
        // raz, zeby nie spamowac na kazdym buildzie nie-Linux.
        println!(
            "cargo:warning=tentaflow: brak {} — bot uzyje fallback RMS dla VAD (gorsza jakosc)",
            vad_src.display()
        );
    }

    println!(
        "cargo:warning=tentaflow: tentaflow-meeting gotowy ({})",
        dest_bin.display()
    );
}

// `OUT_DIR` to `target/<profile>/build/<crate>-<hash>/out`. Czwarty przodek to
// `target/<profile>/`, gdzie laduje binarka glowna.
fn cargo_target_dir() -> PathBuf {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR struktura niespodziewana")
        .to_path_buf()
}

fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest)?;
        } else if path.is_symlink() {
            let target = std::fs::read_link(&path)?;
            let _ = std::fs::remove_file(&dest);
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &dest)?;
            #[cfg(windows)]
            {
                let _ = target;
                std::fs::copy(&path, &dest)?;
            }
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}
