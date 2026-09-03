// =============================================================================
// Plik: build.rs
// Opis: Build script — kompiluje addony do WASM (wasm32-wasip1) i pakuje je
//       jako dane osadzone w binarce (include_bytes!/include_str!).
//       Aktywny tylko gdy feature addon-runtime jest wlaczony.
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Generuj certyfikaty TLS jesli nie istnieja
    generate_self_signed_certs();

    let out_dir_env = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // TENTAFLOW_FAST_BUILD=1 is a developer/test-only switch: it skips the
    // steps that dominate every rebuild of this crate but are irrelevant for
    // `cargo test`/`cargo check` (browser WASM codecs, addon compilation,
    // container context packing). Binaries built with it embed stale or
    // empty browser/addon/container assets and MUST NOT be shipped.
    println!("cargo:rerun-if-env-changed=TENTAFLOW_FAST_BUILD");
    let fast_build = std::env::var("TENTAFLOW_FAST_BUILD")
        .map(|v| v == "1")
        .unwrap_or(false);
    if fast_build {
        println!(
            "cargo:warning=TENTAFLOW_FAST_BUILD=1: skipping wasm codec builds, addon compilation and container packing (dev/test only)"
        );
    }

    // Compile the fused GPU crop-preprocess CUDA kernel (nvcc) and emit its link
    // flags. CUDA preprocessing is supported on Linux and Windows; macOS uses
    // Metal. Done early so a missing CUDA toolchain fails fast.
    compile_cuda_preprocess(&out_dir_env);

    // Skanuj manifesty serwisow tentaflow-containers/*/_services/*.toml,
    // waliduj semantycznie i wygeneruj services_generated.rs + services-manifest.js.
    // To musi byc PRZED dlugim WASM-buildem, zeby blad walidacji wykryl sie szybko.
    generate_services_manifest(&out_dir_env);

    // Zbuduj tentaflow-protocol-wasm (Envelope + MessageBody codec dla browsera)
    // i wygeneruj wasm-bindgen JS glue do www/js/protocol/.
    // MUSI byc przed generate_wwwroot_embed zeby wynikowe pliki trafily do embed.
    if !fast_build {
        build_protocol_wasm_bindings();
    }

    // Zbuduj tentaflow-voxel-wasm (browser WebGPU/WebGL voxel point-cloud
    // renderer) i wygeneruj wasm-bindgen JS glue do www/js/voxel/.
    // MUSI byc przed generate_wwwroot_embed zeby wynikowe pliki trafily do embed.
    if !fast_build {
        build_voxel_wasm_bindings();
    }

    // Wygeneruj asset-manifest.js + sw-version.js + staly ASSET_BUILD_HASH z
    // SHA-256 calego frontu. MUSI byc PO wygenerowaniu wasm glue i
    // services-manifest.js (zeby wliczyc ich tresc) i PRZED wwwroot_embed
    // (zeby wynikowe pliki trafily do embed).
    generate_asset_manifest(&out_dir_env);

    // Generuj wwwroot_embed.rs — pliki statyczne wbudowane w binarie
    // (po wygenerowaniu services-manifest.js, zeby trafil do embed).
    generate_wwwroot_embed(&out_dir_env);

    // Pakuj kontekst dockerow (tentaflow-containers + shared Rust crates)
    // jako tar.gz wbudowany w binarce — deploy module rozpakowuje to do tmpdir
    // i robi `docker build` bez wymagania zewnetrznych zrodel.
    if fast_build {
        std::fs::write(out_dir_env.join("container_bundle.tar.gz"), b"").unwrap();
    } else {
        pack_container_contexts(&out_dir_env);
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bundle_dir = out_dir.join("addon_bundles");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Sprawdz czy wasm32-wasip1 target jest zainstalowany
    let has_wasm_target = !fast_build && check_wasm_target();

    // Zbierz informacje o skompilowanych addonach
    let mut bundled_addons: Vec<BundledAddonInfo> = Vec::new();

    // Skanuj oba katalogi addonow: darmowe (addons/) i platne (addons-pro/)
    let addon_dirs = [Path::new("addons"), Path::new("addons-pro")];
    for addons_dir in &addon_dirs {
        if !addons_dir.exists() {
            continue;
        }
        // Rerun jesli katalog sie zmieni
        println!("cargo:rerun-if-changed={}", addons_dir.display());

        let entries = match std::fs::read_dir(addons_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let addon_dir = entry.path();
            if !addon_dir.is_dir() {
                continue;
            }
            let manifest_path = addon_dir.join("manifest.toml");
            if !manifest_path.exists() {
                continue;
            }

            // The package id "ml-studio" belongs to the NATIVE ML Studio app
            // (app-platform P2.2, src/ml_studio/app-manifest.toml). The legacy
            // WASM prototype under addons/ml-studio stays in the tree but out
            // of the bundle — two catalog packages with one id would collide
            // in the instance gate and the native-hooks lookup.
            if addon_dir.file_name().and_then(|n| n.to_str()) == Some("ml-studio") {
                continue;
            }

            // The manifest `runtime` field is the source of truth for which
            // toolchain builds the addon (must match language_adapter.rs):
            // "dotnet" → dotnet publish, everything else (wasmtime/wasmi, or
            // unset) → cargo. Gating on the manifest — not on which project
            // file happens to exist — keeps the build path aligned with the
            // adapter the host will select at load time.
            let runtime = read_manifest_runtime(&manifest_path);
            let is_dotnet = runtime.as_deref() == Some("dotnet");
            let csproj = if is_dotnet {
                find_csproj(&addon_dir)
            } else {
                None
            };
            let is_rust = !is_dotnet && addon_dir.join("Cargo.toml").exists();
            if is_dotnet && csproj.is_none() {
                println!(
                    "cargo:warning=Addon '{}' — manifest runtime=\"dotnet\" ale brak pliku .csproj, pomijam",
                    addon_dir.file_name().unwrap().to_string_lossy()
                );
                continue;
            }
            if !is_rust && !is_dotnet {
                continue;
            }

            let addon_name = addon_dir.file_name().unwrap().to_string_lossy().to_string();

            // Track every source file so cargo reruns build.rs when addon
            // code changes. Without this, editing src/lib.rs inside an addon
            // does NOT trigger a rebuild — cargo only watches the directory
            // entry (add/remove), not recursive file content.
            let src_dir = addon_dir.join("src");
            if src_dir.is_dir() {
                for src_entry in walkdir_rs(&src_dir) {
                    println!("cargo:rerun-if-changed={}", src_entry.display());
                }
            }
            if is_rust {
                println!(
                    "cargo:rerun-if-changed={}",
                    addon_dir.join("Cargo.toml").display()
                );
            }
            if let Some(csproj_path) = &csproj {
                println!("cargo:rerun-if-changed={}", csproj_path.display());
                for cs in list_cs_sources(&addon_dir) {
                    println!("cargo:rerun-if-changed={}", cs.display());
                }
            }
            println!(
                "cargo:rerun-if-changed={}",
                addon_dir.join("manifest.toml").display()
            );
            if src_dir.is_dir() {
                println!("cargo:rerun-if-changed={}", src_dir.display());
            }
            if addon_dir.join("migrations").exists() {
                println!(
                    "cargo:rerun-if-changed={}",
                    addon_dir.join("migrations").display()
                );
            }
            if addon_dir.join("flows").exists() {
                println!(
                    "cargo:rerun-if-changed={}",
                    addon_dir.join("flows").display()
                );
            }

            println!(
                "cargo:warning=Addon '{}' — rozpoczynam budowanie WASM",
                addon_name
            );

            if is_dotnet {
                // Addon .NET (NativeAOT-LLVM) — dotnet publish do wasm32-wasip1.
                let wasm_path = match build_dotnet_addon(&addon_dir, &addon_name) {
                    Some(p) => p,
                    None => continue,
                };
                if let Err(bad_imports) = validate_wasm_imports(&wasm_path) {
                    panic!(
                        "\n\nAddon '{}' — WASM import namespace error!\n\
                         The following imports use the \"env\" module instead of \"tentaflow\":\n\
                         {}\n",
                        addon_name, bad_imports
                    );
                }
                let bundle_addon_dir = bundle_dir.join(&addon_name);
                std::fs::create_dir_all(&bundle_addon_dir).unwrap();
                std::fs::copy(&wasm_path, bundle_addon_dir.join("addon.wasm")).unwrap();
                std::fs::copy(
                    addon_dir.join("manifest.toml"),
                    bundle_addon_dir.join("manifest.toml"),
                )
                .unwrap();
                for file in &["SKILL.md", "DESCRIPTION.md", "blocks.json", "icon.png"] {
                    let src = addon_dir.join(file);
                    if src.exists() {
                        std::fs::copy(&src, bundle_addon_dir.join(file)).ok();
                    }
                }
                copy_dir_flat(
                    &addon_dir.join("migrations"),
                    &bundle_addon_dir.join("migrations"),
                );
                copy_dir_flat(&addon_dir.join("flows"), &bundle_addon_dir.join("flows"));
                bundled_addons.push(BundledAddonInfo {
                    name: addon_name,
                    bundle_path: bundle_addon_dir,
                });
                continue;
            }

            if !has_wasm_target {
                println!(
                    "cargo:warning=Addon '{}' — pomijam: brak wasm32-wasip1 target \
                     (zainstaluj: rustup target add wasm32-wasip1)",
                    addon_name
                );
                continue;
            }

            // Kompiluj addon do WASM
            // WAZNE: usun RUSTFLAGS/CARGO_ENCODED_RUSTFLAGS z parent process —
            // build-rust.sh ustawia flagi iOS (-mios-version-min, libclang_rt.ios.a)
            // ktore powoduja blad linkera WASM (rust-lld nie obsluguje flag iOS)
            // KRYTYCZNE: root .cargo/config.toml ma `target-dir = "target_shared"`.
            // Sub-cargo dziedziczy ten config przez parent traversal → WASM
            // ladowal w repo_root/target_shared/wasm32-wasip1/release/ zamiast
            // w addon_dir/target/wasm32-wasip1/release/ gdzie build.rs go szuka.
            // Skutek: build.rs print'owal "kompilacja zakonczona pomyslnie" ale
            // potem "brak pliku .wasm" i embedowal STARY bundled WASM z db.
            // Override CARGO_TARGET_DIR explicit na addon-local target/, env
            // var wygrywa z config.toml.
            // Absolute path required — current_dir() zmienia CWD na addon_dir,
            // a relative "target" interpretowane od nowego CWD = duplikacja
            // (addon_dir/addon_dir/target). Canonicalize z addon_dir (relative
            // od tentaflow-core/) → absolute.
            // Wspoldzielony katalog wasm dla WSZYSTKICH addonow — addon-sdk i
            // wspolne deps kompiluja sie RAZ, nie 18x (per-addon target/ to bylo
            // ~25 GB i 18-krotna rekompilacja sdk). Katalog jest SIBLINGIEM
            // target_shared (nie pod nim) → wlasny lock pliku, brak deadlocku z
            // parent cargo. Buildy addonow sa sekwencyjne (status() blokuje),
            // wiec jeden wspoldzielony katalog nie ma kontencji locka.
            let addon_target = shared_addon_wasm_target();
            let status = Command::new("cargo")
                .args(["build", "--target", "wasm32-wasip1", "--release"])
                .current_dir(&addon_dir)
                .env("CARGO_TARGET_DIR", &addon_target)
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("CFLAGS")
                .env_remove("CXXFLAGS")
                .env_remove("IPHONEOS_DEPLOYMENT_TARGET")
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!(
                        "cargo:warning=Addon '{}' — kompilacja WASM zakonczona pomyslnie",
                        addon_name
                    );
                }
                Ok(s) => {
                    println!(
                        "cargo:warning=Addon '{}' — blad kompilacji WASM (kod: {}), pomijam",
                        addon_name, s
                    );
                    continue;
                }
                Err(e) => {
                    println!(
                        "cargo:warning=Addon '{}' — nie udalo sie uruchomic cargo: {}, pomijam",
                        addon_name, e
                    );
                    continue;
                }
            }

            // Znajdz skompilowany .wasm — nazwa crate z Cargo.toml (zamien '-' na '_')
            let wasm_crate_name = read_crate_name(&addon_dir)
                .unwrap_or_else(|| format!("tentaflow_addon_{}", addon_name));
            let wasm_filename = format!("{}.wasm", wasm_crate_name);
            let wasm_path = addon_target
                .join("wasm32-wasip1/release")
                .join(&wasm_filename);

            if !wasm_path.exists() {
                println!(
                    "cargo:warning=Addon '{}' — brak pliku .wasm: {}, pomijam",
                    addon_name,
                    wasm_path.display()
                );
                continue;
            }

            // Validate WASM imports — all host functions MUST use the
            // "tentaflow" namespace (not "env"). Bare `extern "C"` without
            // `#[link(wasm_import_module = "tentaflow")]` defaults to "env"
            // and silently breaks at runtime. Fail the build early.
            if let Err(bad_imports) = validate_wasm_imports(&wasm_path) {
                panic!(
                    "\n\nAddon '{}' — WASM import namespace error!\n\
                     The following imports use the \"env\" module instead of \"tentaflow\":\n\
                     {}\n\n\
                     Fix: add #[link(wasm_import_module = \"tentaflow\")] to the extern \"C\" block \
                     in the addon's src/lib.rs.\n",
                    addon_name, bad_imports
                );
            }

            // Skopiuj bundle (wasm + metadane) do OUT_DIR
            let bundle_addon_dir = bundle_dir.join(&addon_name);
            std::fs::create_dir_all(&bundle_addon_dir).unwrap();

            // Kopiuj WASM
            std::fs::copy(&wasm_path, bundle_addon_dir.join("addon.wasm")).unwrap();

            // Kopiuj metadane
            std::fs::copy(
                addon_dir.join("manifest.toml"),
                bundle_addon_dir.join("manifest.toml"),
            )
            .unwrap();

            for file in &["SKILL.md", "DESCRIPTION.md", "blocks.json", "icon.png"] {
                let src = addon_dir.join(file);
                if src.exists() {
                    std::fs::copy(&src, bundle_addon_dir.join(file)).ok();
                }
            }

            // Kopiuj migracje jesli sa
            let migrations_dir = addon_dir.join("migrations");
            if migrations_dir.exists() {
                let dest_migrations = bundle_addon_dir.join("migrations");
                std::fs::create_dir_all(&dest_migrations).unwrap();
                if let Ok(entries) = std::fs::read_dir(&migrations_dir) {
                    for m in entries.flatten() {
                        std::fs::copy(m.path(), dest_migrations.join(m.file_name())).ok();
                    }
                }
            }

            // Kopiuj flows jesli sa
            let flows_dir = addon_dir.join("flows");
            if flows_dir.exists() {
                let dest_flows = bundle_addon_dir.join("flows");
                std::fs::create_dir_all(&dest_flows).unwrap();
                if let Ok(entries) = std::fs::read_dir(&flows_dir) {
                    for f in entries.flatten() {
                        std::fs::copy(f.path(), dest_flows.join(f.file_name())).ok();
                    }
                }
            }

            bundled_addons.push(BundledAddonInfo {
                name: addon_name,
                bundle_path: bundle_addon_dir,
            });
        }
    } // koniec for addons_dir

    // Generuj plik Rust z osadzonymi danymi addonow
    generate_bundled_rs(&out_dir, &bundled_addons);
}

// =============================================================================
// GPU crop-preprocess CUDA kernel — nvcc compile + link flags
// =============================================================================

/// Compiles the fused-preprocess CUDA kernels (`cuda/crop_resize_normalize.cu`
/// and `cuda/nv12_to_rgb_resize_normalize.cu`) into static libs in OUT_DIR via
/// nvcc i emituje flagi linkera dla bibliotek oraz runtime CUDA. Jawna funkcja
/// `vision-cuda-preprocess` włącza tę ścieżkę, więc buildy AMD/Intel nie wywołują
/// nvcc. `--fmad=false` zachowuje zgodność obliczeń próbkowania f64 z CPU
/// `resize_rgb` (kontrakcja FMA może zmienić zaokrąglenie na granicy Q8).
fn compile_cuda_preprocess(out_dir: &Path) {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let want = matches!(target_os.as_deref(), Ok("linux" | "windows"))
        && std::env::var_os("CARGO_FEATURE_INFERENCE_VISION_GPU").is_some()
        && std::env::var_os("CARGO_FEATURE_VISION_ORT").is_some()
        && std::env::var_os("CARGO_FEATURE_VISION_CUDA_PREPROCESS").is_some();
    if !want {
        return;
    }

    let nvcc = locate_nvcc();
    // CUDA lib dir is <cuda-root>/lib64 (nvcc lives in <cuda-root>/bin).
    let cuda_lib_dir = nvcc
        .parent()
        .and_then(|bin| bin.parent())
        .map(|root| root.join("lib64"))
        .filter(|p| p.is_dir());

    // Each fused kernel compiles to its own static lib in OUT_DIR. `--fmad=false`
    // keeps the shared f64 sampling math bit-for-bit with the CPU `resize_rgb`.
    for (src, lib) in [
        ("cuda/crop_resize_normalize.cu", "libtf_crop_resize.a"),
        (
            "cuda/nv12_to_rgb_resize_normalize.cu",
            "libtf_nv12_preprocess.a",
        ),
    ] {
        let cu = Path::new(src);
        if !cu.exists() {
            panic!("CUDA preprocess source missing: {}", cu.display());
        }
        println!("cargo:rerun-if-changed={src}");

        let lib_path = out_dir.join(lib);
        let status = Command::new(&nvcc)
            .arg("-O3")
            .arg("-Xcompiler")
            .arg("-fPIC")
            .arg("--fmad=false")
            .arg("-lib")
            .arg(cu)
            .arg("-o")
            .arg(&lib_path)
            .status()
            .unwrap_or_else(|e| panic!("failed to run nvcc ({}): {e}", nvcc.display()));
        if !status.success() {
            panic!("nvcc failed to compile {} (status {status})", cu.display());
        }
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=tf_crop_resize");
    println!("cargo:rustc-link-lib=static=tf_nv12_preprocess");
    if let Some(dir) = &cuda_lib_dir {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    // Debian/Ubuntu ship libcudart in the multiarch dir; keep both searchable.
    println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu");
    println!("cargo:rustc-link-lib=dylib=cudart");
    // nvcc-generated host stubs pull the C++ runtime.
    println!("cargo:rustc-link-lib=dylib=stdc++");
}

/// Locates nvcc: `$NVCC`, then the pinned/standard CUDA install paths, then PATH.
fn locate_nvcc() -> PathBuf {
    if let Ok(p) = std::env::var("NVCC") {
        return PathBuf::from(p);
    }
    for cand in ["/usr/local/cuda-13.0/bin/nvcc", "/usr/local/cuda/bin/nvcc"] {
        if Path::new(cand).exists() {
            return PathBuf::from(cand);
        }
    }
    PathBuf::from("nvcc")
}

// =============================================================================
// Automatyczne generowanie certyfikatow TLS (self-signed)
// =============================================================================

/// Sprawdza czy certyfikaty TLS istnieja w ../certs/ — jesli nie, generuje
/// self-signed certyfikat EC (prime256r1) wazny 10 lat za pomoca openssl CLI.
fn generate_self_signed_certs() {
    let certs_dir = Path::new("../certs");
    let cert_path = certs_dir.join("cert.pem");
    let key_path = certs_dir.join("key.pem");

    // Przebuduj jesli certyfikat zostanie usuniety
    println!("cargo:rerun-if-changed=../certs/cert.pem");

    if cert_path.exists() && key_path.exists() {
        return;
    }

    println!(
        "cargo:warning=Certyfikaty TLS nie znalezione — generuje self-signed (rcgen, pure Rust)..."
    );

    // Utworz katalog certs/ jesli nie istnieje
    if let Err(e) = std::fs::create_dir_all(certs_dir) {
        println!(
            "cargo:warning=Nie udalo sie utworzyc katalogu certs/: {}. \
             Utworz go recznie i uruchom build ponownie.",
            e
        );
        return;
    }

    // Generuj self-signed cert z rcgen — EC P-256, wazny 10 lat
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .expect("Blad generowania klucza EC P-256");

    let mut params = rcgen::CertificateParams::new(vec!["tentaflow".to_string()])
        .expect("Blad tworzenia CertificateParams");
    params.not_before = rcgen::date_time_ymd(2025, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .expect("Blad generowania certyfikatu self-signed");

    if let Err(e) = std::fs::write(&cert_path, cert.pem()) {
        println!("cargo:warning=Nie udalo sie zapisac cert.pem: {}", e);
        return;
    }
    if let Err(e) = std::fs::write(&key_path, key_pair.serialize_pem()) {
        println!("cargo:warning=Nie udalo sie zapisac key.pem: {}", e);
        return;
    }

    println!("cargo:warning=Certyfikaty TLS wygenerowane pomyslnie w certs/ (EC P-256, 10 lat)");
}

// =============================================================================
// Struktury pomocnicze
// =============================================================================

struct BundledAddonInfo {
    name: String,
    bundle_path: PathBuf,
}

// =============================================================================
// Sprawdzanie dostepnosci wasm32-wasip1 target
// =============================================================================

fn check_wasm_target() -> bool {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.lines().any(|line| line.trim() == "wasm32-wasip1")
        }
        Err(_) => {
            println!(
                "cargo:warning=Nie udalo sie uruchomic rustup — pomijam sprawdzanie WASM target"
            );
            false
        }
    }
}

// Wspoldzielony katalog target dla buildow WASM wszystkich addonow. Sibling
// `target_shared` (repo_root/target-addon-wasm) — NIE pod target_shared, zeby
// miec osobny lock pliku i nie deadlockowac z parent cargo trzymajacym lock na
// target_shared. Dzieki wspoldzieleniu addon-sdk + wspolne deps kompiluja sie
// raz dla wszystkich addonow zamiast osobno per addon.
fn shared_addon_wasm_target() -> PathBuf {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest.parent().unwrap_or(&manifest);
    repo_root.join("target-addon-wasm")
}

// =============================================================================
// Addony .NET — dotnet publish (NativeAOT-LLVM) do wasm32-wasip1
// =============================================================================

/// Odczytuje `runtime = "..."` z sekcji `[addon]` manifestu. Zwraca None gdy
/// pole nie istnieje. Parser jest liniowy (jak reszta build.rs) i czyta tylko
/// dopoki jest w sekcji [addon], zeby nie zlapac `runtime` z innej sekcji.
fn read_manifest_runtime(manifest_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(manifest_path).ok()?;
    let mut in_addon = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_addon = trimmed == "[addon]";
            continue;
        }
        if in_addon && trimmed.starts_with("runtime") {
            if let Some(val) = extract_toml_string_value(trimmed) {
                return Some(val);
            }
        }
    }
    None
}

/// Znajduje plik .csproj w katalogu addonu (poziom root).
fn find_csproj(addon_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(addon_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "csproj").unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

/// Zbiera pliki .cs addonu (rekursywnie, pomijajac bin/ i obj/).
fn list_cs_sources(addon_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![addon_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name != "bin" && name != "obj" {
                    stack.push(path);
                }
            } else if path.extension().map(|e| e == "cs").unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out
}

/// Zwraca sciezke do WASI SDK: env WASI_SDK_PATH albo auto-detekcja w cache
/// natywnym TentaFlow (katalog `wasi-sdk-*` w TENTAFLOW_NATIVE_CACHE).
fn resolve_wasi_sdk() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WASI_SDK_PATH") {
        let path = PathBuf::from(p);
        if path.join("share/wasi-sysroot").exists() {
            return Some(path);
        }
    }
    let cache_root = std::env::var("TENTAFLOW_NATIVE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache")
                });
            base.join("tentaflow-native-libs")
        });
    let entries = std::fs::read_dir(&cache_root).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("wasi-sdk-"))
                    .unwrap_or(false)
                && p.join("share/wasi-sysroot").exists()
        })
        .collect();
    candidates.sort();
    candidates.pop()
}

/// Buduje addon .NET przez `dotnet publish -r wasi-wasm` (NativeAOT-LLVM,
/// bare module wasip1 przez IlcLlvmTarget + LinkerFlavor z Directory.Build.rsp
/// addonu). Zwraca sciezke do wynikowego .wasm albo None (skip z warningiem) —
/// jak Rustowa sciezka przy braku targetu wasm32-wasip1.
fn build_dotnet_addon(addon_dir: &Path, addon_name: &str) -> Option<PathBuf> {
    let dotnet_ok = Command::new("dotnet")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !dotnet_ok {
        println!(
            "cargo:warning=Addon '{}' — pomijam: brak dotnet SDK w PATH \
             (zainstaluj .NET 10 SDK aby budowac addony C#)",
            addon_name
        );
        return None;
    }

    let Some(wasi_sdk) = resolve_wasi_sdk() else {
        println!(
            "cargo:warning=Addon '{}' — pomijam: brak WASI SDK \
             (ustaw WASI_SDK_PATH albo rozpakuj wasi-sdk-25+ do \
             ~/.cache/tentaflow-native-libs/)",
            addon_name
        );
        return None;
    };

    let status = Command::new("dotnet")
        .args(["publish", "-c", "Release", "-r", "wasi-wasm"])
        .current_dir(addon_dir)
        .env("WASI_SDK_PATH", &wasi_sdk)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!(
                "cargo:warning=Addon '{}' — blad dotnet publish (kod: {}), pomijam",
                addon_name, s
            );
            return None;
        }
        Err(e) => {
            println!(
                "cargo:warning=Addon '{}' — nie udalo sie uruchomic dotnet: {}, pomijam",
                addon_name, e
            );
            return None;
        }
    }

    // Wynik: bin/Release/net*/wasi-wasm/publish/<Assembly>.wasm — bierzemy
    // jedyny plik .wasm z katalogu publish.
    let release_dir = addon_dir.join("bin/Release");
    let mut wasm_files: Vec<PathBuf> = Vec::new();
    if let Ok(tfms) = std::fs::read_dir(&release_dir) {
        for tfm in tfms.flatten() {
            let publish = tfm.path().join("wasi-wasm/publish");
            if let Ok(files) = std::fs::read_dir(&publish) {
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().map(|e| e == "wasm").unwrap_or(false) {
                        wasm_files.push(p);
                    }
                }
            }
        }
    }
    match wasm_files.len() {
        1 => {
            println!(
                "cargo:warning=Addon '{}' — kompilacja WASM (.NET) zakonczona pomyslnie",
                addon_name
            );
            wasm_files.pop()
        }
        0 => {
            println!(
                "cargo:warning=Addon '{}' — dotnet publish nie wytworzyl pliku .wasm, pomijam",
                addon_name
            );
            None
        }
        n => {
            println!(
                "cargo:warning=Addon '{}' — znaleziono {} plikow .wasm w publish, pomijam",
                addon_name, n
            );
            None
        }
    }
}

/// Kopiuje pliki (plasko) z katalogu zrodlowego do docelowego, jesli istnieje.
fn copy_dir_flat(src: &Path, dest: &Path) {
    if !src.exists() {
        return;
    }
    std::fs::create_dir_all(dest).unwrap();
    if let Ok(entries) = std::fs::read_dir(src) {
        for e in entries.flatten() {
            std::fs::copy(e.path(), dest.join(e.file_name())).ok();
        }
    }
}

// =============================================================================
// Odczyt nazwy crate z Cargo.toml addonu
// =============================================================================

fn read_crate_name(addon_dir: &Path) -> Option<String> {
    let cargo_toml = std::fs::read_to_string(addon_dir.join("Cargo.toml")).ok()?;

    // Prosty parser — szukamy name = "..." w sekcji [package] lub [lib]
    // Preferuj [lib] name jesli istnieje, bo to nazwa wynikowego .wasm
    let mut in_lib = false;
    let mut lib_name = None;
    let mut package_name = None;

    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_lib = trimmed == "[lib]";
        }
        if trimmed.starts_with("name") {
            if let Some(val) = extract_toml_string_value(trimmed) {
                if in_lib {
                    lib_name = Some(val);
                } else if package_name.is_none() {
                    package_name = Some(val);
                }
            }
        }
    }

    // Nazwa WASM to lib name (jesli [lib] jest cdylib) lub package name z '-' -> '_'
    let name = lib_name.or(package_name)?;
    Some(name.replace('-', "_"))
}

fn extract_toml_string_value(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() != 2 {
        return None;
    }
    let val = parts[1].trim().trim_matches('"');
    Some(val.to_string())
}

// =============================================================================
// Generowanie bundled_addons.rs z include_bytes!/include_str!
// =============================================================================

fn generate_bundled_rs(out_dir: &Path, addons: &[BundledAddonInfo]) {
    let mut code = String::new();

    code.push_str(
        "// =============================================================================\n",
    );
    code.push_str("// Plik: bundled_addons.rs (auto-generated by build.rs)\n");
    code.push_str("// Opis: Osadzone addony WASM — skompilowane z binarka.\n");
    code.push_str("//       NIE EDYTUJ RECZNIE — generowane automatycznie.\n");
    code.push_str(
        "// =============================================================================\n\n",
    );

    code.push_str("/// Pojedynczy wbudowany addon\n");
    code.push_str("pub struct BundledAddon {\n");
    code.push_str("    /// Nazwa addonu (identyfikator katalogu)\n");
    code.push_str("    pub name: &'static str,\n");
    code.push_str("    /// Skompilowany modul WASM\n");
    code.push_str("    pub wasm_bytes: &'static [u8],\n");
    code.push_str("    /// Zawartosc manifest.toml\n");
    code.push_str("    pub manifest_toml: &'static str,\n");
    code.push_str("    /// Zawartosc SKILL.md (moze byc pusta)\n");
    code.push_str("    pub skill_md: &'static str,\n");
    code.push_str("    /// Zawartosc DESCRIPTION.md (moze byc pusta)\n");
    code.push_str("    pub description_md: &'static str,\n");
    code.push_str("    /// Zawartosc blocks.json (moze byc pusta)\n");
    code.push_str("    pub blocks_json: &'static str,\n");
    code.push_str("    /// Pliki migracji SQL addona\n");
    code.push_str("    pub migrations: &'static [(&'static str, &'static str)],\n");
    code.push_str("    /// Pliki flow JSON addona\n");
    code.push_str("    pub flows: &'static [(&'static str, &'static str)],\n");
    code.push_str("}\n\n");

    code.push_str("/// Lista wszystkich wbudowanych addonow\n");
    code.push_str("pub const BUNDLED_ADDONS: &[BundledAddon] = &[\n");

    for addon in addons {
        let wasm_path = addon.bundle_path.join("addon.wasm");
        let manifest_path = addon.bundle_path.join("manifest.toml");
        let skill_path = addon.bundle_path.join("SKILL.md");
        let desc_path = addon.bundle_path.join("DESCRIPTION.md");
        let blocks_path = addon.bundle_path.join("blocks.json");
        let migrations_path = addon.bundle_path.join("migrations");
        let flows_path = addon.bundle_path.join("flows");

        // Plik WASM i manifest musza istniec
        if !wasm_path.exists() || !manifest_path.exists() {
            continue;
        }

        code.push_str("    BundledAddon {\n");
        code.push_str(&format!("        name: \"{}\",\n", addon.name));
        code.push_str(&format!(
            "        wasm_bytes: include_bytes!(\"{}\"),\n",
            escape_path(&wasm_path)
        ));
        code.push_str(&format!(
            "        manifest_toml: include_str!(\"{}\"),\n",
            escape_path(&manifest_path)
        ));

        if skill_path.exists() {
            code.push_str(&format!(
                "        skill_md: include_str!(\"{}\"),\n",
                escape_path(&skill_path)
            ));
        } else {
            code.push_str("        skill_md: \"\",\n");
        }

        if desc_path.exists() {
            code.push_str(&format!(
                "        description_md: include_str!(\"{}\"),\n",
                escape_path(&desc_path)
            ));
        } else {
            code.push_str("        description_md: \"\",\n");
        }

        if blocks_path.exists() {
            code.push_str(&format!(
                "        blocks_json: include_str!(\"{}\"),\n",
                escape_path(&blocks_path)
            ));
        } else {
            code.push_str("        blocks_json: \"\",\n");
        }

        code.push_str("        migrations: &[\n");
        if migrations_path.exists() {
            let mut migrations = std::fs::read_dir(&migrations_path)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            migrations.sort_by_key(|entry| entry.file_name());
            for migration in migrations {
                let path = migration.path();
                let name = migration.file_name().to_string_lossy().to_string();
                code.push_str(&format!(
                    "            (\"{}\", include_str!(\"{}\")),\n",
                    name,
                    escape_path(&path)
                ));
            }
        }
        code.push_str("        ],\n");

        code.push_str("        flows: &[\n");
        if flows_path.exists() {
            let mut flows = std::fs::read_dir(&flows_path)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            flows.sort_by_key(|entry| entry.file_name());
            for flow in flows {
                let path = flow.path();
                let name = flow.file_name().to_string_lossy().to_string();
                code.push_str(&format!(
                    "            (\"{}\", include_str!(\"{}\")),\n",
                    name,
                    escape_path(&path)
                ));
            }
        }
        code.push_str("        ],\n");

        code.push_str("    },\n");
    }

    code.push_str("];\n");

    let bundled_path = out_dir.join("bundled_addons.rs");
    std::fs::write(&bundled_path, code).unwrap();
}

/// Escapuje sciezke dla uzycia w include_bytes!/include_str! (backslashe na /)
fn escape_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

// =============================================================================
// Generowanie wwwroot_embed.rs — pliki statyczne dashboardu
// =============================================================================

/// Skanuje www/ rekurencyjnie i generuje wwwroot_embed.rs z include_bytes!
/// dla kazdego pliku. Rejestruje rerun-if-changed na kazdym pliku zeby cargo
/// automatycznie rekompilowalo po zmianie jakiegokolwiek zasobu www.
fn generate_wwwroot_embed(out_dir: &Path) {
    use sha2::{Digest, Sha256};
    let wwwroot = Path::new("www");
    if !wwwroot.exists() {
        // Brak www — generuj pusta funkcje lookup
        let code =
            "fn wwwroot_lookup(_path: &str) -> Option<(&'static str, &'static [u8], &'static str)> { None }\n";
        std::fs::write(out_dir.join("wwwroot_embed.rs"), code).unwrap();
        return;
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_wwwroot_files(wwwroot, wwwroot, &mut files);

    // Rejestruj rerun-if-changed na kazdym pliku — cargo ZAWSZE rekompiluje
    // gdy jakikolwiek plik www sie zmieni
    println!("cargo:rerun-if-changed=www");
    for (_, abs_path) in &files {
        println!("cargo:rerun-if-changed={}", abs_path.display());
    }

    let mut code = String::new();
    code.push_str("// Auto-generated by build.rs — NIE EDYTUJ RECZNIE\n\n");

    // Generuj stale z include_bytes! dla kazdego pliku
    for (i, (rel_path, abs_path)) in files.iter().enumerate() {
        code.push_str(&format!(
            "static WWWROOT_FILE_{}: &[u8] = include_bytes!(\"{}\");\n",
            i,
            escape_path(abs_path)
        ));
        let _ = rel_path; // uzywany nizej w lookup
    }

    code.push_str("\n");

    // Generuj funkcje lookup — trzeci element to ETag (per-plik SHA-256, 16 hex).
    // Serwer uzywa go do warunkowych GET (If-None-Match -> 304), wiec caching w
    // przegladarce dziala ZAWSZE, niezaleznie od service workera/certa.
    code.push_str(
        "fn wwwroot_lookup(path: &str) -> Option<(&'static str, &'static [u8], &'static str)> {\n",
    );
    code.push_str("    match path {\n");

    for (i, (rel_path, abs_path)) in files.iter().enumerate() {
        let mime = guess_mime(rel_path);
        let bytes = std::fs::read(abs_path).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(&bytes);
        let etag: String = h
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{:02x}", b))
            .collect();
        code.push_str(&format!(
            "        \"{}\" => Some((\"{}\", WWWROOT_FILE_{}, \"{}\")),\n",
            rel_path, mime, i, etag
        ));
    }

    code.push_str("        _ => None,\n");
    code.push_str("    }\n");
    code.push_str("}\n");

    std::fs::write(out_dir.join("wwwroot_embed.rs"), code).unwrap();
}

/// Zapisuje plik tylko gdy tresc sie zmienila — bez tego przepisywanie
/// identycznej tresci bije mtime i wpada w petle rebuildu (rerun-if-changed=www).
fn write_if_changed(path: &Path, content: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == content {
            return;
        }
    }
    std::fs::write(path, content).unwrap();
}

/// Skanuje www/ i liczy zbiorczy SHA-256 calego frontu (ASSET_BUILD_HASH).
/// Generuje trzy artefakty z tego samego hasha:
///   - www/js/generated/asset-manifest.js — lista wszystkich zasobow +
///     ASSET_BUILD_HASH (browser: precache w service workerze + porownanie
///     w handshake WS),
///   - www/js/generated/sw-version.js — importScripts w service workerze; zmiana
///     hasha zmienia bajty importu => browser wykrywa update SW i przecacheowuje,
///   - $OUT_DIR/asset_build_hash.rs — staly Rust wysylany w MetaSchemaVersionAck.
/// Dzieki temu KAZDA zmiana frontu (JS/CSS/wasm glue/panel addona) wywoluje
/// odswiezenie cache i wykrycie nieaktualnego frontu przy (re)connect WS.
fn generate_asset_manifest(out_dir: &Path) {
    use sha2::{Digest, Sha256};

    let wwwroot = Path::new("www");
    // Pliki generowane w tym kroku wykluczamy z hasha (self-reference) —
    // inaczej ich wlasna tresc zmienialaby hash w nieskonczonosc.
    const SELF: &[&str] = &[
        "js/generated/asset-manifest.js",
        "js/generated/sw-version.js",
    ];

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if wwwroot.exists() {
        collect_wwwroot_files(wwwroot, wwwroot, &mut files);
    }
    // Deterministyczna kolejnosc — hash niezalezny od kolejnosci read_dir.
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut agg = Sha256::new();
    let mut list: Vec<String> = Vec::new();
    for (rel, abs) in &files {
        if SELF.contains(&rel.as_str()) {
            continue;
        }
        let bytes = std::fs::read(abs).unwrap_or_default();
        let mut fh = Sha256::new();
        fh.update(&bytes);
        let digest = fh.finalize();
        agg.update(rel.as_bytes());
        agg.update([0u8]);
        agg.update(digest);
        list.push(format!("/{}", rel));
    }
    let full: String = agg
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    let hash = &full[..16];

    let mut js = String::new();
    js.push_str("// Auto-generated by build.rs — NIE EDYTUJ RECZNIE\n");
    js.push_str(&format!("export const ASSET_BUILD_HASH = \"{}\";\n", hash));
    js.push_str("export const ASSET_MANIFEST = [\n");
    for p in &list {
        js.push_str(&format!("  {:?},\n", p));
    }
    js.push_str("];\n");
    write_if_changed(&wwwroot.join("js/generated/asset-manifest.js"), &js);

    // Classic worker importScripts — service worker nie moze importowac ESM,
    // wiec hash i pelna lista sa tu jako globalne (self.__ASSET_*).
    let mut sw = String::new();
    sw.push_str("// Auto-generated by build.rs — NIE EDYTUJ RECZNIE\n");
    sw.push_str(&format!("self.__ASSET_BUILD_HASH = {:?};\n", hash));
    sw.push_str("self.__ASSET_MANIFEST = [\n");
    for p in &list {
        sw.push_str(&format!("  {:?},\n", p));
    }
    sw.push_str("];\n");
    write_if_changed(&wwwroot.join("js/generated/sw-version.js"), &sw);

    std::fs::write(
        out_dir.join("asset_build_hash.rs"),
        format!("pub const ASSET_BUILD_HASH: &str = {:?};\n", hash),
    )
    .unwrap();
}

/// Rekurencyjnie zbiera pliki z katalogu www.
fn collect_wwwroot_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_wwwroot_files(base, &path, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let abs = std::fs::canonicalize(&path).unwrap_or(path.clone());
            out.push((rel, abs));
        }
    }
}

/// Okreslenie MIME type na podstawie rozszerzenia pliku
fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        "webp" => "image/webp",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Czy to build bez ANI JEDNEGO lokalnego silnika inferencji (edycja slim).
/// Czytane z features samego rdzenia, wiec sygnal jest ten sam niezaleznie od
/// tego, kto sklada liste features (binarka, testy, inny crate).
fn is_slim_edition() -> bool {
    const LOCAL_ENGINE_FEATURES: &[&str] = &[
        "CARGO_FEATURE_INFERENCE_WHISPER",
        "CARGO_FEATURE_INFERENCE_LLAMACPP",
        "CARGO_FEATURE_INFERENCE_SHERPA",
        "CARGO_FEATURE_INFERENCE_SUPERTONIC",
        "CARGO_FEATURE_INFERENCE_VISION_GPU",
        "CARGO_FEATURE_INFERENCE_MLX",
    ];
    LOCAL_ENGINE_FEATURES
        .iter()
        .all(|f| std::env::var_os(f).is_none())
}

// =============================================================================
// Pakowanie kontekstu Docker (tentaflow-containers + shared Rust crates)
// w tar.gz wbudowany w binarce. Pozwala na deploy bez zewnetrznych zrodel.
// =============================================================================

fn pack_container_contexts(out_dir: &Path) {
    use std::process::Command;

    let workspace_root = Path::new("..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(".."));
    let containers_dir = workspace_root.join("tentaflow-containers");
    let protocol_dir = workspace_root.join("tentaflow-protocol");
    let transport_dir = workspace_root.join("tentaflow-transport");
    let voice_dir = workspace_root.join("tentaflow-voice");
    // vendor/ trzyma patched ed25519-dalek wymagany przez [patch.crates-io]
    // w tentaflow-containers/sidecar/Cargo.toml. Bez tego docker build sidecara
    // pada na "vendor: not found" przy pierwszym RUN cargo build.
    let vendor_dir = workspace_root.join("vendor");

    if !containers_dir.exists()
        || !protocol_dir.exists()
        || !transport_dir.exists()
        || !voice_dir.exists()
        || !vendor_dir.exists()
    {
        println!(
            "cargo:warning=pack_container_contexts: brak jednego z wymaganych katalogow: {}, {}, {}, {}, {} — embed pominiety",
            containers_dir.display(),
            protocol_dir.display(),
            transport_dir.display(),
            voice_dir.display(),
            vendor_dir.display()
        );
        // Stworz pusty plik zeby include_bytes! nie padlo
        std::fs::write(out_dir.join("container_bundle.tar.gz"), b"").ok();
        return;
    }

    // Zmiany w kontekstach trigerują rebuild. KONTENERY (Dockerfile'e, server.py,
    // entrypointy) embedujemy jako dane — cargo ich NIE śledzi normalnie, a
    // `rerun-if-changed=<katalog>` łapie tylko add/remove, NIE edycję treści
    // istniejących plików. Bez per-pliku `git pull` zmieniający server.py/Dockerfile
    // + `cargo run` NIE re-embeduje bundla → binarka trzyma stare kontenery, brak
    // "Aktualizacja dostępna", a obraz dalej buduje się ze starego źródła (np. /detect
    // zamiast /v1/infer, OCR bez TORCH_CUDA_ARCH_LIST).
    rerun_if_changed_recursive(&containers_dir);
    println!("cargo:rerun-if-changed={}", protocol_dir.display());
    println!("cargo:rerun-if-changed={}", transport_dir.display());
    println!("cargo:rerun-if-changed={}", voice_dir.display());
    println!("cargo:rerun-if-changed={}", vendor_dir.display());
    println!("cargo:rerun-if-changed=src/services/manifest/vocabulary.rs");

    let bundle_path = out_dir.join("container_bundle.tar.gz");

    // Bundlujemy DEFINICJE kontenerów, nie zbudowane artefakty. Wykluczamy
    // build/runtime śmieci, żeby nie wciskać GB do binarki (rmeta!): target/,
    // node_modules/, .git/, ale TEŻ wirtualne środowiska Pythona (.venv/venv —
    // deploy tworzy je przez `uv sync` WEWNĄTRZ katalogu bundla; bez tego
    // wykluczenia trafiały do paczki → rmeta tentaflow-core puchło do 12 GB),
    // __pycache__/*.pyc, instancje/szablony bundli oraz wagi modeli.
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&bundle_path)
        .arg("--exclude=target")
        .arg("--exclude=node_modules")
        .arg("--exclude=.git")
        .arg("--exclude=.venv")
        .arg("--exclude=venv")
        .arg("--exclude=__pycache__")
        .arg("--exclude=*.pyc")
        .arg("--exclude=bundle-instances")
        .arg("--exclude=bundle-templates")
        .arg("--exclude=*.pth")
        .arg("--exclude=*.onnx")
        .arg("--exclude=*.gguf")
        .arg("--exclude=*.safetensors")
        .arg("-C")
        .arg(&workspace_root)
        .arg("tentaflow-containers")
        .arg("tentaflow-protocol")
        .arg("tentaflow-transport")
        .arg("tentaflow-voice")
        .arg("vendor")
        .status();

    match status {
        Ok(s) if s.success() => {
            let size = std::fs::metadata(&bundle_path)
                .map(|m| m.len())
                .unwrap_or(0);
            println!(
                "cargo:warning=container_bundle.tar.gz spakowany ({} KB)",
                size / 1024
            );
        }
        _ => {
            println!("cargo:warning=tar nieudany — embed kontenerow nie zadzialal");
            std::fs::write(&bundle_path, b"").ok();
        }
    }
}

// =============================================================================
// Generator manifestu serwisow — skanuje tentaflow-containers/*/_services/*.toml,
// waliduje semantycznie 4 reguly ze SCHEMA.md i emituje:
//   - $OUT_DIR/services_generated.rs       (Rust const z embedded JSON)
//   - www/js/generated/services-manifest.js  (ESM module dla GUI)
//
// UWAGA: typy serde sa duplikatem z src/services/manifest/types.rs.
// To wymuszone — build.rs i lib to osobne crates i nie moga dzielic kodu
// bez wydzielania osobnego mini-crate.
// =============================================================================

mod services_manifest_build {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ServiceManifest {
        pub engine: Engine,
        pub deploy: DeploySection,
        #[serde(default, rename = "model_preset")]
        pub model_presets: Vec<ModelPreset>,
        /// Typed parameter schema dla wizard formularza i auto-tunera.
        /// Mirror runtime types z `services/manifest/types.rs::EngineParameter`.
        #[serde(default, rename = "parameter")]
        pub parameters: Vec<EngineParameter>,
        /// Sha256 of the docker build context tree; empty string when the
        /// manifest has no [deploy.docker] or uses compose_path only.
        #[serde(default)]
        pub docker_source_hash: String,
        /// Sha256 of the native build tree (binary/python-bundle); empty for
        /// embedded/external runtimes.
        #[serde(default)]
        pub native_source_hash: String,
        /// Mirror of runtime `ServiceManifest.required_assets` — runtime model
        /// files the bundle excludes (`*.onnx` & friends). Must be carried
        /// through, otherwise deploy never learns what to fetch.
        #[serde(default, rename = "required_asset")]
        pub required_assets: Vec<RequiredAsset>,
    }

    /// Mirror runtime `RequiredAsset`.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RequiredAsset {
        pub path: String,
        pub mount_path: String,
        pub url: String,
        pub sha256: String,
        #[serde(default)]
        pub repo_path: Option<String>,
        #[serde(default)]
        pub env_var: Option<String>,
    }

    /// Mirror runtime `EngineParameter`. Build-time type — synchronizowany
    /// ręcznie z `services/manifest/types.rs`.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct EngineParameter {
        pub key: String,
        pub label_pl: String,
        pub label_en: String,
        pub kind: ParameterKind,
        #[serde(default)]
        pub range: Option<NumRange>,
        #[serde(default)]
        pub options: Option<Vec<String>>,
        pub default: serde_json::Value,
        pub bindings: Vec<ParameterBinding>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum ParameterKind {
        Float,
        Int,
        Bool,
        Enum,
        String,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub struct NumRange {
        pub min: f64,
        pub max: f64,
        #[serde(default)]
        pub step: Option<f64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ParameterBinding {
        pub when: DeployTarget,
        #[serde(flatten)]
        pub target: BindingTarget,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum DeployTarget {
        Docker,
        NativeEmbedded,
        NativePythonBundle,
        NativeBinary,
        External,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum BindingTarget {
        Env {
            name: String,
        },
        LlamacppField {
            field: String,
        },
        WhisperField {
            field: String,
            #[serde(default)]
            request_override: bool,
        },
        MlxField {
            field: String,
            #[serde(default)]
            request_override: bool,
        },
        OllamaOptions {
            key: String,
        },
        PythonRequestBody {
            field: String,
        },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Engine {
        pub id: String,
        pub category: Category,
        pub name: String,
        pub description_pl: String,
        pub description_en: String,
        pub homepage: String,
        pub license: String,
        #[serde(default)]
        pub icon: Option<String>,
        #[serde(default)]
        pub resource_kind: Option<ResourceKind>,
        /// Mirror runtime Engine.provider.
        #[serde(default)]
        pub provider: Option<String>,
        #[serde(default)]
        pub requires_model: Option<bool>,
        #[serde(default)]
        pub gpu_supported: Option<bool>,
        /// Mirror of runtime `Engine.reverse_requests` — sidecars allowed to
        /// open streams back to Core (meeting bot).
        #[serde(default)]
        pub reverse_requests: bool,
        /// Tri-state DGX Spark gate. Mirror of runtime `Engine.dgx_spark`.
        #[serde(default)]
        pub dgx_spark: Option<bool>,
        /// Mirror of runtime `Engine.cluster_capable` — carried through so the
        /// GUI deploy wizard can offer the engine for cluster targets.
        #[serde(default)]
        pub cluster_capable: Option<bool>,
        /// Mirror of runtime `Engine.cluster_launch` ("ray" default / "vllm-mp").
        #[serde(default)]
        pub cluster_launch: Option<String>,
        /// Mirror of runtime `Engine.preset_only` — katalog chowa karte takiego
        /// silnika i pokazuje wylacznie jego kafelki modeli.
        #[serde(default)]
        pub preset_only: Option<bool>,
        pub default_port: u16,
        pub api: ApiKind,
        pub version: String,
        /// Three independent capability axes (D.12). Each is `None` when
        /// the manifest defers to category defaults; an explicit empty
        /// list is invalid and rejected by validation.
        #[serde(default)]
        pub service_surfaces: Option<Vec<String>>,
        #[serde(default)]
        pub input_modalities: Option<Vec<String>>,
        #[serde(default)]
        pub output_modalities: Option<Vec<String>>,
        /// Mirror runtime `Engine.backend` — `"vllm"` for every vLLM-served
        /// engine. Must be carried through to the generated manifest so the GUI
        /// (deploy wizard + service editor) can gate the VRAM calculator on it.
        #[serde(default)]
        pub backend: Option<String>,
        /// Mirror of runtime `Engine.lifecycle`. Without it the field would be
        /// dropped when the TOML is re-serialised into the embedded JSON, and
        /// an `on-demand` engine would be deployed as a long-lived service.
        #[serde(default)]
        pub lifecycle: Option<EngineLifecycle>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum EngineLifecycle {
        Service,
        OnDemand,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "kebab-case")]
    pub enum Category {
        Llm,
        Stt,
        Tts,
        Embeddings,
        Reranker,
        Vision,
        ImageGen,
        VideoGen,
        MusicGen,
        Model3dGen,
        Agents,
        Tools,
        Training,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum ApiKind {
        OpenaiCompatible,
        OllamaNative,
        SherpaTts,
        SherpaStt,
        Comfyui,
        Anthropic,
        AzureOpenai,
        Elevenlabs,
        Soniox,
        Custom,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "kebab-case")]
    pub enum ResourceKind {
        Ai,
        Infra,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DeploySection {
        #[serde(default)]
        pub docker: Option<DockerDeploy>,
        #[serde(default)]
        pub native: Option<NativeDeploy>,
        #[serde(default)]
        pub external: Option<ExternalDeploy>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DockerDeploy {
        #[serde(default)]
        pub context_path: Option<String>,
        #[serde(default)]
        pub compose_path: Option<String>,
        pub platforms: Vec<TargetOs>,
        #[serde(default)]
        pub download_image: Option<String>,
        #[serde(default)]
        pub download_size_mb: Option<u64>,
        /// Required as of Phase 6: declares whether TentaFlow wraps the
        /// container's HTTP API in a QUIC sidecar (`sidecar-quic`) or speaks
        /// HTTP directly to the host-mapped port (`direct-http`). Validated
        /// with a dedicated rule below so the error message is actionable when
        /// the field is missing.
        #[serde(default)]
        pub transport: Option<DockerTransport>,
        /// Build-args wspolne dla kazdej arch GPU + macierz per arch-tag —
        /// mirror runtime `DockerDeploy`. Musza tu byc, inaczej build.rs gubi
        /// te pola przy parsowaniu TOML → JSON (serde ignoruje nieznane).
        #[serde(default)]
        pub default_build_args: HashMap<String, String>,
        #[serde(default)]
        pub arch_variants: HashMap<String, DockerArchVariant>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct DockerArchVariant {
        #[serde(default)]
        pub build_args: HashMap<String, String>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum DockerTransport {
        SidecarQuic,
        DirectHttp,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct NativeDeploy {
        pub platforms: Vec<TargetOs>,
        pub runtime: NativeRuntime,
        #[serde(default)]
        pub feature_flag: Option<String>,
        #[serde(default)]
        pub binary_path: Option<String>,
        #[serde(default)]
        pub bundle_path: Option<String>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum NativeRuntime {
        Embedded,
        Binary,
        PythonBundle,
        ManagedCli,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ExternalDeploy {
        pub platforms: Vec<TargetOs>,
        #[serde(default)]
        pub detection_binary: String,
        pub detection_endpoint: String,
        #[serde(default = "default_health_path")]
        pub detection_health_path: String,
        #[serde(default)]
        pub requires_api_key: bool,
    }
    fn default_health_path() -> String {
        "/".to_string()
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "lowercase")]
    pub enum TargetOs {
        Linux,
        Macos,
        Windows,
        Ios,
        Android,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ModelPreset {
        pub id: String,
        pub display_name: String,
        pub repo: String,
        #[serde(default)]
        pub quantization: Option<String>,
        #[serde(default)]
        pub recommended: bool,
        /// Renders this preset as its own deployable catalog tile (featured
        /// model) while reusing the parent engine's deploy path.
        #[serde(default)]
        pub featured: bool,
        /// Per-preset overrides for the three capability axes. Same
        /// semantics as on `Engine` — `None` falls through to engine
        /// then category defaults; explicit empty list is rejected.
        #[serde(default)]
        pub service_surfaces: Option<Vec<String>>,
        #[serde(default)]
        pub input_modalities: Option<Vec<String>>,
        #[serde(default)]
        pub output_modalities: Option<Vec<String>>,
        /// vLLM speculative decoding pairing (mirror of runtime
        /// `ModelPreset` fields). `speculator_method` flows through to
        /// `--speculative-config '{"method": ...}'` unchanged.
        #[serde(default)]
        pub speculator_repo: Option<String>,
        #[serde(default)]
        pub speculator_method: Option<String>,
        #[serde(default)]
        pub speculator_num_tokens: Option<u32>,
        /// Mirror `ModelPreset::sampling` — bez tego pola serde po cichu gubi
        /// blok `[model_preset.sampling]`, a runtime czyta manifesty wlasnie
        /// z JSON-a generowanego tutaj.
        #[serde(default)]
        pub sampling: Option<SamplingDefaults>,
        /// Plik checkpointu image-gen (ComfyUI) pobierany z `repo` przy deployu.
        /// Mirror `ModelPreset::checkpoint_file` z `services/manifest/types.rs`.
        #[serde(default)]
        pub checkpoint_file: Option<String>,
        /// Warianty kwantyzacji tego samego modelu — kazdy to inne repo HF pod
        /// wybrana kwantyzacje. `repo`/`quantization` na preset = wariant „standard".
        /// Wizard pokazuje je jako wybor przy kalkulatorze i podmienia repo.
        #[serde(default, rename = "quant_variant")]
        pub quant_variants: Vec<QuantVariant>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct QuantVariant {
        pub quantization: String,
        pub repo: String,
        #[serde(default)]
        pub display_name: Option<String>,
    }

    /// Mirror `manifest::types::SamplingDefaults`.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SamplingDefaults {
        #[serde(default)]
        pub temperature: Option<f64>,
        #[serde(default)]
        pub top_p: Option<f64>,
        #[serde(default)]
        pub top_k: Option<i64>,
        #[serde(default)]
        pub min_p: Option<f64>,
    }

    // Single source of truth for the three wire-string allow-lists is
    // `src/services/manifest/vocabulary.rs`; we `include!` it so build-time
    // validation cannot drift from runtime validation. The included file
    // declares `pub const VALID_*` items at module scope.
    include!("src/services/manifest/vocabulary.rs");

    /// Whitelist regex `^[a-z0-9][a-z0-9_-]{0,63}$` dla engine.id.
    /// MUSI byc identyczna z `validate_engine_id` w runtime.
    fn is_valid_engine_id(id: &str) -> bool {
        let bytes = id.as_bytes();
        if bytes.is_empty() || bytes.len() > 64 {
            return false;
        }
        let first = bytes[0];
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return false;
        }
        bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    }

    /// Walidacja semantyczna identyczna z runtime — 4 reguly ze SCHEMA.md.
    pub fn validate(
        manifest: &ServiceManifest,
        containers_root: &std::path::Path,
    ) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();
        let eid = &manifest.engine.id;

        // Reguła 1: engine.id whitelist regex.
        if !is_valid_engine_id(eid) {
            errors.push(format!(
                "engine id = '{}' nie spelnia wymaganego formatu \
                 '^[a-z0-9][a-z0-9_-]{{0,63}}$' (1-64 znakow, kebab/snake_case)",
                eid
            ));
        }

        // Reguła 2: minimum jedna sekcja deploy.
        let d = &manifest.deploy;
        if d.docker.is_none() && d.native.is_none() && d.external.is_none() {
            errors.push(format!(
                "engine '{}': brak sekcji deploymentu — wymagana przynajmniej jedna z \
                 [deploy.docker], [deploy.native], [deploy.external]",
                eid
            ));
        }

        // Reguła 3: deploy.native.runtime spojny z polami.
        if let Some(n) = &d.native {
            match n.runtime {
                NativeRuntime::Embedded => {
                    // `feature_flag` jest opcjonalny — silniki ktorych
                    // backend (np. tract-onnx dla vision) jest zawsze
                    // wkompilowany na wszystkich platformach nie maja
                    // odpowiadajacej Cargo feature.
                    if n.binary_path.is_some() || n.bundle_path.is_some() {
                        errors.push(format!(
                            "engine '{}': deploy.native.runtime = embedded \
                             nie moze miec binary_path/bundle_path",
                            eid
                        ));
                    }
                }
                NativeRuntime::Binary => {
                    if n.binary_path.is_none()
                        || n.feature_flag.is_some()
                        || n.bundle_path.is_some()
                    {
                        errors.push(format!(
                            "engine '{}': deploy.native.runtime = binary wymaga pola \
                             binary_path (i nie moze miec feature_flag/bundle_path)",
                            eid
                        ));
                    }
                }
                NativeRuntime::PythonBundle => {
                    if n.bundle_path.is_none()
                        || n.feature_flag.is_some()
                        || n.binary_path.is_some()
                    {
                        errors.push(format!(
                            "engine '{}': deploy.native.runtime = python-bundle wymaga \
                             pola bundle_path (i nie moze miec feature_flag/binary_path)",
                            eid
                        ));
                    }
                }
                NativeRuntime::ManagedCli => {
                    if n.binary_path.is_none()
                        || n.feature_flag.is_some()
                        || n.bundle_path.is_some()
                    {
                        errors.push(format!(
                            "engine '{}': deploy.native.runtime = managed-cli wymaga pola \
                             binary_path (i nie moze miec feature_flag/bundle_path)",
                            eid
                        ));
                    }
                }
            }
        }

        if let Some(docker) = &d.docker {
            let has_context = docker
                .context_path
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            let has_compose = docker
                .compose_path
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            if has_context == has_compose {
                errors.push(format!(
                    "engine '{}': deploy.docker must define exactly one of context_path or compose_path",
                    eid
                ));
            }

            // Reguła 5 (Faza 6): kazda sekcja [deploy.docker] musi miec pole
            // `transport` ustawione na "sidecar-quic" lub "direct-http". Pole
            // jest typowane przez serde; Option=None tutaj oznacza ze plik TOML
            // pomijal klucz.
            if docker.transport.is_none() {
                errors.push(format!(
                    "engine '{}': [deploy.docker] is missing required `transport` field — \
                     set it to \"sidecar-quic\" (TentaFlow QUIC sidecar in front of the \
                     engine HTTP API) or \"direct-http\" (TentaFlow speaks HTTP directly \
                     to the host-mapped port).",
                    eid
                ));
            }
        }

        // Reguła 4: sciezki na dysku.
        if let Some(docker) = &d.docker {
            if let Some(path) = &docker.context_path {
                check_path(
                    containers_root,
                    path,
                    "deploy.docker.context_path",
                    eid,
                    &mut errors,
                );
            }
            if let Some(path) = &docker.compose_path {
                check_file(
                    containers_root,
                    path,
                    "deploy.docker.compose_path",
                    eid,
                    &mut errors,
                );
            }
        }
        if let Some(n) = &d.native {
            if let Some(p) = &n.binary_path {
                check_path(
                    containers_root,
                    p,
                    "deploy.native.binary_path",
                    eid,
                    &mut errors,
                );
            }
            if let Some(p) = &n.bundle_path {
                check_path(
                    containers_root,
                    p,
                    "deploy.native.bundle_path",
                    eid,
                    &mut errors,
                );
            }
        }

        // Capability axes (service_surfaces / input_modalities / output_modalities).
        // Each is Option<Vec<String>>; empty list is invalid, unknown values are
        // rejected. Mirrors `validate_engine` in src/services/manifest/validate.rs.
        validate_enum_list(
            "engine.service_surfaces",
            eid,
            manifest.engine.service_surfaces.as_deref(),
            VALID_SERVICE_SURFACES,
            &mut errors,
        );
        validate_enum_list(
            "engine.input_modalities",
            eid,
            manifest.engine.input_modalities.as_deref(),
            VALID_INPUT_MODALITIES,
            &mut errors,
        );
        validate_enum_list(
            "engine.output_modalities",
            eid,
            manifest.engine.output_modalities.as_deref(),
            VALID_OUTPUT_MODALITIES,
            &mut errors,
        );
        for preset in &manifest.model_presets {
            validate_enum_list(
                "model_preset.service_surfaces",
                eid,
                preset.service_surfaces.as_deref(),
                VALID_SERVICE_SURFACES,
                &mut errors,
            );
            validate_enum_list(
                "model_preset.input_modalities",
                eid,
                preset.input_modalities.as_deref(),
                VALID_INPUT_MODALITIES,
                &mut errors,
            );
            validate_enum_list(
                "model_preset.output_modalities",
                eid,
                preset.output_modalities.as_deref(),
                VALID_OUTPUT_MODALITIES,
                &mut errors,
            );
        }

        validate_parameters(manifest, &mut errors);
        validate_required_assets(manifest, &mut errors);

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Walidacja sekcji [[required_asset]]: `path` to sama nazwa pliku w
    /// centralnym katalogu (bez separatorow), `mount_path` sciezka absolutna
    /// w kontenerze, `repo_path` (opcjonalny) sciezka wzgledna w repo bez
    /// `..`, URL http(s), sha256 jako 64 znaki lowercase hex. Bledna
    /// deklaracja zatrzymuje build — w deployu objawilaby sie dopiero jako
    /// silnik startujacy bez modelu.
    fn validate_required_assets(manifest: &ServiceManifest, errors: &mut Vec<String>) {
        let eid = &manifest.engine.id;
        let mut seen: std::collections::HashSet<&str> = Default::default();
        for asset in &manifest.required_assets {
            if !seen.insert(asset.path.as_str()) {
                errors.push(format!(
                    "engine '{}': required_asset.path '{}' zduplikowany",
                    eid, asset.path
                ));
            }
            let name_ok = !asset.path.is_empty()
                && !asset.path.contains('/')
                && !asset.path.contains('\\')
                && asset.path != "."
                && asset.path != "..";
            if !name_ok {
                errors.push(format!(
                    "engine '{}': required_asset.path '{}' musi byc sama nazwa pliku (bez separatorow sciezki)",
                    eid, asset.path
                ));
            }
            if !asset.mount_path.starts_with('/') {
                errors.push(format!(
                    "engine '{}': required_asset.mount_path '{}' musi byc sciezka absolutna",
                    eid, asset.mount_path
                ));
            }
            if let Some(repo) = &asset.repo_path {
                let p = std::path::Path::new(repo);
                let ok = p.is_relative()
                    && p.components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                if !ok {
                    errors.push(format!(
                        "engine '{}': required_asset.repo_path '{}' musi byc wzgledna sciezka w repo",
                        eid, repo
                    ));
                }
            }
            if let Some(env) = &asset.env_var {
                let ok = !env.is_empty()
                    && env
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    && env.as_bytes()[0].is_ascii_uppercase();
                if !ok {
                    errors.push(format!(
                        "engine '{}': required_asset.env_var '{}' musi pasowac do '^[A-Z][A-Z0-9_]*$'",
                        eid, env
                    ));
                }
            }
            if !(asset.url.starts_with("https://") || asset.url.starts_with("http://")) {
                errors.push(format!(
                    "engine '{}': required_asset.url '{}' musi byc adresem http(s)",
                    eid, asset.url
                ));
            }
            if asset.sha256.len() != 64
                || !asset
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                errors.push(format!(
                    "engine '{}': required_asset.sha256 '{}' musi byc 64-znakowym lowercase hex",
                    eid, asset.sha256
                ));
            }
        }
    }

    /// Walidacja semantyczna sekcji [[parameter]]:
    ///   - klucze unikalne w obrebie engine,
    ///   - kind/range/options spojne (range dla float|int, options dla enum),
    ///   - default zgodny z kind i (gdy enum) z options,
    ///   - bindings niepusta,
    ///   - binding.when wskazuje na deploy section ktory manifest realnie ma,
    ///   - binding.target.Env.name pasuje do regex `^[A-Z][A-Z0-9_]*$`,
    ///   - binding.target.*Field/Options.field/key niepuste.
    fn validate_parameters(manifest: &ServiceManifest, errors: &mut Vec<String>) {
        let eid = &manifest.engine.id;
        let mut seen_keys: std::collections::HashSet<&str> = Default::default();
        for p in &manifest.parameters {
            // Klucz unikalny.
            if !seen_keys.insert(p.key.as_str()) {
                errors.push(format!(
                    "engine '{}': parameter.key '{}' zduplikowany",
                    eid, p.key
                ));
                continue;
            }
            // kind ↔ range/options spojnosc.
            match p.kind {
                ParameterKind::Float | ParameterKind::Int => {
                    if p.range.is_none() {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind={:?} wymaga range",
                            eid, p.key, p.kind
                        ));
                    }
                    if p.options.is_some() {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind={:?} nie powinien miec options",
                            eid, p.key, p.kind
                        ));
                    }
                }
                ParameterKind::Enum => {
                    let Some(opts) = &p.options else {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind=enum wymaga options",
                            eid, p.key
                        ));
                        continue;
                    };
                    if opts.is_empty() {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind=enum wymaga niepustej listy options",
                            eid, p.key
                        ));
                    }
                }
                ParameterKind::Bool | ParameterKind::String => {
                    if p.range.is_some() {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind={:?} nie powinien miec range",
                            eid, p.key, p.kind
                        ));
                    }
                    if p.options.is_some() && p.kind == ParameterKind::Bool {
                        errors.push(format!(
                            "engine '{}': parameter '{}' kind=bool nie powinien miec options",
                            eid, p.key
                        ));
                    }
                }
            }
            // Default zgodny z kind.
            let default_ok = match p.kind {
                ParameterKind::Float => p.default.is_f64() || p.default.is_i64(),
                ParameterKind::Int => p.default.is_i64() || p.default.is_u64(),
                ParameterKind::Bool => p.default.is_boolean(),
                ParameterKind::Enum => {
                    p.default.is_string()
                        && p.options.as_ref().is_some_and(|opts| {
                            opts.iter().any(|o| Some(o.as_str()) == p.default.as_str())
                        })
                }
                ParameterKind::String => p.default.is_string(),
            };
            if !default_ok {
                errors.push(format!(
                    "engine '{}': parameter '{}' default '{}' niezgodny z kind={:?}",
                    eid, p.key, p.default, p.kind
                ));
            }
            // Default w zakresie (gdy range).
            if let Some(range) = p.range {
                let value = p
                    .default
                    .as_f64()
                    .or_else(|| p.default.as_i64().map(|v| v as f64));
                if let Some(v) = value {
                    if v < range.min || v > range.max {
                        errors.push(format!(
                            "engine '{}': parameter '{}' default {} poza zakresem [{}, {}]",
                            eid, p.key, v, range.min, range.max
                        ));
                    }
                }
            }
            // Bindings niepusta.
            if p.bindings.is_empty() {
                errors.push(format!(
                    "engine '{}': parameter '{}' wymaga przynajmniej jednego binding",
                    eid, p.key
                ));
                continue;
            }
            // Walidacja per binding.
            for (i, b) in p.bindings.iter().enumerate() {
                // when musi pasowac do deklarowanej deploy section.
                let target_present = match b.when {
                    DeployTarget::Docker => manifest.deploy.docker.is_some(),
                    DeployTarget::NativeEmbedded => manifest
                        .deploy
                        .native
                        .as_ref()
                        .is_some_and(|n| n.runtime == NativeRuntime::Embedded),
                    DeployTarget::NativePythonBundle => manifest
                        .deploy
                        .native
                        .as_ref()
                        .is_some_and(|n| n.runtime == NativeRuntime::PythonBundle),
                    DeployTarget::NativeBinary => manifest
                        .deploy
                        .native
                        .as_ref()
                        .is_some_and(|n| n.runtime == NativeRuntime::Binary),
                    DeployTarget::External => manifest.deploy.external.is_some(),
                };
                if !target_present {
                    errors.push(format!(
                        "engine '{}': parameter '{}' binding[{}] when={:?} \
                         wskazuje na deploy method ktorej manifest nie deklaruje",
                        eid, p.key, i, b.when
                    ));
                }
                // Walidacja pol target.
                match &b.target {
                    BindingTarget::Env { name } => {
                        if !is_valid_env_name(name) {
                            errors.push(format!(
                                "engine '{}': parameter '{}' binding[{}] env.name '{}' \
                                 niezgodny z regex '^[A-Z][A-Z0-9_]*$'",
                                eid, p.key, i, name
                            ));
                        }
                    }
                    BindingTarget::LlamacppField { field }
                    | BindingTarget::WhisperField { field, .. }
                    | BindingTarget::MlxField { field, .. }
                    | BindingTarget::PythonRequestBody { field } => {
                        if field.trim().is_empty() {
                            errors.push(format!(
                                "engine '{}': parameter '{}' binding[{}] field jest pusty",
                                eid, p.key, i
                            ));
                        }
                    }
                    BindingTarget::OllamaOptions { key } => {
                        if key.trim().is_empty() {
                            errors.push(format!(
                                "engine '{}': parameter '{}' binding[{}] ollama_options.key jest pusty",
                                eid, p.key, i
                            ));
                        }
                    }
                }
            }
        }
    }

    fn is_valid_env_name(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_uppercase() {
            return false;
        }
        chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    }

    fn validate_enum_list(
        field: &str,
        engine_id: &str,
        value: Option<&[String]>,
        allowed: &[&str],
        errors: &mut Vec<String>,
    ) {
        let Some(list) = value else {
            return;
        };
        if list.is_empty() {
            errors.push(format!(
                "engine '{}': {} jest pusta lista — uzyj braku pola, \
                 zeby fallback do category default",
                engine_id, field
            ));
            return;
        }
        for v in list {
            if !allowed.iter().any(|a| *a == v.as_str()) {
                errors.push(format!(
                    "engine '{}': {} zawiera nieznana wartosc '{}' \
                     (dozwolone: {:?})",
                    engine_id, field, v, allowed
                ));
            }
        }
    }

    fn check_path(
        root: &std::path::Path,
        rel: &str,
        field: &str,
        engine_id: &str,
        errors: &mut Vec<String>,
    ) {
        let full = root.join(rel);
        if !full.is_dir() {
            errors.push(format!(
                "engine '{}': sciezka {} = '{}' nie istnieje na dysku ({})",
                engine_id,
                field,
                rel,
                full.display()
            ));
        }
    }

    fn check_file(
        root: &std::path::Path,
        rel: &str,
        field: &'static str,
        engine_id: &str,
        errors: &mut Vec<String>,
    ) {
        let full = root.join(rel);
        if !full.is_file() {
            errors.push(format!(
                "engine '{}': sciezka {} = '{}' nie istnieje na dysku",
                engine_id, field, rel
            ));
        }
    }
}

/// Files/extensions excluded from the deterministic source-tree hash. These
/// are either local build artifacts, editor cruft, or virtualenv state that
/// must not affect whether a container source is considered "updated".
const HASH_SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

const HASH_SKIP_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// Returns true when the path component should be skipped while walking the
/// source tree for hashing.
fn hash_should_skip(name: &str) -> bool {
    if HASH_SKIP_DIRS.iter().any(|d| *d == name) {
        return true;
    }
    if HASH_SKIP_FILES.iter().any(|f| *f == name) {
        return true;
    }
    if name.ends_with(".pyc") || name.ends_with(".pyo") {
        return true;
    }
    false
}

/// Computes a deterministic sha256 of all files under `root`. The relative
/// path (with `/` separators) is mixed into the hash, so renames and moves
/// change the digest. Returns an empty string when `root` does not exist.
/// Emituje `cargo:rerun-if-changed` dla KAŻDEGO pliku w drzewie `root`.
/// `rerun-if-changed=<katalog>` śledzi tylko mtime katalogu (add/remove), więc
/// edycja treści istniejącego pliku (np. lib.rs codec wasm) nie triggeruje
/// rerun build.rs → wygenerowany `wasm_glue.wasm` zostaje stary. Per-plik to
/// naprawia.
fn rerun_if_changed_recursive(root: &Path) {
    use walkdir::WalkDir;
    if !root.exists() {
        return;
    }
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_file() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
}

fn compute_source_hash(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    use walkdir::WalkDir;

    if !root.is_dir() {
        return String::new();
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !hash_should_skip(&name)
        });

    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        // rerun-if-changed PER PLIK: `rerun-if-changed=<katalog>` (emitowane przez
        // wołającego) śledzi tylko mtime KATALOGU — zmienia się przy add/remove,
        // NIE przy edycji treści zagnieżdżonego pliku. Bez tego edycja entrypoint.sh
        // / server.py nie triggeruje rerun build.rs → `*_source_hash` zostaje stary
        // → dashboard NIE pokazuje "Aktualizuj". Śledzimy każdy plik z osobna.
        println!("cargo:rerun-if-changed={}", entry.path().display());
        files.push((rel, entry.path().to_path_buf()));
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (rel, abs) in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        match std::fs::read(abs) {
            Ok(bytes) => hasher.update(&bytes),
            Err(e) => {
                println!(
                    "cargo:warning=compute_source_hash: skip {}: {}",
                    abs.display(),
                    e
                );
            }
        }
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

/// Łączy hashe źródeł z wielu katalogów w jeden (deterministycznie, w kolejności
/// podania). Każdy `compute_source_hash` emituje też rerun-if-changed per plik,
/// więc edycja treści w DOWOLNYM z katalogów triggeruje rerun build.rs. Używane
/// dla docker_source_hash obejmującego context (Dockerfile/entrypoint) + python
/// bundle (server.py), które obraz dockera materializuje razem.
/// Repo-relative `COPY` sources of a Dockerfile, restricted to paths under
/// `tentaflow-containers/`. The docker build context is the REPO ROOT, so a
/// Dockerfile may pull in files that live outside its own `context_path` —
/// shared patch scripts, for instance. Those files end up baked into the image,
/// so `docker_source_hash` has to cover them; otherwise editing only a patch
/// leaves the tag unchanged and the deploy silently reuses the stale image.
/// Stage copies (`COPY --from=`) reference an earlier build stage, not the
/// context, and are skipped.
fn dockerfile_external_copy_sources(dockerfile: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dockerfile) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("COPY ") else {
            continue;
        };
        let mut args: Vec<&str> = rest.split_whitespace().collect();
        if args.iter().any(|a| a.starts_with("--from=")) {
            continue;
        }
        args.retain(|a| !a.starts_with("--"));
        // Last argument is the destination inside the image, never a source.
        args.pop();
        for src in args {
            if let Some(rel) = src.strip_prefix("tentaflow-containers/") {
                out.push(rel.to_string());
            }
        }
    }
    out
}

fn compute_source_hash_multi(roots: &[PathBuf]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for root in roots {
        hasher.update(compute_source_hash(root).as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(hasher.finalize())
}

fn generate_services_manifest(out_dir: &Path) {
    use services_manifest_build::{validate, ResourceKind, ServiceManifest};
    use std::collections::HashSet;

    let workspace_root = Path::new("..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(".."));
    let containers_dir = workspace_root.join("tentaflow-containers");

    if !containers_dir.is_dir() {
        println!(
            "cargo:warning=generate_services_manifest: brak {} — generuje pusty rejestr",
            containers_dir.display()
        );
        write_generated(out_dir, "[]");
        write_js_module(Path::new("www/js/generated/services-manifest.js"), "[]");
        return;
    }

    // Skanuj wszystkie kategorie (top-level dirs w tentaflow-containers).
    let mut manifest_files: Vec<PathBuf> = Vec::new();
    let entries = match std::fs::read_dir(&containers_dir) {
        Ok(e) => e,
        Err(e) => {
            panic!(
                "generate_services_manifest: nie mozna odczytac {}: {}",
                containers_dir.display(),
                e
            );
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Pomin podkatalogi techniczne (zaczynajace sie od '_', np. _schema).
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') {
            continue;
        }
        let services_dir = path.join("_services");
        if !services_dir.is_dir() {
            continue;
        }
        // Rerun-if-changed dla katalogu (lapie dodanie/usuniecie pliku) ORAZ
        // dla kazdego pliku .toml osobno. Na APFS mtime katalogu NIE zmienia
        // sie przy edycji zawartosci pliku, wiec sam katalog nie wystarcza —
        // bez per-plik watch edycja manifestu nie przebudowuje binarki i
        // zostaje stary baked manifest (np. brak featured preset).
        println!("cargo:rerun-if-changed={}", services_dir.display());

        let svc_entries = match std::fs::read_dir(&services_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for svc in svc_entries.flatten() {
            let p = svc.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                println!("cargo:rerun-if-changed={}", p.display());
                manifest_files.push(p);
            }
        }
    }

    // Stabilna kolejnosc — sortujemy alfabetycznie sciezki.
    manifest_files.sort();

    let mut loaded: Vec<ServiceManifest> = Vec::new();
    let mut seen_engine_ids: HashSet<String> = HashSet::new();

    for file in &manifest_files {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => panic!("Nie mozna odczytac manifestu '{}': {}", file.display(), e),
        };

        let mut manifest: ServiceManifest = match toml::from_str(&content) {
            Ok(m) => m,
            Err(e) => panic!("Bledny TOML w manifescie '{}':\n  {}", file.display(), e),
        };

        // Walidacja semantyczna — 4 reguly.
        if let Err(errs) = validate(&manifest, &containers_dir) {
            let joined = errs
                .iter()
                .map(|s| format!("  - {}", s))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "Walidacja manifestu '{}' nieudana:\n{}",
                file.display(),
                joined
            );
        }

        // Compute source-tree hashes for docker and native deploy variants.
        // These seed the `deployed_source_hash` column for each service row
        // and later let the dashboard flag "update available".
        if let Some(docker) = manifest.deploy.docker.as_ref() {
            if let Some(ctx) = docker.context_path.as_deref() {
                let ctx_path = containers_dir.join(ctx);
                // docker_source_hash MUSI obejmować WSZYSTKIE źródła, które obraz
                // dockera materializuje — context_path (Dockerfile, entrypoint.sh)
                // ORAZ python-bundle (server.py + helpery), bo Dockerfile COPY'uje
                // go z `deploy.native.bundle_path`. Bez bundla zmiana server.py
                // była niewykrywalna → dashboard nie pokazywał "Aktualizuj".
                let mut roots = vec![ctx_path.clone()];
                for rel in dockerfile_external_copy_sources(&ctx_path.join("Dockerfile")) {
                    roots.push(containers_dir.join(rel));
                }
                if let Some(native) = manifest.deploy.native.as_ref() {
                    if let Some(rel) = native
                        .binary_path
                        .as_deref()
                        .or(native.bundle_path.as_deref())
                    {
                        roots.push(containers_dir.join(rel));
                    }
                }
                manifest.docker_source_hash = compute_source_hash_multi(&roots);
            }
        }
        if let Some(native) = manifest.deploy.native.as_ref() {
            let native_root = native
                .binary_path
                .as_deref()
                .or(native.bundle_path.as_deref());
            if let Some(rel) = native_root {
                let path = containers_dir.join(rel);
                println!("cargo:rerun-if-changed={}", path.display());
                manifest.native_source_hash = compute_source_hash(&path);
            }
        }

        // Globalna unikalnosc engine.id cross-file (poza 4 regulami per-file).
        if !seen_engine_ids.insert(manifest.engine.id.clone()) {
            panic!(
                "Walidacja manifestu '{}' nieudana:\n  - duplikat engine.id = '{}' \
                 (ten sam id w innym pliku _services)",
                file.display(),
                manifest.engine.id
            );
        }

        loaded.push(manifest);
    }

    // Edycja slim: bez zadnego lokalnego silnika inferencji nie ma czym uruchomic
    // kontenera z modelem, wiec katalog pokazuje tylko to, co na takim wezle
    // realnie dziala — uslugi uzytkowe (`resource_kind = "infra"`: searxng,
    // browser-renderer, milvus, iroh-relay, test-runner) oraz dostawcow, ktorzy
    // zyja wylacznie po zdalnym API (`[deploy.external]` jako jedyna sekcja).
    // Filtrujemy TU, w jednym generatorze, bo z niego powstaja OBIE sciezki:
    // rejestr Rust (services_generated.rs) i katalog GUI (services-manifest.js).
    // Inaczej dashboard pokazywalby kafelki, ktorych backend odmawia uruchomic.
    if is_slim_edition() {
        let before = loaded.len();
        loaded.retain(|m| {
            m.engine.resource_kind == Some(ResourceKind::Infra) || m.deploy.external.is_some()
        });
        // Silnik dostepny i zdalnie, i lokalnie (ollama) zostaje w katalogu, ale
        // wylacznie jako endpoint — lokalny deploy sciaga model, czyli dokladnie
        // to, czego slim nie robi.
        let mut trimmed = 0usize;
        for m in loaded.iter_mut() {
            if m.engine.resource_kind != Some(ResourceKind::Infra)
                && (m.deploy.docker.is_some() || m.deploy.native.is_some())
            {
                m.deploy.docker = None;
                m.deploy.native = None;
                trimmed += 1;
            }
        }
        println!(
            "cargo:warning=Edycja slim: katalog ograniczony do {} pozycji z {} \
             (ukryto silniki modelowe; {} zostawiono tylko jako zdalny endpoint)",
            loaded.len(),
            before,
            trimmed
        );
    }

    // Serializuj wszystko do JSON. pretty dla GUI, compact dla embed Rust (size).
    let json_compact = serde_json::to_string(&loaded)
        .expect("Bug: ServiceManifest powinien serializowac sie do JSON bez bledow");
    let json_pretty = serde_json::to_string_pretty(&loaded)
        .expect("Bug: ServiceManifest powinien serializowac sie do JSON bez bledow");

    write_generated(out_dir, &json_compact);

    // GUI module — zapisujemy do www, ale podajemy sciezke wzgledem build.rs CWD.
    let js_path = Path::new("www/js/generated/services-manifest.js");
    if let Some(parent) = js_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    write_js_module(js_path, &json_pretty);

    println!(
        "cargo:warning=Manifest serwisow: zaladowano {} silnikow z {} plikow TOML",
        loaded.len(),
        manifest_files.len()
    );
}

fn write_generated(out_dir: &Path, json: &str) {
    // Raw string z separatorem ###" ... "### — JSON nie zawiera tej sekwencji,
    // wiec brak konfliktow nawet z escapowanymi cudzyslowami w stringach.
    let code = format!(
        "// Auto-generated by build.rs — NIE EDYTUJ RECZNIE.\n\
         // Zawiera zserializowany JSON wszystkich manifestow z _services/.\n\
         pub const GENERATED_MANIFEST_JSON: &str = r###\"{}\"###;\n",
        json
    );
    let path = out_dir.join("services_generated.rs");
    std::fs::write(&path, code)
        .unwrap_or_else(|e| panic!("Nie mozna zapisac {}: {}", path.display(), e));
}

fn write_js_module(path: &Path, json_pretty: &str) {
    // Bez wall-clock timestampu — tresc musi byc DETERMINISTYCZNA, bo wchodzi do
    // ASSET_BUILD_HASH (build.rs generate_asset_manifest). Zmienny timestamp
    // powodowalby nowy hash przy KAZDYM buildzie backendu i falszywy komunikat
    // "nowa wersja" mimo braku zmian frontu.
    let content = format!(
        "// =============================================================================\n\
         // Plik: services-manifest.js\n\
         // Opis: AUTO-GENERATED przez build.rs — nie edytuj recznie.\n\
         //       Zrodlo: tentaflow-containers/*/_services/*.toml\n\
         // =============================================================================\n\
         \n\
         export const SCHEMA_VERSION = 2;\n\
         export const SERVICES = {};\n",
        json_pretty
    );
    if let Err(e) = std::fs::write(path, content) {
        println!(
            "cargo:warning=Nie udalo sie zapisac {}: {}",
            path.display(),
            e
        );
    }
}

/// Minimalna funkcja "now" bez dodawania chrono jako build-dep — uzywamy
/// SystemTime + recznej konwersji do ISO-8601 UTC z dokladnoscia do sekundy.
fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Algorytm Howarda Hinnanta — konwersja days_from_civil → Y-M-D.
    let days = (secs / 86_400) as i64;
    let sod = (secs % 86_400) as u32;
    let hour = sod / 3600;
    let min = (sod / 60) % 60;
    let sec = sod % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, min, sec
    )
}

// =============================================================================
// tentaflow-protocol-wasm — build + wasm-bindgen JS glue
// =============================================================================

/// Buduje crate tentaflow-protocol-wasm do targetu wasm32-unknown-unknown,
/// pozniej wola wasm-bindgen CLI zeby wygenerowac JS glue (target=web) do
/// www/js/protocol/. Generowane pliki (wasm_glue.js + wasm_glue_bg.wasm)
/// sa pozniej embedowane do binarki przez generate_wwwroot_embed.
///
/// Non-blocking: brak wasm32-unknown-unknown targetu lub brak wasm-bindgen
/// CLI skutkuje ostrzezeniem, nie bledem kompilacji. CI runner zainstaluje
/// oba narzedzia, lokalne `cargo build` zostanie z istniejacymi plikami
/// (lub ich brakiem — codec.js otrzyma ImportError przy starcie GUI, co
/// sygnalizuje programiscie ze trzeba odswiezyc pipeline).
fn build_protocol_wasm_bindings() {
    // Sciezki wejsciowe/wyjsciowe
    let crate_dir = Path::new("../tentaflow-protocol-wasm");
    let protocol_dir = Path::new("../tentaflow-protocol");
    let out_js_dir = Path::new("www/js/protocol");

    if !crate_dir.exists() {
        println!(
            "cargo:warning=build_protocol_wasm_bindings: brak crate {}, pomijam",
            crate_dir.display()
        );
        return;
    }

    // Rerun-if-changed hooks na zrodlach — PER PLIK (rerun na katalogu nie
    // wykrywa edycji treści istniejacych plikow, wiec wasm_glue.wasm zostawalby
    // stary po zmianie codeca).
    rerun_if_changed_recursive(&crate_dir.join("src"));
    println!("cargo:rerun-if-changed={}/Cargo.toml", crate_dir.display());
    rerun_if_changed_recursive(&protocol_dir.join("src"));
    println!(
        "cargo:rerun-if-changed={}/Cargo.toml",
        protocol_dir.display()
    );
    // The glue's decode is schema-driven (component/inline wire metadata comes
    // from tentaflow-sdk-spec), so a spec change must regenerate wasm_glue too —
    // otherwise new fields decode against stale metadata.
    rerun_if_changed_recursive(Path::new("../tentaflow-sdk-spec/src"));

    // Sprawdz wasm32-unknown-unknown target
    if !check_wasm_browser_target() {
        println!(
            "cargo:warning=tentaflow-protocol-wasm: brak wasm32-unknown-unknown targetu \
             (zainstaluj: rustup target add wasm32-unknown-unknown), pomijam generacje JS glue"
        );
        return;
    }

    // Sprawdz wasm-bindgen CLI — wersja musi byc zgodna z dependency w Cargo.toml
    let bindgen_version = detect_wasm_bindgen_version().unwrap_or_else(|| "unknown".to_string());
    if bindgen_version == "unknown" {
        println!(
            "cargo:warning=tentaflow-protocol-wasm: brak wasm-bindgen CLI w PATH \
             (zainstaluj: cargo install wasm-bindgen-cli --version 0.2.125 --locked), pomijam"
        );
        return;
    }

    // CARGO_TARGET_DIR isolation — oddzielny target dir dla WASM build zeby
    // uniknac lockingu na parent cargo i race condition na metadata.json.
    let isolated_target =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("protocol_wasm_target");
    std::fs::create_dir_all(&isolated_target).ok();

    // 1) cargo build --target wasm32-unknown-unknown --release
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(crate_dir)
        .env("CARGO_TARGET_DIR", &isolated_target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CFLAGS")
        .env_remove("CXXFLAGS")
        .env_remove("IPHONEOS_DEPLOYMENT_TARGET")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=tentaflow-protocol-wasm: kompilacja wasm32 OK");
        }
        Ok(s) => {
            println!(
                "cargo:warning=tentaflow-protocol-wasm: cargo build zakonczone kodem {}, pomijam glue",
                s
            );
            return;
        }
        Err(e) => {
            println!(
                "cargo:warning=tentaflow-protocol-wasm: nie udalo sie uruchomic cargo: {}, pomijam",
                e
            );
            return;
        }
    }

    let wasm_file =
        isolated_target.join("wasm32-unknown-unknown/release/tentaflow_protocol_wasm.wasm");
    if !wasm_file.exists() {
        println!(
            "cargo:warning=tentaflow-protocol-wasm: brak wynikowego .wasm: {}, pomijam",
            wasm_file.display()
        );
        return;
    }

    // 2) wasm-bindgen --target web --out-dir www/js/protocol --out-name wasm_glue
    std::fs::create_dir_all(out_js_dir).ok();
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-dir"])
        .arg(out_js_dir)
        .args(["--out-name", "wasm_glue", "--no-typescript"])
        .arg(&wasm_file)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:warning=tentaflow-protocol-wasm: wasm-bindgen ({}) wygenerowal glue do {}",
                bindgen_version,
                out_js_dir.display()
            );
        }
        Ok(s) => {
            // Fail hard zamiast cichego warning. Pre-existing zachowanie:
            // gdy wasm-bindgen pad (np. CLI vs library version mismatch),
            // build.rs wracal warning a JS glue zostawal stary — klient
            // dashboard mial schema_version=stary, server=nowy, WS dropped
            // co tickem. Hard fail przy starcie build wymusza fix
            // (zaktualizuj wasm-bindgen-cli do wersji z Cargo.toml).
            panic!(
                "wasm-bindgen zakonczone kodem {} — JS glue moze byc niespojny z .wasm. \
                 Sprawdz czy `wasm-bindgen --version` zgadza sie z Cargo.toml \
                 (`tentaflow-protocol-wasm/Cargo.toml`). Reinstall: \
                 `cargo install wasm-bindgen-cli --version <X.Y.Z> --locked` gdzie \
                 X.Y.Z to wersja z Cargo.toml.",
                s
            );
        }
        Err(e) => {
            panic!("nie udalo sie uruchomic wasm-bindgen: {}", e);
        }
    }
}

/// Buduje crate tentaflow-voxel-wasm do targetu wasm32-unknown-unknown, potem
/// wola wasm-bindgen CLI (target=web) zeby wygenerowac JS glue do www/js/voxel/
/// (voxel_glue.js + voxel_glue_bg.wasm). Te pliki sa pozniej embedowane do
/// binarki przez generate_wwwroot_embed.
///
/// Non-blocking jak build_protocol_wasm_bindings: brak wasm32-unknown-unknown
/// targetu lub brak wasm-bindgen CLI skutkuje cargo:warning, nie bledem — robot
/// LiDAR tab po prostu nie wczyta renderera, reszta dashboardu dziala.
fn build_voxel_wasm_bindings() {
    let crate_dir = Path::new("../tentaflow-voxel-wasm");
    let out_js_dir = Path::new("www/js/voxel");

    if !crate_dir.exists() {
        println!(
            "cargo:warning=build_voxel_wasm_bindings: brak crate {}, pomijam",
            crate_dir.display()
        );
        return;
    }

    // Rerun-if-changed hooks na zrodlach crate'a. Per-plik (rekursja), bo
    // rerun-if-changed na katalogu lapie tylko dodanie/usuniecie wpisu, nie
    // edycje istniejacego src/lib.rs — bez tego edycja renderera zostawialaby
    // stary voxel_glue_bg.wasm w embedzie.
    println!("cargo:rerun-if-changed={}/Cargo.toml", crate_dir.display());
    for f in walkdir_rs(&crate_dir.join("src")) {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    if !check_wasm_browser_target() {
        println!(
            "cargo:warning=tentaflow-voxel-wasm: brak wasm32-unknown-unknown targetu \
             (zainstaluj: rustup target add wasm32-unknown-unknown), pomijam generacje JS glue"
        );
        return;
    }

    let bindgen_version = detect_wasm_bindgen_version().unwrap_or_else(|| "unknown".to_string());
    if bindgen_version == "unknown" {
        println!(
            "cargo:warning=tentaflow-voxel-wasm: brak wasm-bindgen CLI w PATH \
             (zainstaluj: cargo install wasm-bindgen-cli --version 0.2.125 --locked), pomijam"
        );
        return;
    }

    // Oddzielny target dir zeby uniknac lockingu na parent cargo.
    let isolated_target =
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("voxel_wasm_target");
    std::fs::create_dir_all(&isolated_target).ok();

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(crate_dir)
        .env("CARGO_TARGET_DIR", &isolated_target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CFLAGS")
        .env_remove("CXXFLAGS")
        .env_remove("IPHONEOS_DEPLOYMENT_TARGET")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=tentaflow-voxel-wasm: kompilacja wasm32 OK");
        }
        Ok(s) => {
            println!(
                "cargo:warning=tentaflow-voxel-wasm: cargo build zakonczone kodem {}, pomijam glue",
                s
            );
            return;
        }
        Err(e) => {
            println!(
                "cargo:warning=tentaflow-voxel-wasm: nie udalo sie uruchomic cargo: {}, pomijam",
                e
            );
            return;
        }
    }

    let wasm_file =
        isolated_target.join("wasm32-unknown-unknown/release/tentaflow_voxel_wasm.wasm");
    if !wasm_file.exists() {
        println!(
            "cargo:warning=tentaflow-voxel-wasm: brak wynikowego .wasm: {}, pomijam",
            wasm_file.display()
        );
        return;
    }

    std::fs::create_dir_all(out_js_dir).ok();
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-dir"])
        .arg(out_js_dir)
        .args(["--out-name", "voxel_glue", "--no-typescript"])
        .arg(&wasm_file)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:warning=tentaflow-voxel-wasm: wasm-bindgen ({}) wygenerowal glue do {}",
                bindgen_version,
                out_js_dir.display()
            );
        }
        Ok(s) => {
            panic!(
                "wasm-bindgen (voxel) zakonczone kodem {} — JS glue moze byc niespojny z .wasm. \
                 Sprawdz czy `wasm-bindgen --version` zgadza sie z \
                 `tentaflow-voxel-wasm/Cargo.toml`. Reinstall: \
                 `cargo install wasm-bindgen-cli --version <X.Y.Z> --locked`.",
                s
            );
        }
        Err(e) => {
            panic!("nie udalo sie uruchomic wasm-bindgen (voxel): {}", e);
        }
    }
}

/// Sprawdza czy wasm32-unknown-unknown jest zainstalowany (browser target).
fn check_wasm_browser_target() -> bool {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .any(|line| line.trim() == "wasm32-unknown-unknown")
        }
        Err(_) => false,
    }
}

/// Scans WASM binary imports for functions in the "env" module. Returns
/// Err with a human-readable list if any are found — those indicate a
/// missing `#[link(wasm_import_module = "tentaflow")]` on the extern block.
/// Allowed "env" imports: none. Allowed modules: "tentaflow",
/// "wasi_snapshot_preview1", "wasi".
fn validate_wasm_imports(wasm_path: &Path) -> Result<(), String> {
    let bytes = match std::fs::read(wasm_path) {
        Ok(b) => b,
        Err(e) => return Err(format!("  (cannot read WASM: {e})")),
    };

    // Minimal WASM binary parser — just enough to scan the import section.
    // WASM magic: \0asm, version 1. Sections are (id: u8, size: u32leb128, payload).
    // Import section id = 2. Each import: (module: str, name: str, desc).
    let mut bad = Vec::new();
    let mut pos = 8; // skip magic + version
    while pos < bytes.len() {
        let section_id = bytes[pos];
        pos += 1;
        let (section_len, adv) = read_leb128_u32(&bytes[pos..]);
        pos += adv;
        let section_end = pos + section_len as usize;

        if section_id == 2 {
            // Import section
            let mut p = pos;
            let (count, adv) = read_leb128_u32(&bytes[p..]);
            p += adv;
            for _ in 0..count {
                let (mod_len, adv) = read_leb128_u32(&bytes[p..]);
                p += adv;
                let module = std::str::from_utf8(&bytes[p..p + mod_len as usize]).unwrap_or("?");
                p += mod_len as usize;
                let (name_len, adv) = read_leb128_u32(&bytes[p..]);
                p += adv;
                let name = std::str::from_utf8(&bytes[p..p + name_len as usize]).unwrap_or("?");
                p += name_len as usize;
                // Skip import descriptor (kind: u8 + type index)
                let kind = bytes[p];
                p += 1;
                let (_idx, adv) = read_leb128_u32(&bytes[p..]);
                p += adv;
                if kind == 1 {
                    // table import: extra (limits)
                    let _flags = bytes[p];
                    p += 1;
                    let (_init, adv) = read_leb128_u32(&bytes[p..]);
                    p += adv;
                    if _flags & 1 != 0 {
                        let (_, adv) = read_leb128_u32(&bytes[p..]);
                        p += adv;
                    }
                } else if kind == 2 {
                    // memory import: (limits)
                    let _flags = bytes[p];
                    p += 1;
                    let (_, adv) = read_leb128_u32(&bytes[p..]);
                    p += adv;
                    if _flags & 1 != 0 {
                        let (_, adv) = read_leb128_u32(&bytes[p..]);
                        p += adv;
                    }
                } else if kind == 3 {
                    // global import: valtype + mut
                    p += 2;
                }

                if module == "env" {
                    bad.push(format!("  env::{name}"));
                }
            }
            break;
        }
        pos = section_end;
    }

    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join("\n"))
    }
}

fn read_leb128_u32(bytes: &[u8]) -> (u32, usize) {
    let mut result: u32 = 0;
    let mut shift = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
    }
    (result, bytes.len())
}

/// Zwraca wersje zainstalowanego wasm-bindgen CLI (np. "0.2.100") lub None.
fn detect_wasm_bindgen_version() -> Option<String> {
    let output = Command::new("wasm-bindgen")
        .args(["--version"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Format: "wasm-bindgen 0.2.100"
    text.split_whitespace().nth(1).map(|s| s.to_string())
}

/// Recursively collect all file paths under `dir`. No external crate needed.
fn walkdir_rs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir_rs(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}
