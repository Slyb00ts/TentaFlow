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
    copy_native_dynamic_libs();
    copy_isolated_whisper_dylib();
    copy_zvec_dylib();
    copy_versioned_shared_libs_linux();
    build_mlx_bridge();
    build_kokoro_bridge();
    build_meeting_bot();
}

// Kopiuje aktualny vendored libzvec_c_api (.so/.dylib) PLASKO do target/<profile>,
// nadpisując poprzednią kopię. Powód: deploy uruchamia binarkę z
// `LD_LIBRARY_PATH=target/<profile>`, który MA pierwszeństwo nad rpath do vendora.
// Gdy `build-zvec.sh` przebuduje vendored .so (np. po dodaniu izolacji symboli
// protobuf), stara kopia w target shadow'owała świeży vendored → binarka ładowała
// nieaktualną bibliotekę (crash `corrupted size` na Linuxie z dwiema kopiami protobuf).
// Pełna kopia bajtów (nie hardlink) + rerun-if-changed gwarantuje świeżość.
fn copy_zvec_dylib() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let (platform, libname) = match target.as_str() {
        "linux" => ("linux-x86_64", "libzvec_c_api.so"),
        "macos" => ("macos-arm64", "libzvec_c_api.dylib"),
        _ => return,
    };
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let src = manifest
        .join("..")
        .join("tentaflow-zvec-sys")
        .join("vendor")
        .join("lib")
        .join(platform)
        .join(libname);
    if !src.exists() {
        return;
    }
    println!("cargo:rerun-if-changed={}", src.display());

    let out_dir = match std::env::var("OUT_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => return,
    };
    let Some(target_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let dst = target_dir.join(libname);
    let _ = std::fs::remove_file(&dst);
    if let Err(e) = std::fs::copy(&src, &dst) {
        println!(
            "cargo:warning=tentaflow: kopia zvec {} -> {} nieudana: {}",
            src.display(),
            dst.display(),
            e
        );
    }
}

// ----- Linux linker flags ----------------------------------------------------
// Rpath $ORIGIN: sherpa-rs (libsherpa-onnx-c-api.so + libonnxruntime.so) oraz
// izolowany whisper (libwhisper_tf.so) sa kopiowane do target/<profile>/ przy
// buildzie. Bez rpath binarka szukalaby ich tylko w systemowych sciezkach
// (/usr/lib, LD_LIBRARY_PATH) i padla z "error while loading shared libraries".
// $ORIGIN = szukaj obok exe. macOS uzywa @loader_path (build_mlx_bridge).
//
// whisper.cpp i llama.cpp NIE dziela juz statycznego ggml: whisper jest
// izolowanym dylibem z prywatnym (ukrytym) ggml (patrz build-whisper-cpp.sh +
// whisper-rs-sys/build.rs), wiec znikla potrzeba `--allow-multiple-definition`
// (Linux) / `/FORCE:MULTIPLE` (MSVC) — symbole ggml_* whispera nie trafiaja juz
// do glownego linku binarki.
fn set_linux_rpath() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
            add_native_dynamic_rpath();
        }
        "macos" => {
            // Na macOS dylib zvec ma install name `@rpath/libzvec_c_api.dylib`,
            // wiec binarka potrzebuje absolutnego rpath do zwendorowanego katalogu
            // (analogicznie jak Linux). Rpath @loader_path/target_dir ustawia
            // build_mlx_bridge — tu chodzi tylko o vendor zvec.
            add_native_dynamic_rpath();
        }
        _ => {}
    }
}

fn add_native_dynamic_rpath() {
    let platform = match native_platform() {
        Some(v) => v,
        None => return,
    };
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = manifest
        .join("../native-libs")
        .join(platform)
        .join("lib-dynamic");
    if let Ok(abs) = lib_dir.canonicalize() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", abs.display());
    }
}

// Kopiuje dynamiczne biblioteki przygotowane przez scripts/native-libs/build-all.*
// obok binarki. Biblioteki z DT_NEEDED muszą istnieć przed startem procesu, więc
// build.rs jest właściwym miejscem na ten krok dla lokalnych i release buildów.
fn copy_native_dynamic_libs() {
    let platform = match native_platform() {
        Some(v) => v,
        None => return,
    };
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dynamic_dir = manifest
        .join("../native-libs")
        .join(platform)
        .join("lib-dynamic");
    println!("cargo:rerun-if-changed={}", dynamic_dir.display());
    if !dynamic_dir.exists() {
        return;
    }

    let target_dir = cargo_target_dir();
    let entries = match std::fs::read_dir(&dynamic_dir) {
        Ok(entries) => entries,
        Err(e) => {
            println!(
                "cargo:warning=tentaflow: nie moge odczytac {}: {}",
                dynamic_dir.display(),
                e
            );
            return;
        }
    };

    // Usun stare wersjonowane libonnxruntime z targetu zanim skopiujemy aktualne —
    // po bumpie wersji (np. 1.22.0 -> 1.26.0) zostawalyby OBA pliki, a ort
    // load-dynamic (probe w supertonic.rs) wybralby niedeterministycznie stary.
    if let Ok(target_entries) = std::fs::read_dir(&target_dir) {
        for e in target_entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let versioned_ort = name.starts_with("libonnxruntime.so")
                || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"));
            if versioned_ort {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    for entry in entries.flatten() {
        let dest = target_dir.join(entry.file_name());
        if let Err(e) = copy_runtime_entry(&entry.path(), &dest) {
            println!(
                "cargo:warning=tentaflow: copy native runtime {} -> {} nieudane: {}",
                entry.path().display(),
                dest.display(),
                e
            );
        }
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    }
}

// Kopiuje IZOLOWANY dylib whispera (libwhisper_tf.so / .dll) PLASKO obok
// binarki. whisper-rs-sys linkuje go dynamicznie i liczy na $ORIGIN w runtime,
// a `copy_native_dynamic_libs` odtwarza tylko strukture katalogow lib-dynamic
// (whisper-cpp/<variant>/...), wiec sam .so nie laduje plasko tam gdzie szuka
// loader. Apple pomijamy — tam STT idzie przez MLX, whisper.cpp sie nie linkuje.
fn copy_isolated_whisper_dylib() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" || target_os == "ios" {
        return;
    }
    let platform = match native_platform() {
        Some(v) => v,
        None => return,
    };
    let variant =
        std::env::var("WHISPER_CPP_NATIVE_VARIANT").unwrap_or_else(|_| "multi".to_string());
    let fname = match target_os.as_str() {
        "windows" => "whisper_tf.dll",
        _ => "libwhisper_tf.so",
    };
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest
        .join("../native-libs")
        .join(&platform)
        .join("lib-dynamic")
        .join("whisper-cpp")
        .join(&variant)
        .join(fname);
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.exists() {
        return;
    }
    let dest = cargo_target_dir().join(fname);
    let _ = std::fs::remove_file(&dest);
    if std::fs::hard_link(&src, &dest).is_err() {
        if let Err(e) = std::fs::copy(&src, &dest) {
            println!(
                "cargo:warning=tentaflow: copy isolated whisper {} -> {} nieudane: {}",
                src.display(),
                dest.display(),
                e
            );
        }
    }
}

fn native_platform() -> Option<String> {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").ok()?;
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok()?;
    let arch = match target_arch.as_str() {
        "x86_64" => "x86_64",
        "aarch64" => {
            if target_os == "linux" {
                "aarch64"
            } else {
                "arm64"
            }
        }
        _ => return None,
    };
    let os = match target_os.as_str() {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

fn copy_runtime_entry(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.is_dir() {
        let _ = std::fs::remove_dir_all(dst);
        return copy_dir_recursive(src, dst);
    }
    let _ = std::fs::remove_file(dst);
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(src)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, dst)?;
            return Ok(());
        }
        #[cfg(windows)]
        {
            let resolved = src.parent().unwrap_or_else(|| std::path::Path::new(".")).join(target);
            std::fs::copy(resolved, dst)?;
            return Ok(());
        }
    }
    match std::fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => std::fs::copy(src, dst).map(|_| ()),
    }
}

// Starsze buildy llama-cpp-sys-2 mogly zostawic wersjonowane .so w out/lib.
// Kopiujemy je, jesli juz istnieja, ale nie czekamy na nie: obecny build
// korzysta z native-libs i nie powinien blokowac sie na dawnym CMake output.
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
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }

    let copied = lib_dirs
        .iter()
        .map(|lib_dir| copy_versioned_from(lib_dir, &target_dir))
        .sum::<usize>();
    if copied == 0 {
        println!(
            "cargo:warning=tentaflow: brak versioned llama .so w dawnych out/lib — uzywam native-libs"
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

    // Metal Toolchain guard. Na macOS 26 / Xcode 26 kompilator Metala jest
    // osobnym, doinstalowywanym komponentem. Bez niego xcodebuild "buduje"
    // mlx-swift, ale metallib (skompilowane kernele GPU) jest zepsuty/niepelny
    // -> kazdy model MLX zwraca belkot (zle logity), bez zadnego bledu builda.
    // Failujemy glosno z dokladna komenda naprawy zamiast wysylac zepsute GPU.
    let metal_ok = Command::new("xcrun")
        .args(["--sdk", "macosx", "metal", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !metal_ok {
        panic!(
            "Brak Metal Toolchain — mlx-swift nie skompiluje kerneli GPU i kazdy \
             model MLX bedzie zwracal belkot. Zainstaluj komponent i przebuduj:\n\
             \n    xcodebuild -downloadComponent MetalToolchain\n\
             \nPotem wyczysc cache metalliba:\n\
             \n    rm -rf {}/build-xcode\n",
            package_dir.display()
        );
    }

    let xcode_build_dir = package_dir.join("build-xcode");
    let xcode_status = Command::new("xcodebuild")
        .args([
            "-scheme",
            "MLXBridge",
            "-destination",
            &format!("platform=macOS,arch={}", xcode_arch),
            "-configuration",
            "Release",
            // mlx-swift-lm 3.x uzywa makr Swift (#hubDownloader,
            // #huggingFaceTokenizerLoader). xcodebuild domyslnie blokuje
            // niezatwierdzone makra ("Macro must be enabled before use") i
            // wtedy dylib NIE powstaje -> backend mlx niedostepny. Flaga
            // pomija ten gate dla buildow z linii polecen.
            "-skipMacroValidation",
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

    // Zwendorowany MisakiSwift (zaleznosc lokalna KokoroBridge) — jego Resources
    // (wagi BART us/gb_bart.safetensors + leksykony) trafiaja do bundla
    // MisakiSwift_MisakiSwift. Bez tej obserwacji zmiana wag nie wymusza rebuildu
    // i bundle zostaje nieaktualny (brak wag => Kokoro TTS crashuje na ang. OOV).
    let misaki_dir = package_dir
        .parent()
        .expect("KokoroBridge/.. musi istniec")
        .join("vendor/MisakiSwift");
    println!("cargo:rerun-if-changed={}/Resources", misaki_dir.display());
    println!("cargo:rerun-if-changed={}/Sources", misaki_dir.display());

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
            "-skipMacroValidation",
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

    // KokoroBridge — w przeciwienstwie do MLXBridge (statyczny, samowystarczalny)
    // — jest DYNAMICZNIE linkowany: dylib ma @rpath zaleznosci na MLX.framework,
    // Cmlx, MLXNN, MisakiSwift, MLXUtilsLibrary, Numerics (bo MisakiSwift/
    // MLXUtilsLibrary deklaruja `type: .dynamic`). Bez dostarczenia tych
    // frameworkow + resource bundli (KokoroBridge_KokoroSwiftLocal, MisakiSwift,
    // Cmlx-metallib, ZIPFoundation) obok dylib, dlopen pada
    // ("Library not loaded: @rpath/MLX.framework") i deploy konczy sie
    // generycznym "load kokoro". rpath @loader_path + target_dir ustawia
    // build_mlx_bridge. Walidacja: dlopen+loadModel+synthesize OK po skopiowaniu.
    if let Ok(entries) = std::fs::read_dir(products.join("PackageFrameworks")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_str() == Some("KokoroBridge.framework") {
                continue; // skopiowany juz jako libKokoroBridge.dylib
            }
            if entry.path().extension().and_then(|s| s.to_str()) != Some("framework") {
                continue;
            }
            let dest = target_dir.join(&name);
            let _ = std::fs::remove_dir_all(&dest);
            if let Err(e) = copy_dir_recursive(&entry.path(), &dest) {
                println!(
                    "cargo:warning=tentaflow: copy {} nieudane: {}",
                    name.to_string_lossy(),
                    e
                );
            }
        }
    }
    // Resource bundle (Bundle.module): KokoroSwiftLocal (espeak/lexicon),
    // MisakiSwift (G2P), Cmlx (default.metallib), ZIPFoundation.
    if let Ok(entries) = std::fs::read_dir(&products) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if entry.path().extension().and_then(|s| s.to_str()) != Some("bundle") {
                continue;
            }
            let dest = target_dir.join(&name);
            let _ = std::fs::remove_dir_all(&dest);
            if let Err(e) = copy_dir_recursive(&entry.path(), &dest) {
                println!(
                    "cargo:warning=tentaflow: copy {} nieudane: {}",
                    name.to_string_lossy(),
                    e
                );
            }
        }
    }

    println!(
        "cargo:warning=tentaflow: KokoroBridge gotowy ({} + frameworks + bundles)",
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
    cmd.env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    // KLUCZOWE: parent ma .cargo/config.toml z `target-dir = "target_shared"`
    // ktore stosuje sie do KAZDEGO wywolania cargo w obrebie repo (sub-cargo
    // tez znajduje ten config przez parent traversal). Bez explicit override
    // nested cargo build probowal zapisac do target_shared/, ale parent
    // cargo trzymal exclusive file lock — deadlock (cargo wisi w 0% CPU).
    // env_remove("CARGO_TARGET_DIR") nie pomaga, bo .cargo/config.toml NIE
    // jest env var. Ustawiamy CARGO_TARGET_DIR explicit na bot's OWN target/
    // — env var override .cargo/config.toml.
    cmd.env("CARGO_TARGET_DIR", bot_dir.join("target"));

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
