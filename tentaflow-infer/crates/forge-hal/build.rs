// =============================================================================
// Plik: build.rs
// Opis: Buduje shim właściwości HIP i wskazuje linkerowi bibliotekę ROCm.
//       Uruchamia się WYŁĄCZNIE z cechą `hip`, więc maszyny bez ROCm budują
//       forge-hal bez zmian.
// =============================================================================
fn main() {
    build_metal_shim();
    println!("cargo:rerun-if-changed=hip/forge_hip_shim.c");
    println!("cargo:rerun-if-env-changed=ROCM_PATH");
    if std::env::var_os("CARGO_FEATURE_HIP").is_none() {
        return;
    }
    let rocm = std::env::var("ROCM_PATH").unwrap_or_else(|_| "/opt/rocm".to_string());
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let obj = format!("{out}/forge_hip_shim.o");
    let status = std::process::Command::new(format!("{rocm}/llvm/bin/clang"))
        .args(["-O2", "-fPIC", "-D__HIP_PLATFORM_AMD__", "-c", "hip/forge_hip_shim.c", "-o", &obj])
        .arg(format!("-I{rocm}/include"))
        .status()
        .expect("uruchomienie clang z ROCm");
    assert!(status.success(), "kompilacja shimu HIP nie powiodła się");
    let lib = format!("{out}/libforge_hip_shim.a");
    let status = std::process::Command::new("ar")
        .args(["crs", &lib, &obj])
        .status()
        .expect("uruchomienie ar");
    assert!(status.success(), "archiwizacja shimu HIP nie powiodła się");
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=forge_hip_shim");
    println!("cargo:rustc-link-search=native={rocm}/lib");
    println!("cargo:rustc-link-lib=dylib=amdhip64");
}

/// Shim Metala. Buduje się wyłącznie z cechą `metal` i wyłącznie na macOS,
/// więc Linux i Windows kompilują forge-hal bez zmian.
fn build_metal_shim() {
    println!("cargo:rerun-if-changed=metal/forge_metal_shim.m");
    if std::env::var_os("CARGO_FEATURE_METAL").is_none() {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        panic!("cecha `metal` wymaga celu macOS");
    }
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let obj = format!("{out}/forge_metal_shim.o");
    let status = std::process::Command::new("clang")
        .args([
            "-O2",
            "-fPIC",
            "-fobjc-arc",
            "-c",
            "metal/forge_metal_shim.m",
            "-o",
            &obj,
        ])
        .status()
        .expect("uruchomienie clang");
    assert!(status.success(), "kompilacja shimu Metal nie powiodla sie");
    let lib = format!("{out}/libforge_metal_shim.a");
    let status = std::process::Command::new("ar")
        .args(["crs", &lib, &obj])
        .status()
        .expect("uruchomienie ar");
    assert!(status.success(), "archiwizacja shimu Metal nie powiodla sie");
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=forge_metal_shim");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
