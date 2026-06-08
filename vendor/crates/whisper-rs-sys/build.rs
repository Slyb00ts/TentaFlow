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
    let lib_dir = platform_root
        .join("lib-static")
        .join("whisper-cpp")
        .join(&variant);

    println!("cargo:rerun-if-env-changed=TENTAFLOW_NATIVE_LIBS_DIR");
    println!("cargo:rerun-if-env-changed=WHISPER_CPP_NATIVE_VARIANT");
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed=wrapper.h");

    require(&include_dir.join("whisper.h"));
    require(&lib_dir.join(static_name("whisper", &target)));
    require(&lib_dir.join(static_name("ggml", &target)));
    require(&lib_dir.join(static_name("ggml-base", &target)));
    require(&lib_dir.join(static_name("ggml-cpu", &target)));

    generate_bindings(&include_dir);

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    link_static_if_exists(&lib_dir, "whisper", &target);
    link_static_if_exists(&lib_dir, "ggml", &target);
    link_static_if_exists(&lib_dir, "ggml-base", &target);
    link_static_if_exists(&lib_dir, "ggml-cpu", &target);
    link_static_if_exists(&lib_dir, "ggml-vulkan", &target);
    link_static_if_exists(&lib_dir, "ggml-cuda", &target);
    link_static_if_exists(&lib_dir, "ggml-hip", &target);
    link_static_if_exists(&lib_dir, "ggml-metal", &target);

    link_system_libs(&lib_dir, &target);

    println!("cargo:WHISPER_CPP_VERSION=native-libs");
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
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => panic!("whisper-rs-sys: brak native-libs dla targetu {other}"),
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

fn link_static_if_exists(lib_dir: &Path, name: &str, target: &str) {
    if lib_dir.join(static_name(name, target)).exists() {
        println!("cargo:rustc-link-lib=static={name}");
    }
}

fn link_system_libs(lib_dir: &Path, target: &str) {
    if target.contains("linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=gomp");
    } else if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=framework=Accelerate");
        if lib_dir.join("libggml-metal.a").exists() {
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=Metal");
            println!("cargo:rustc-link-lib=framework=MetalKit");
        }
    }

    if lib_dir.join(static_name("ggml-vulkan", target)).exists() {
        println!("cargo:rustc-link-lib=dylib=vulkan");
    }

    if lib_dir.join(static_name("ggml-cuda", target)).exists() {
        println!("cargo:rustc-link-search=native=/usr/local/cuda/lib64");
        println!("cargo:rustc-link-search=native=/usr/local/cuda/lib64/stubs");
        println!("cargo:rustc-link-search=native=/opt/cuda/lib64");
        println!("cargo:rustc-link-search=native=/opt/cuda/lib64/stubs");
        println!("cargo:rustc-link-lib=dylib=cublas");
        println!("cargo:rustc-link-lib=dylib=cudart");
        println!("cargo:rustc-link-lib=dylib=cublasLt");
        println!("cargo:rustc-link-lib=dylib=cuda");
        println!("cargo:rustc-link-lib=static=culibos");
    }

    if lib_dir.join(static_name("ggml-hip", target)).exists() {
        let hip_path = env::var("HIP_PATH").unwrap_or_else(|_| "/opt/rocm".to_string());
        println!("cargo:rustc-link-search=native={}/lib", hip_path);
        println!("cargo:rustc-link-lib=dylib=hipblas");
        println!("cargo:rustc-link-lib=dylib=rocblas");
        println!("cargo:rustc-link-lib=dylib=amdhip64");
    }
}
