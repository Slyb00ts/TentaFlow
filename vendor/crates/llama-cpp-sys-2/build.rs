// =============================================================================
// Plik: build.rs
// Opis: Generuje bindingi llama.cpp i linkuje gotowe biblioteki z native-libs.
// =============================================================================

use std::env;
use std::path::{Path, PathBuf};

enum TargetOs {
    Linux,
    Macos,
    Ios,
    Windows,
}

fn main() {
    if cfg!(feature = "mtmd") {
        panic!("llama-cpp-sys-2: feature mtmd wymaga osobnego eksportu native-libs");
    }

    let target = env::var("TARGET").unwrap();
    let target_os = target_os(&target);
    let native_root = native_root();
    let platform = platform_name(&target);
    let variant = env::var("LLAMA_CPP_NATIVE_VARIANT").unwrap_or_else(|_| "multi".to_string());
    let platform_root = native_root.join(platform);
    let include_dir = platform_root.join("include").join("llama");
    let lib_dir = platform_root
        .join("lib-static")
        .join("llama-cpp")
        .join(&variant);

    println!("cargo:rerun-if-env-changed=TENTAFLOW_NATIVE_LIBS_DIR");
    println!("cargo:rerun-if-env-changed=LLAMA_CPP_NATIVE_VARIANT");
    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=wrapper_common.h");
    println!("cargo:rerun-if-changed=wrapper_common.cpp");
    println!("cargo:rerun-if-changed=wrapper_oai.h");
    println!("cargo:rerun-if-changed=wrapper_oai.cpp");
    println!("cargo:rerun-if-changed=wrapper_speculative.h");
    println!("cargo:rerun-if-changed=wrapper_speculative.cpp");
    println!("cargo:rerun-if-changed=wrapper_utils.h");

    require(&include_dir.join("llama.h"));
    require(&include_dir.join("ggml.h"));
    require(&include_dir.join("gguf.h"));
    require(&include_dir.join("common").join("chat.h"));
    require(&include_dir.join("common").join("json-schema-to-grammar.h"));
    require(&lib_dir.join(static_name("llama", &target)));
    require(&lib_dir.join(static_name("ggml", &target)));
    require(&lib_dir.join(static_name("ggml-base", &target)));
    require(&lib_dir.join(static_name("ggml-cpu", &target)));

    generate_bindings(&include_dir, &target);
    compile_wrappers(&include_dir, &target_os);
    link_native(&lib_dir, &target, &target_os);
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

    panic!("llama-cpp-sys-2: nie znaleziono katalogu native-libs");
}

fn platform_name(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "aarch64-apple-darwin" => "macos-arm64",
        "aarch64-apple-ios" => "ios-arm64",
        "aarch64-apple-ios-sim" => "ios-sim-arm64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => panic!("llama-cpp-sys-2: brak native-libs dla targetu {other}"),
    }
}

fn target_os(target: &str) -> TargetOs {
    if target.contains("windows") {
        TargetOs::Windows
    } else if target.contains("apple-ios") {
        TargetOs::Ios
    } else if target.contains("apple-darwin") {
        TargetOs::Macos
    } else if target.contains("linux") {
        TargetOs::Linux
    } else {
        panic!("llama-cpp-sys-2: nieobslugiwany target {target}");
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
            "llama-cpp-sys-2: brak {}. Zbuduj biblioteki przez scripts/native-libs/build-all.sh",
            path.display()
        );
    }
}

fn generate_bindings(include_dir: &Path, target: &str) {
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .derive_partialeq(true)
        .allowlist_function("ggml_.*")
        .allowlist_type("ggml_.*")
        .allowlist_function("gguf_.*")
        .allowlist_type("gguf_.*")
        .allowlist_function("llama_.*")
        .allowlist_type("llama_.*")
        .allowlist_function("llama_rs_.*")
        .allowlist_type("llama_rs_.*")
        .prepend_enum_name(false);

    // clang nie rozumie triple Rusta `arm64-apple-ios-sim` ('sim' to nieprawidłowa
    // wersja) — dla symulatora podajemy poprawny triple ze środowiskiem `simulator`.
    if target == "aarch64-apple-ios-sim" {
        builder = builder.clang_arg("--target=arm64-apple-ios-simulator");
    }

    let bindings = builder
        .generate()
        .expect("llama-cpp-sys-2: bindgen nie wygenerowal bindings.rs");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("llama-cpp-sys-2: nie mozna zapisac bindings.rs");
}

fn compile_wrappers(include_dir: &Path, target_os: &TargetOs) {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("wrapper_common.cpp")
        .file("wrapper_oai.cpp")
        .file("wrapper_speculative.cpp")
        .include(include_dir)
        .include(Path::new("."))
        .flag_if_supported("-std=c++17")
        .flag_if_supported("-Wno-unused-function")
        .pic(true);

    if matches!(target_os, TargetOs::Windows) {
        build.flag("/std:c++17");
        build.flag("/wd4505");
    }

    build.compile("llama_cpp_sys_2_common_wrapper");
}

fn link_native(lib_dir: &Path, target: &str, target_os: &TargetOs) {
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    link_static_if_exists(lib_dir, "llama-common", target);
    link_static_if_exists(lib_dir, "llama-common-base", target);
    link_static_if_exists(lib_dir, "common", target);
    link_static_if_exists(lib_dir, "cpp-httplib", target);
    link_static_if_exists(lib_dir, "llama", target);
    link_static_if_exists(lib_dir, "ggml", target);
    link_static_if_exists(lib_dir, "ggml-base", target);
    link_static_if_exists(lib_dir, "ggml-cpu", target);
    link_static_if_exists(lib_dir, "ggml-vulkan", target);
    link_static_if_exists(lib_dir, "ggml-cuda", target);
    link_static_if_exists(lib_dir, "ggml-hip", target);
    link_static_if_exists(lib_dir, "ggml-metal", target);

    match target_os {
        TargetOs::Linux => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
            println!("cargo:rustc-link-lib=dylib=gomp");
        }
        TargetOs::Macos | TargetOs::Ios => {
            println!("cargo:rustc-link-lib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=Metal");
            println!("cargo:rustc-link-lib=framework=MetalKit");
            println!("cargo:rustc-link-lib=framework=Accelerate");
        }
        TargetOs::Windows => {
            println!("cargo:rustc-link-lib=advapi32");
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
        println!("cargo:rustc-link-lib=dylib=cudart");
        println!("cargo:rustc-link-lib=dylib=cublas");
        println!("cargo:rustc-link-lib=dylib=cublasLt");
        println!("cargo:rustc-link-lib=dylib=cuda");
        println!("cargo:rustc-link-lib=static=culibos");
    }

    if lib_dir.join(static_name("ggml-hip", target)).exists() {
        let hip_path = env::var("HIP_PATH").unwrap_or_else(|_| "/opt/rocm".to_string());
        println!("cargo:rustc-link-search=native={}/lib", hip_path);
        println!("cargo:rustc-link-lib=dylib=amdhip64");
        println!("cargo:rustc-link-lib=dylib=rocblas");
        println!("cargo:rustc-link-lib=dylib=hipblas");
    }
}

fn link_static_if_exists(lib_dir: &Path, name: &str, target: &str) {
    if lib_dir.join(static_name(name, target)).exists() {
        println!("cargo:rustc-link-lib=static={name}");
    }
}
