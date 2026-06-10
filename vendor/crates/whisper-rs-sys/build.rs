// =============================================================================
// Plik: build.rs
// Opis: Generuje bindingi whisper.cpp i linkuje gotowe biblioteki z native-libs.
// =============================================================================

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap();
    let native_root = native_root();
    let platform = platform_name(&target);
    let variant = env::var("WHISPER_CPP_NATIVE_VARIANT").unwrap_or_else(|_| "multi".to_string());
    let platform_root = native_root.join(platform);
    let include_dir = platform_root.join("include").join("whisper");
    // Whisper jest linkowany jako IZOLOWANY dylib (prywatny ggml, eksport tylko
    // whisper_*) — budowany przez scripts/native-libs/build-whisper-cpp.sh. Dzieki
    // temu glowna binarka moze rownoczesnie linkowac STATYCZNY ggml z llama.cpp
    // (inna wersja) bez kolizji symboli. NIE linkujemy tu statycznych ggml ani
    // libow backendow (cuda/vulkan/...) — wszystko jest zamkniete w .so.
    let lib_dir = platform_root
        .join("lib-dynamic")
        .join("whisper-cpp")
        .join(&variant);

    println!("cargo:rerun-if-env-changed=TENTAFLOW_NATIVE_LIBS_DIR");
    println!("cargo:rerun-if-env-changed=WHISPER_CPP_NATIVE_VARIANT");
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed=wrapper.h");

    require(&include_dir.join("whisper.h"));
    require(&lib_dir.join(dynamic_name("whisper_tf", &target)));

    generate_bindings(&include_dir);

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=whisper_tf");
    // Runtime: tentaflow/build.rs kopiuje libwhisper_tf.* obok binarki ($ORIGIN
    // rpath); na macOS install-name to @rpath/libwhisper_tf.dylib.

    println!("cargo:WHISPER_CPP_VERSION=native-libs-isolated-dylib");
}

fn native_root() -> PathBuf {
    if let Ok(path) = env::var("TENTAFLOW_NATIVE_LIBS_DIR") {
        return PathBuf::from(path);
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    for parent in manifest.ancestors() {
        let candidate = parent.join("native-libs");
        if candidate.is_dir() {
            return candidate;
        }
    }

    panic!("whisper-rs-sys: nie znaleziono katalogu native-libs");
}

fn platform_name(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "aarch64-apple-darwin" => "macos-arm64",
        "aarch64-linux-android" => "android-arm64",
        "armv7-linux-androideabi" => "android-armv7",
        "x86_64-linux-android" => "android-x86_64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => panic!("whisper-rs-sys: brak native-libs dla targetu {other}"),
    }
}

fn dynamic_name(name: &str, target: &str) -> String {
    if target.contains("windows-msvc") {
        format!("{name}.dll")
    } else if target.contains("apple") {
        format!("lib{name}.dylib")
    } else {
        format!("lib{name}.so")
    }
}

fn require(path: &Path) {
    if !path.exists() {
        panic!(
            "whisper-rs-sys: brak {}. Zbuduj biblioteki przez scripts/native-libs/build-all.sh",
            path.display()
        );
    }
}

fn generate_bindings(include_dir: &Path) {
    let bindings = bindgen::Builder::default()
        .rust_edition(bindgen::RustEdition::Edition2021)
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("whisper-rs-sys: bindgen nie wygenerowal bindings.rs");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("whisper-rs-sys: nie mozna zapisac bindings.rs");
}
