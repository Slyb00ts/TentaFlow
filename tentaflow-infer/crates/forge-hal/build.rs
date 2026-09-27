// =============================================================================
// Plik: build.rs
// Opis: Buduje shimy natywne (HIP, Metal, CoreML) i wskazuje linkerowi ich
//       biblioteki. Każdy shim uruchamia się WYŁĄCZNIE ze swoją cechą
//       (`hip`, `metal`, `coreml`), więc maszyny bez ROCm czy bez Apple SDK
//       budują forge-hal bez zmian.
// =============================================================================
fn main() {
    build_metal_shim();
    build_coreml_shim();
    println!("cargo:rerun-if-changed=hip/forge_hip_shim.c");
    println!("cargo:rerun-if-env-changed=ROCM_PATH");
    if std::env::var_os("CARGO_FEATURE_HIP").is_none() {
        return;
    }
    let rocm = std::env::var("ROCM_PATH").unwrap_or_else(|_| "/opt/rocm".to_string());
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let obj = format!("{out}/forge_hip_shim.o");
    let status = std::process::Command::new(format!("{rocm}/llvm/bin/clang"))
        .args([
            "-O2",
            "-fPIC",
            "-D__HIP_PLATFORM_AMD__",
            "-c",
            "hip/forge_hip_shim.c",
            "-o",
            &obj,
        ])
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

/// Shim Metala. Buduje się wyłącznie z cechą `metal` i wyłącznie na Apple,
/// więc Linux i Windows kompilują forge-hal bez zmian.
///
/// iOS potrzebuje jawnego celu i sysrootu SDK — bez nich clang zbudowałby
/// obiekt dla macOS, a linker odrzuciłby go dopiero na końcu, z komunikatem
/// nie wskazującym na przyczynę.
fn build_metal_shim() {
    println!("cargo:rerun-if-changed=metal/forge_metal_shim.m");
    if std::env::var_os("CARGO_FEATURE_METAL").is_none() {
        return;
    }
    build_objc_shim("metal", "metal/forge_metal_shim.m", "forge_metal_shim");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// Shim CoreML. Ta sama ścieżka co Metal, osobna cecha `coreml`: wiązanie ANE
/// ma działać także wtedy, gdy backend Metal nie jest włączony, więc Foundation
/// linkuje się tu niezależnie (podwójne `framework=Foundation` jest nieszkodliwe).
fn build_coreml_shim() {
    println!("cargo:rerun-if-changed=coreml/forge_coreml_shim.m");
    if std::env::var_os("CARGO_FEATURE_COREML").is_none() {
        return;
    }
    // `-fobjc-exceptions`: shim łapie NSException z CoreML (@try/@catch) i
    // oddaje je jako kod błędu zamiast pozwolić im zabić wątek roboczy.
    build_objc_shim_with(
        "coreml",
        "coreml/forge_coreml_shim.m",
        "forge_coreml_shim",
        &["-fobjc-exceptions"],
    );
    println!("cargo:rustc-link-lib=framework=CoreML");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// Kompiluje jeden plik Objective-C (ARC) do biblioteki statycznej `lib{name}.a`
/// w OUT_DIR i zgłasza ją linkerowi. Panikuje na celu innym niż Apple.
fn build_objc_shim(feature: &str, source: &str, name: &str) {
    build_objc_shim_with(feature, source, name, &[]);
}

/// Jak [`build_objc_shim`], z dodatkowymi flagami clanga (np. wyjątki ObjC).
fn build_objc_shim_with(feature: &str, source: &str, name: &str, extra: &[&str]) {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let sdk = match os.as_str() {
        "macos" => None,
        "ios" => Some(
            if std::env::var("TARGET")
                .unwrap_or_default()
                .ends_with("-sim")
            {
                ("iphonesimulator", "arm64-apple-ios13.0-simulator")
            } else {
                ("iphoneos", "arm64-apple-ios13.0")
            },
        ),
        other => panic!("cecha `{feature}` wymaga celu Apple, a nie `{other}`"),
    };
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let obj = format!("{out}/{name}.o");
    let mut cmd = std::process::Command::new("clang");
    cmd.args(["-O2", "-fPIC", "-fobjc-arc"])
        .args(extra)
        .args(["-c", source, "-o", &obj]);
    if let Some((sdk_name, target)) = sdk {
        let path = std::process::Command::new("xcrun")
            .args(["--sdk", sdk_name, "--show-sdk-path"])
            .output()
            .expect("uruchomienie xcrun");
        assert!(path.status.success(), "brak SDK `{sdk_name}`");
        cmd.arg("-isysroot")
            .arg(String::from_utf8_lossy(&path.stdout).trim())
            .args(["-target", target]);
    }
    let status = cmd.status().expect("uruchomienie clang");
    assert!(
        status.success(),
        "kompilacja shimu `{feature}` nie powiodla sie"
    );
    let lib = format!("{out}/lib{name}.a");
    let status = std::process::Command::new("ar")
        .args(["crs", &lib, &obj])
        .status()
        .expect("uruchomienie ar");
    assert!(
        status.success(),
        "archiwizacja shimu `{feature}` nie powiodla sie"
    );
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static={name}");
}
