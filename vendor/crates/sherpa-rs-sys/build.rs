// =============================================================================
// Plik: build.rs
// Opis: Generuje bindingi sherpa-onnx i linkuje gotowe biblioteki z native-libs.
// =============================================================================

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap();
    let native_root = native_root();
    let platform = platform_name(&target);
    let platform_root = native_root.join(platform);
    let include_dir = platform_root.join("include");
    let lib_dir = platform_root.join("lib-static");
    let dynamic_dir = platform_root.join("lib-dynamic");

    println!("cargo:rerun-if-env-changed=TENTAFLOW_NATIVE_LIBS_DIR");
    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed={}", dynamic_dir.display());
    println!("cargo:rerun-if-changed=wrapper.h");

    require(&include_dir.join("sherpa-onnx").join("c-api.h"));
    require(&lib_dir.join(static_name("sherpa-onnx-c-api", &target)));
    require(&lib_dir.join(static_name("sherpa-onnx-core", &target)));
    require(&lib_dir.join(static_name("onnxruntime", &target)));

    generate_bindings(&include_dir, &target);

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-search=native={}", dynamic_dir.display());

    for lib in [
        "sherpa-onnx-c-api",
        "sherpa-onnx-core",
        "kaldi-native-fbank-core",
        "kaldi-decoder-core",
        "sherpa-onnx-kaldifst-core",
        "sherpa-onnx-fstfar",
        "sherpa-onnx-fst",
        "sherpa-onnx-portaudio_static",
        "kissfft-float",
        "ssentencepiece_core",
        "piper_phonemize",
        "espeak-ng",
        "ucd",
        "cargs",
        "onnxruntime",
    ] {
        link_static_if_exists(&lib_dir, lib, &target);
    }

    link_system_libs(&target);
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

    panic!("sherpa-rs-sys: nie znaleziono katalogu native-libs");
}

fn platform_name(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "aarch64-apple-darwin" => "macos-arm64",
        "aarch64-apple-ios" => "ios-arm64",
        "aarch64-apple-ios-sim" => "ios-sim-arm64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => panic!("sherpa-rs-sys: brak native-libs dla targetu {other}"),
    }
}

fn static_name(name: &str, target: &str) -> String {
    if target.contains("windows-msvc") {
        format!("{name}.lib")
    } else {
        format!("lib{name}.a")
    }
}

fn require(path: &Path) {
    if !path.exists() {
        panic!(
            "sherpa-rs-sys: brak {}. Zbuduj biblioteki przez scripts/native-libs/build-all.sh",
            path.display()
        );
    }
}

fn generate_bindings(include_dir: &Path, target: &str) {
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // clang nie rozumie triple Rusta `arm64-apple-ios-sim` ('sim' to nieprawidłowa
    // wersja) — dla symulatora podajemy poprawny triple ze środowiskiem `simulator`.
    if target == "aarch64-apple-ios-sim" {
        builder = builder.clang_arg("--target=arm64-apple-ios-simulator");
    }

    let bindings = builder
        .generate()
        .expect("sherpa-rs-sys: bindgen nie wygenerowal bindings.rs");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("sherpa-rs-sys: nie mozna zapisac bindings.rs");
}

fn link_static_if_exists(lib_dir: &Path, name: &str, target: &str) {
    if lib_dir.join(static_name(name, target)).exists() {
        println!("cargo:rustc-link-lib=static={name}");
    }
}

fn link_system_libs(target: &str) {
    if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=pthread");
        println!("cargo:rustc-link-lib=dylib=dl");
        println!("cargo:rustc-link-lib=dylib=m");
        println!("cargo:rustc-link-lib=dylib=rt");
        println!("cargo:rustc-link-lib=dylib=asound");
    } else if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=framework=CoreML");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
    } else if target.contains("windows") {
        println!("cargo:rustc-link-lib=advapi32");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=winmm");
    }
}
