// =============================================================================
// Plik: build.rs
// Opis: Buduje swift/MLXBridge/ przez `xcodebuild` (NIE `swift build`!) bo tylko
//       Xcode kompiluje Metal shadery do `default.metallib`. Wynik:
//         - MLXBridge.framework/Versions/A/MLXBridge → kopiowane jako
//           libMLXBridge.dylib obok cargo binary
//         - mlx-swift_Cmlx.bundle/ → kopiowane obok bin (mlx szuka bundla
//           przez NS::Bundle::allBundles() przy starcie)
//       Bez metallib: "Failed to load the default metallib" przy pierwszym
//       wywolaniu MLX, czyli model = bełkot.
// =============================================================================

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package_dir = manifest_dir
        .parent()
        .expect("tentaflow-core/.. musi istniec")
        .join("tentaflow-desktop/macos/swift/MLXBridge");
    let package_swift = package_dir.join("Package.swift");

    if !package_swift.exists() {
        println!(
            "cargo:warning=tentaflow: brak {}, omijam Swift bridge build",
            package_swift.display()
        );
        return;
    }

    println!(
        "cargo:rerun-if-changed={}/Package.swift",
        package_dir.display()
    );
    println!("cargo:rerun-if-changed={}/Sources", package_dir.display());

    let cargo_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let xcode_arch = match cargo_arch.as_str() {
        "aarch64" => "arm64",
        other => other,
    };

    // xcodebuild — w przeciwienstwie do `swift build` kompiluje Metal kernels.
    let xcode_build_dir = package_dir.join("build-xcode");
    let xcode_status = Command::new("xcodebuild")
        .args([
            "-scheme",
            "MLXBridge",
            "-destination",
            &format!("platform=macOS,arch={}", xcode_arch),
            "-configuration",
            "Release",
            "-derivedDataPath",
        ])
        .arg(&xcode_build_dir)
        .arg("build")
        .current_dir(&package_dir)
        .status();
    if !matches!(xcode_status, Ok(s) if s.success()) {
        println!(
            "cargo:warning=tentaflow: xcodebuild nieudane — Swift MLX bridge nie zbudowany"
        );
        return;
    }

    // Sciezki z xcodebuild:
    //  - Framework binary (Mach-O dylib z install_name @rpath/MLXBridge.framework/...)
    //  - Resource bundle z default.metallib
    let products = xcode_build_dir.join("Build/Products/Release");
    let framework_binary = products.join("PackageFrameworks/MLXBridge.framework/Versions/A/MLXBridge");
    let metallib_bundle = products.join("mlx-swift_Cmlx.bundle");

    if !framework_binary.exists() {
        println!(
            "cargo:warning=tentaflow: xcodebuild OK ale brak {}",
            framework_binary.display()
        );
        return;
    }
    if !metallib_bundle.exists() {
        println!(
            "cargo:warning=tentaflow: brak {} — bez metallib MLX nie zadziala",
            metallib_bundle.display()
        );
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR struktura niespodziewana");

    // Kopiuj framework binary jako libMLXBridge.dylib (rename — ten sam Mach-O,
    // tylko inna nazwa zeby libloading::Library::new("libMLXBridge.dylib") znalazl).
    let dylib_dest = target_dir.join("libMLXBridge.dylib");
    if let Err(e) = std::fs::copy(&framework_binary, &dylib_dest) {
        println!("cargo:warning=tentaflow: copy dylib nieudane: {}", e);
        return;
    }

    // Naprawa install_name dylib — z `@rpath/MLXBridge.framework/...` na proste
    // `@rpath/libMLXBridge.dylib`, zeby dlopen znalazl bibloteke po nazwie.
    let _ = Command::new("install_name_tool")
        .args(["-id", "@rpath/libMLXBridge.dylib"])
        .arg(&dylib_dest)
        .status();

    // Kopiuj cały bundle (rekurencyjnie) — mlx szuka bundla przez NS::Bundle.
    let bundle_dest = target_dir.join("mlx-swift_Cmlx.bundle");
    let _ = std::fs::remove_dir_all(&bundle_dest);
    if let Err(e) = copy_dir_recursive(&metallib_bundle, &bundle_dest) {
        println!(
            "cargo:warning=tentaflow: copy mlx-swift_Cmlx.bundle nieudane: {}",
            e
        );
        return;
    }

    println!(
        "cargo:warning=tentaflow: MLXBridge gotowy ({} + bundle)",
        dylib_dest.display()
    );

    // rpath zeby binary znalazlo dylib obok siebie po install.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        target_dir.display()
    );
}

fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest)?;
        } else if path.is_symlink() {
            // Re-create symlink (frameworks have Versions/Current → A symlinks).
            let target = std::fs::read_link(&path)?;
            let _ = std::fs::remove_file(&dest);
            std::os::unix::fs::symlink(target, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}
