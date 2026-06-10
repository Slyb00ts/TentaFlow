// =============================================================================
// Plik: build.rs
// Opis: Dopina brakujące statyczne biblioteki GStreamer Android do linkowania
//       mobilnej biblioteki JNI.
// =============================================================================

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("android") {
        return;
    }

    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH_aarch64_linux_android");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH_armv7_linux_androideabi");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH_x86_64_linux_android");

    let env_key = match target.as_str() {
        "aarch64-linux-android" => "PKG_CONFIG_PATH_aarch64_linux_android",
        "armv7-linux-androideabi" => "PKG_CONFIG_PATH_armv7_linux_androideabi",
        "x86_64-linux-android" => "PKG_CONFIG_PATH_x86_64_linux_android",
        _ => return,
    };

    let Some(pkg_config_dir) = env::var_os(env_key).map(PathBuf::from) else {
        return;
    };
    let Some(lib_dir) = pkg_config_dir.parent().and_then(|p| p.parent()).map(|p| p.join("lib")) else {
        return;
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for lib in ["ffi", "pcre2-8", "gmodule-2.0", "iconv", "intl"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
}
