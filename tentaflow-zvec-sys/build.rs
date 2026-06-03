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
        panic!(
            "tentaflow-zvec-sys: missing {}. Build it with `./scripts/build-zvec.sh {}`",
            path.display(),
            platform
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
    if target.contains("linux") || target.contains("apple-darwin") {
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

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include.display()))
        .allowlist_function("zvec_.*")
        .allowlist_type("zvec_.*")
        .allowlist_var("ZVEC_.*")
        .generate()
        .expect("bindgen failed to generate zvec FFI bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}
