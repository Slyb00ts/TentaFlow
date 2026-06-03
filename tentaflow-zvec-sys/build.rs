// ===== File: build.rs — link the vendored zvec static archive + generate FFI bindings =====
//
// zvec is a heavy C++ engine (RocksDB + Arrow + protobuf + ...). We do NOT compile
// it here: it is built once per platform into a single self-contained static archive
// (`libzvec_c_api.a`) by `scripts/build-zvec.sh` and vendored under `vendor/lib/<platform>`.
// This build script only links that archive and runs bindgen over the C API header.

use std::env;
use std::path::{Path, PathBuf};

fn require(path: &Path, platform: &str) {
    if !path.exists() {
        // build-zvec.sh is a bash script and cannot run on native Windows; point
        // Windows users at the PowerShell installer that builds zvec with MSVC.
        let hint = if platform == "windows-x86_64" {
            "scripts\\setup.ps1".to_string()
        } else {
            format!("./scripts/build-zvec.sh {platform}")
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
    let include = manifest.join("vendor/include");

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

    let lib_dir = manifest.join("vendor/lib").join(platform);
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

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
        // MSVC links against the import library (`zvec_c_api.lib`) at build time;
        // the actual `zvec_c_api.dll` is resolved at runtime from PATH or the
        // executable's directory. Windows has no rpath, so the .dll must be copied
        // next to the binary (tentaflow/build.rs handles that for the main exe).
        require(&lib_dir.join("zvec_c_api.lib"), platform);
        println!("cargo:rustc-link-lib=dylib=zvec_c_api");
    } else if target.contains("linux") || target.contains("apple-darwin") {
        let shared = if target.contains("apple") {
            "libzvec_c_api.dylib"
        } else {
            "libzvec_c_api.so"
        };
        require(&lib_dir.join(shared), platform);
        println!("cargo:rustc-link-lib=dylib=zvec_c_api");
        // Let this crate's own test/bench binaries find the lib without
        // LD_LIBRARY_PATH. Dependent binaries (tentaflow) set their own rpath.
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    } else {
        require(&lib_dir.join("libzvec_c_api.a"), platform);
        require(&lib_dir.join("libzvec_deps.a"), platform);
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
