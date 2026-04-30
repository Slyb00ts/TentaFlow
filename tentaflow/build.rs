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
    set_linux_rpath();
    copy_versioned_shared_libs_linux();
    build_mlx_bridge();
    build_kokoro_bridge();
    build_meeting_bot();
}

// ----- Linux linker flags ----------------------------------------------------
// 1. Rpath $ORIGIN: sherpa-rs kopiuje libsherpa-onnx-c-api.so +
//    libonnxruntime.so do target/<profile>/ przy pierwszym buildzie. Bez
//    ustawionego rpath binarka szuka tych libsow tylko w systemowych sciezkach
//    (/usr/lib, LD_LIBRARY_PATH) i pada z "error while loading shared
//    libraries". Rpath $ORIGIN mowi linkerowi: szukaj obok exe. macOS uzywa
//    @loader_path (ustawione w build_mlx_bridge).
// 2. --allow-multiple-definition: whisper-rs (whisper-rs-sys) i llama-cpp-2
//    (llama-cpp-sys-2) OBIE staty­cznie linkuja wlasna kopie ggml-quants.c
//    (whisper.cpp i llama.cpp uzywaja tego samego ggml runtime'u). Linker
//    GNU ld widzi te same symbole `quantize_*`, `ggml_validate_row_data` itd.
//    w obu rlibach i wykrzykuje "multiple definition". Funkcje sa bit-by-bit
//    identyczne (te same tagi wersji ggml), wiec --allow-multiple-definition
//    ka linkerowi wybrac pierwsza i ignorowac kolejne. Komentarz w
//    tentaflow-core/Cargo.toml:11-14 ostrzegal o tym konflikcie — alternatywa
//    bylaby wykluczenie inference-whisper przy gpu-cuda/vulkan/rocm, ale
//    user moze potrzebowac obu jednoczesnie (LLM + STT lokalnie).
fn set_linux_rpath() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
}

// llama-cpp-sys-2 build.rs:124 ma glob "*.so" ktory matchuje tylko symlinki bez
// wersji (libllama.so), ale binarka kompiluje sie z SONAME libllama.so.0 i tego
// szuka w runtime. Dociagamy wersjonowane pliki sami, dopoki upstream nie
// naprawi tego globa. Dotyczy buildow z `dynamic-link` (np. gpu-cuda na CUDA 13,
// gdzie statyczne cublas_static.a nie istnieje).
//
// Cargo nie gwarantuje ze llama-cpp-sys-2 cmake build skonczy sie przed naszym
// build.rs (build skrypty roznych krat moga sie nakladac z ich kompilacjami),
// wiec pollujemy az versioned libe sie pojawia. `cargo:rerun-if-changed` na
// out/lib zapewnia ze cargo invaliduje nasz cache gdy llama sie przebuduje.
fn copy_versioned_shared_libs_linux() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => return,
    };
    let target_dir = match out_dir.ancestors().nth(3) {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    let build_dir = target_dir.join("build");
    println!("cargo:rerun-if-changed={}", build_dir.display());

    let lib_dirs = find_llama_lib_dirs(&build_dir);
    if lib_dirs.is_empty() {
        return;
    }
    for lib_dir in &lib_dirs {
        println!("cargo:rerun-if-changed={}", lib_dir.display());
        // Safety net: dorzuc out/lib jako rpath linker arg. Gdy polling
        // ponizej nie zdazy (cmake build llama+CUDA potrafi trwac 13+ min),
        // binarka i tak znajdzie versioned .so bezposrednio w build dir.
        // Sciezka jest absolutna i hash-zalezna, wiec nie nadaje sie do
        // deploymentu — tylko fallback dla local dev workflow.
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }

    // Polling do 20 min (cmake llama+CUDA build moze trwac ~13 min na NGC).
    // Build.rs i tak musi czekac az llama-cpp-sys-2 dostarczy symbole, wiec
    // ten wait nie blokuje zadnej rownoleglej pracy cargo.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1200);
    let mut copied = 0usize;
    loop {
        copied = 0;
        for lib_dir in &lib_dirs {
            copied += copy_versioned_from(lib_dir, &target_dir);
        }
        if copied > 0 || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    if copied == 0 {
        println!(
            "cargo:warning=tentaflow: nie skopiowano versioned .so w 20 min — fallback rpath \
             na out/lib aktywny, ale binarka przeniesiona w inne miejsce nie zadziala."
        );
    }
}

fn find_llama_lib_dirs(build_dir: &std::path::Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(build_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("llama-cpp-sys-2-") {
            continue;
        }
        result.push(entry.path().join("out").join("lib"));
    }
    result
}

fn copy_versioned_from(lib_dir: &std::path::Path, target_dir: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(lib_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let lib_name = entry.file_name();
        if !lib_name.to_string_lossy().contains(".so.") {
            continue;
        }
        let dst = target_dir.join(&lib_name);
        let src = entry.path();
        let _ = std::fs::remove_file(&dst);
        if std::fs::hard_link(&src, &dst).is_err() && std::fs::copy(&src, &dst).is_err() {
            continue;
        }
        // Bez RPATH na samych .so loader szuka ich tranzytywnych zaleznosci
        // (libllama → libggml.so.0) w systemowych sciezkach i pada. Ustawiamy
        // $ORIGIN zeby kazdy .so szukal swoich deps obok siebie.
        let _ = Command::new("patchelf")
            .args(["--set-rpath", "$ORIGIN"])
            .arg(&dst)
            .status();
        count += 1;
    }
    count
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
        println!("cargo:warning=tentaflow: xcodebuild nieudane — Swift MLX bridge nie zbudowany");
        return;
    }

    let products = xcode_build_dir.join("Build/Products/Release");
    let framework_binary =
        products.join("PackageFrameworks/MLXBridge.framework/Versions/A/MLXBridge");
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
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", target_dir.display());
}

// ----- Kokoro Swift bridge (macOS only) --------------------------------------
//
// Buduje libKokoroBridge.dylib (Kokoro 82M MLX TTS — niezalezny od MLXBridge
// bo wymaga nowszego mlx-swift). Identyczny przeplyw co MLXBridge: xcodebuild
// → kopia dylib + Cmlx bundle → install_name_tool. Bundle Cmlx jest WSPOLNY
// (ten sam mlx-swift), wiec nie nadpisujemy go jezeli juz istnieje.
fn build_kokoro_bridge() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package_dir = manifest_dir
        .parent()
        .expect("tentaflow-core/.. musi istniec")
        .join("tentaflow-desktop/macos/swift/KokoroBridge");
    let package_swift = package_dir.join("Package.swift");
    if !package_swift.exists() {
        println!(
            "cargo:warning=tentaflow: brak {}, omijam KokoroBridge",
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
            "KokoroBridge",
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
        println!("cargo:warning=tentaflow: xcodebuild KokoroBridge nieudane");
        return;
    }
    let products = xcode_build_dir.join("Build/Products/Release");
    let framework_binary =
        products.join("PackageFrameworks/KokoroBridge.framework/Versions/A/KokoroBridge");
    if !framework_binary.exists() {
        println!(
            "cargo:warning=tentaflow: brak {}",
            framework_binary.display()
        );
        return;
    }
    let target_dir = cargo_target_dir();
    let dylib_dest = target_dir.join("libKokoroBridge.dylib");
    if let Err(e) = std::fs::copy(&framework_binary, &dylib_dest) {
        println!("cargo:warning=tentaflow: copy KokoroBridge dylib: {}", e);
        return;
    }
    let _ = Command::new("install_name_tool")
        .args(["-id", "@rpath/libKokoroBridge.dylib"])
        .arg(&dylib_dest)
        .status();
    println!(
        "cargo:warning=tentaflow: KokoroBridge gotowy ({})",
        dylib_dest.display()
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
    let target_dir = cargo_target_dir();
    let bin_name = if cfg!(target_os = "windows") {
        "tentaflow-meeting.exe"
    } else {
        "tentaflow-meeting"
    };
    let dest_bin = target_dir.join(bin_name);

    // Wymus rerun gdy dest_bin znika (np. po `cargo clean` parent crate'u
    // ale child target/ zostal). Bez tego cargo skipowal build.rs na podstawie
    // rerun-if-changed na bot_dir/src — zmiany src nie bylo, wiec build.rs
    // nie odpalal sie i tentaflow-meeting NIE byl kopiowany do
    // tentaflow/target/release/. Skutek: "Failed to start bot: brak binarki
    // tentaflow-meeting obok tentaflow ani w PATH".
    println!("cargo:rerun-if-changed={}", dest_bin.display());
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

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
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
