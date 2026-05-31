// =============================================================================
// Plik: build.rs
// Opis: Buduje swift/MLXBridge/ (Swift Package z mlx-swift + MLXLLM) jako
//       libMLXBridge.dylib i konfiguruje cargo zeby Rust binary linkowal sie
//       przeciwko niej. Bez tego MlxSwiftEngine z tentaflow-core nie ma do
//       czego wolac FFI callbackow.
//
//       Wlaczone tylko gdy feature `mlx-swift-bridge` jest aktywne (default na
//       macOS ARM64) — feature wlacza w tentaflow-core flag `inference-mlx`,
//       ktory rejestruje MlxSwiftEngine. Inne platformy pomijaja Swift build.
// =============================================================================

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Tylko macOS ma SwiftPM + MLX support.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    // Tylko gdy feature jest wlaczone (Cargo CARGO_FEATURE_MLX_SWIFT_BRIDGE=1).
    if std::env::var("CARGO_FEATURE_MLX_SWIFT_BRIDGE").is_err() {
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package_dir = manifest_dir.join("swift/MLXBridge");
    let package_swift = package_dir.join("Package.swift");

    if !package_swift.exists() {
        panic!(
            "Brak Package.swift w {}. Swift Package musi istniec zeby zbudowac libMLXBridge.dylib.",
            package_dir.display()
        );
    }

    println!(
        "cargo:rerun-if-changed={}/Package.swift",
        package_dir.display()
    );
    println!(
        "cargo:rerun-if-changed={}/Sources",
        package_dir.display()
    );
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_MLX_SWIFT_BRIDGE");

    // 1. Resolve dependencies (idempotent — szybkie gdy juz mamy w cache).
    let resolve_status = Command::new("swift")
        .args(["package", "resolve"])
        .current_dir(&package_dir)
        .status()
        .expect("Nie udalo sie odpalic `swift package resolve` — zainstaluj Xcode CLI tools");
    if !resolve_status.success() {
        panic!("swift package resolve nieudane");
    }

    // 2. Build release — zwraca .build/<triple>/release/libMLXBridge.dylib
    let build_status = Command::new("swift")
        .args(["build", "-c", "release"])
        .current_dir(&package_dir)
        .status()
        .expect("Nie udalo sie odpalic `swift build`");
    if !build_status.success() {
        panic!("swift build -c release nieudane");
    }

    // 3. Znajdz wynikowy dylib. SwiftPM uzywa innych nazw architektur niz
    //    cargo (`arm64` vs `aarch64`, `x86_64` jest takie samo).
    let cargo_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let swift_arch = match cargo_arch.as_str() {
        "aarch64" => "arm64",
        other => other,
    };
    let triple = format!("{}-apple-macosx", swift_arch);
    let dylib_dir = package_dir.join(".build").join(&triple).join("release");
    let dylib_path = dylib_dir.join("libMLXBridge.dylib");
    if !dylib_path.exists() {
        panic!(
            "swift build zakonczyl sie OK ale brak {} — sprawdz `swift build -c release` w {}",
            dylib_path.display(),
            package_dir.display()
        );
    }

    // 4. Skopiuj dylib obok wynikowego cargo binary (target/release/) zeby
    //    aplikacja znalazla go przez @rpath przy uruchomieniu z `cargo run`.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/release/build/<crate-hash>/out — wlasciwy target_dir to 4 levels up.
    let target_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR musi miec target/<profile>/build/<hash>/out struktura");
    let dest = target_dir.join("libMLXBridge.dylib");
    if let Err(e) = std::fs::copy(&dylib_path, &dest) {
        panic!(
            "Nie udalo sie skopiowac {} do {}: {}",
            dylib_path.display(),
            dest.display(),
            e
        );
    }
    println!(
        "cargo:warning=MLXBridge: skopiowano libMLXBridge.dylib do {}",
        dest.display()
    );

    // 4b. MLX (mlx-swift 0.31+) szuka swojej Metal biblioteki przez
    //     `current_binary_dir()/mlx.metallib` (dladdr na dylib). SwiftPM trzyma
    //     ja w `mlx-swift_Cmlx.bundle`, ktorego mlx-c NIE znajduje gdy dylib jest
    //     dlopen'owany z binarki Rust (Bundle.mainBundle != app bundle). Kopiujemy
    //     `default.metallib` -> `mlx.metallib` obok dylib (i obok cargo binary),
    //     zeby attempt #1 `load_colocated_library(device, "mlx")` trafil.
    let metallib_src = dylib_dir
        .join("mlx-swift_Cmlx.bundle")
        .join("Contents")
        .join("Resources")
        .join("default.metallib");
    if metallib_src.exists() {
        for metallib_dest in [dylib_dir.join("mlx.metallib"), target_dir.join("mlx.metallib")] {
            if let Err(e) = std::fs::copy(&metallib_src, &metallib_dest) {
                panic!(
                    "Nie udalo sie skopiowac metalliba {} -> {}: {}",
                    metallib_src.display(),
                    metallib_dest.display(),
                    e
                );
            }
        }
        println!("cargo:warning=MLXBridge: skopiowano mlx.metallib obok dylib + binarki");
    } else {
        println!(
            "cargo:warning=MLXBridge: BRAK metalliba {} — MLX nie znajdzie Metal biblioteki (sprawdz mlx-swift_Cmlx.bundle)",
            metallib_src.display()
        );
    }

    // 5. Linker hints — Rust binary linkowany dynamically z MLXBridge.
    //    @loader_path = katalog bin po install. Przy `cargo run` rpath
    //    pokazuje target/release/.
    println!("cargo:rustc-link-search=native={}", dylib_dir.display());
    println!("cargo:rustc-link-search=native={}", target_dir.display());
    println!("cargo:rustc-link-lib=dylib=MLXBridge");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        target_dir.display()
    );
}
