// =============================================================================
// Plik: build.rs
// Opis: Linkuje gotowe zvec z native-libs i generuje bindingi C API.
// =============================================================================
//
use std::env;
use std::path::{Path, PathBuf};

fn require(path: &Path, platform: &str) {
    if !path.exists() {
        let hint = if platform == "windows-x86_64" {
            "scripts\\native-libs\\build-all.ps1 --Only zvec".to_string()
        } else {
            "./scripts/native-libs/build-all.sh --only zvec".to_string()
        };
        panic!(
            "tentaflow-zvec-sys: missing {}. Build it with `{}`",
            path.display(),
            hint
        );
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target = env::var("TARGET").unwrap();

    // Map the Rust target triple to the vendored archive subdirectory.
    let platform = match target.as_str() {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "aarch64-apple-darwin" => "macos-arm64",
        "aarch64-apple-ios" => "ios-arm64",
        "aarch64-apple-ios-sim" => "ios-sim-arm64",
        "aarch64-linux-android" => "android-arm64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => panic!("tentaflow-zvec-sys: no vendored zvec archive for target `{other}`"),
    };

    let native_root = manifest.join("../native-libs").join(platform);
    let include = native_root.join("include");
    let static_dir = native_root.join("lib-static");
    let dynamic_dir = native_root.join("lib-dynamic");

    // Linking model differs by platform:
    //   * Desktop (Linux/macOS): link the self-contained shared lib that zvec's
    //     CMake produces — it bundles RocksDB/Arrow/protobuf and a static libstdc++,
    //     exporting only the C API. Correct and simple; the registrations are baked
    //     in at .so/.dylib link time.
    //   * iOS/Android: link the static archives (a mobile app cannot ship a loose
    //     .so). `libzvec_c_api` (C entry points + the 4 internal zvec libs that carry
    //     the index/metric static-initializer registrations) MUST be whole-archived;
    //     `libzvec_deps` (protobuf/Arrow/RocksDB) is linked normally.
    if target.contains("windows") {
        require(&static_dir.join("zvec_c_api.lib"), platform);
        println!("cargo:rustc-link-search=native={}", static_dir.display());
        println!("cargo:rustc-link-lib=dylib=zvec_c_api");
    } else if target.contains("linux") || target.contains("apple-darwin") {
        let shared = if target.contains("apple") {
            "libzvec_c_api.dylib"
        } else {
            "libzvec_c_api.so"
        };
        require(&dynamic_dir.join(shared), platform);
        println!("cargo:rustc-link-search=native={}", dynamic_dir.display());
        println!("cargo:rustc-link-lib=dylib=zvec_c_api");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dynamic_dir.display());
    } else {
        require(&static_dir.join("libzvec_c_api.a"), platform);
        require(&static_dir.join("libzvec_deps.a"), platform);
        println!("cargo:rustc-link-search=native={}", static_dir.display());
        println!("cargo:rustc-link-lib=static:+whole-archive=zvec_c_api");
        println!("cargo:rustc-link-lib=static=zvec_deps");
        if target.contains("apple") {
            println!("cargo:rustc-link-lib=c++");
        } else if target.contains("android") {
            println!("cargo:rustc-link-lib=c++_shared");
        }
    }

    let header = include.join("zvec/c_api.h");
    println!("cargo:rerun-if-changed={}", header.display());
    println!("cargo:rerun-if-changed={}", static_dir.display());
    println!("cargo:rerun-if-changed={}", dynamic_dir.display());
    println!("cargo:rerun-if-changed=wrapper.h");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include.display()))
        .allowlist_function("zvec_.*")
        .allowlist_type("zvec_.*")
        .allowlist_var("ZVEC_.*");

    // Cross-compiling to iOS: bindgen domyslnie podaje clangowi rustowy triple
    // (np. `aarch64-apple-ios-sim`), ktorego clang nie akceptuje ("version 'sim'
    // is invalid"), a bez sysroota nie znajduje naglowkow systemowych (string.h).
    // Podajemy clangowi poprawny triple + -isysroot wlasciwego SDK (xcrun), zeby
    // FFI dla device i symulatora dzialalo niezaleznie od env z build-rust.sh.
    if target.contains("apple-ios") {
        let (sdk, clang_triple) = if target.contains("-sim") {
            ("iphonesimulator", "arm64-apple-ios-simulator")
        } else {
            ("iphoneos", "arm64-apple-ios")
        };
        let out = std::process::Command::new("xcrun")
            .args(["--sdk", sdk, "--show-sdk-path"])
            .output()
            .unwrap_or_else(|e| panic!("tentaflow-zvec-sys: xcrun --sdk {sdk} --show-sdk-path: {e}"));
        if !out.status.success() {
            panic!("tentaflow-zvec-sys: xcrun --sdk {sdk} --show-sdk-path failed");
        }
        let sdk_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        builder = builder
            .clang_arg("-target")
            .clang_arg(clang_triple)
            .clang_arg("-isysroot")
            .clang_arg(sdk_path);
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate zvec FFI bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}
