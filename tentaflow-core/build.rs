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

    // Skanuj manifesty serwisow tentaflow-containers/*/_services/*.toml,
    // waliduj semantycznie i wygeneruj services_generated.rs + services-manifest.js.
    // To musi byc PRZED dlugim WASM-buildem, zeby blad walidacji wykryl sie szybko.
    generate_services_manifest(&out_dir_env);

    // Generuj wwwroot_embed.rs — pliki statyczne wbudowane w binarie
    // (po wygenerowaniu services-manifest.js, zeby trafil do embed).
    generate_wwwroot_embed(&out_dir_env);

    // Pakuj kontekst dockerow (tentaflow-containers + tentaflow-protocol)
    // jako tar.gz wbudowany w binarce — deploy module rozpakowuje to do tmpdir
    // i robi `docker build` bez wymagania zewnetrznych zrodel.
    pack_container_contexts(&out_dir_env);

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bundle_dir = out_dir.join("addon_bundles");
    std::fs::create_dir_all(&bundle_dir).unwrap();

    // Sprawdz czy wasm32-wasip1 target jest zainstalowany
    let has_wasm_target = check_wasm_target();

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
            if !addon_dir.join("Cargo.toml").exists() {
                continue;
            }
            if !addon_dir.join("manifest.toml").exists() {
                continue;
            }

            let addon_name = addon_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();

            println!("cargo:warning=Addon '{}' — rozpoczynam budowanie WASM", addon_name);

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
            let status = Command::new("cargo")
                .args(["build", "--target", "wasm32-wasip1", "--release"])
                .current_dir(&addon_dir)
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
            let wasm_crate_name = read_crate_name(&addon_dir).unwrap_or_else(|| {
                format!("tentaflow_addon_{}", addon_name)
            });
            let wasm_filename = format!("{}.wasm", wasm_crate_name);
            let wasm_path = addon_dir
                .join("target/wasm32-wasip1/release")
                .join(&wasm_filename);

            if !wasm_path.exists() {
                println!(
                    "cargo:warning=Addon '{}' — brak pliku .wasm: {}, pomijam",
                    addon_name,
                    wasm_path.display()
                );
                continue;
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

    println!("cargo:warning=Certyfikaty TLS nie znalezione — generuje self-signed (rcgen, pure Rust)...");

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

    let cert = params.self_signed(&key_pair)
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
            println!("cargo:warning=Nie udalo sie uruchomic rustup — pomijam sprawdzanie WASM target");
            false
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

    code.push_str("// =============================================================================\n");
    code.push_str("// Plik: bundled_addons.rs (auto-generated by build.rs)\n");
    code.push_str("// Opis: Osadzone addony WASM — skompilowane z binarka.\n");
    code.push_str("//       NIE EDYTUJ RECZNIE — generowane automatycznie.\n");
    code.push_str("// =============================================================================\n\n");

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
    code.push_str("}\n\n");

    code.push_str("/// Lista wszystkich wbudowanych addonow\n");
    code.push_str("pub const BUNDLED_ADDONS: &[BundledAddon] = &[\n");

    for addon in addons {
        let wasm_path = addon.bundle_path.join("addon.wasm");
        let manifest_path = addon.bundle_path.join("manifest.toml");
        let skill_path = addon.bundle_path.join("SKILL.md");
        let desc_path = addon.bundle_path.join("DESCRIPTION.md");
        let blocks_path = addon.bundle_path.join("blocks.json");

        // Plik WASM i manifest musza istniec
        if !wasm_path.exists() || !manifest_path.exists() {
            continue;
        }

        code.push_str("    BundledAddon {\n");
        code.push_str(&format!(
            "        name: \"{}\",\n",
            addon.name
        ));
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

/// Skanuje wwwroot/ rekurencyjnie i generuje wwwroot_embed.rs z include_bytes!
/// dla kazdego pliku. Rejestruje rerun-if-changed na kazdym pliku zeby cargo
/// automatycznie rekompilowalo po zmianie jakiegokolwiek zasobu www.
fn generate_wwwroot_embed(out_dir: &Path) {
    let wwwroot = Path::new("wwwroot");
    if !wwwroot.exists() {
        // Brak wwwroot — generuj pusta funkcje lookup
        let code = "fn wwwroot_lookup(_path: &str) -> Option<(&'static str, &'static [u8])> { None }\n";
        std::fs::write(out_dir.join("wwwroot_embed.rs"), code).unwrap();
        return;
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_wwwroot_files(wwwroot, wwwroot, &mut files);

    // Rejestruj rerun-if-changed na kazdym pliku — cargo ZAWSZE rekompiluje
    // gdy jakikolwiek plik www sie zmieni
    println!("cargo:rerun-if-changed=wwwroot");
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

    // Generuj funkcje lookup
    code.push_str("fn wwwroot_lookup(path: &str) -> Option<(&'static str, &'static [u8])> {\n");
    code.push_str("    match path {\n");

    for (i, (rel_path, _)) in files.iter().enumerate() {
        let mime = guess_mime(rel_path);
        code.push_str(&format!(
            "        \"{}\" => Some((\"{}\", WWWROOT_FILE_{})),\n",
            rel_path, mime, i
        ));
    }

    code.push_str("        _ => None,\n");
    code.push_str("    }\n");
    code.push_str("}\n");

    std::fs::write(out_dir.join("wwwroot_embed.rs"), code).unwrap();
}

/// Rekurencyjnie zbiera pliki z katalogu wwwroot
fn collect_wwwroot_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_wwwroot_files(base, &path, out);
        } else if path.is_file() {
            let rel = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
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
        _ => "application/octet-stream",
    }
}

// =============================================================================
// Pakowanie kontekstu Docker (tentaflow-containers + tentaflow-protocol)
// w tar.gz wbudowany w binarce. Pozwala na deploy bez zewnetrznych zrodel.
// =============================================================================

fn pack_container_contexts(out_dir: &Path) {
    use std::process::Command;

    let workspace_root = Path::new("..").canonicalize().unwrap_or_else(|_| PathBuf::from(".."));
    let containers_dir = workspace_root.join("tentaflow-containers");
    let protocol_dir = workspace_root.join("tentaflow-protocol");

    if !containers_dir.exists() || !protocol_dir.exists() {
        println!(
            "cargo:warning=pack_container_contexts: brak {} albo {} — embed pominiety",
            containers_dir.display(),
            protocol_dir.display()
        );
        // Stworz pusty plik zeby include_bytes! nie padlo
        std::fs::write(out_dir.join("container_bundle.tar.gz"), b"").ok();
        return;
    }

    // Zmiany w kontekstach trigerują rebuild
    println!("cargo:rerun-if-changed={}", containers_dir.display());
    println!("cargo:rerun-if-changed={}", protocol_dir.display());

    let bundle_path = out_dir.join("container_bundle.tar.gz");

    // Wykluczamy `target/`, `node_modules/`, `.git/`, zeby nie wciskac
    // kilkudziesieciu MB binarek do binarki.
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&bundle_path)
        .arg("--exclude=target")
        .arg("--exclude=node_modules")
        .arg("--exclude=.git")
        .arg("-C")
        .arg(&workspace_root)
        .arg("tentaflow-containers")
        .arg("tentaflow-protocol")
        .status();

    match status {
        Ok(s) if s.success() => {
            let size = std::fs::metadata(&bundle_path).map(|m| m.len()).unwrap_or(0);
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
// waliduje semantycznie 9 regul ze SCHEMA.md i emituje:
//   - $OUT_DIR/services_generated.rs       (Rust const z embedded JSON)
//   - wwwroot/js/generated/services-manifest.js  (ESM module dla GUI)
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
        #[serde(default, rename = "variant")]
        pub variants: Vec<Variant>,
        #[serde(default, rename = "model_preset")]
        pub model_presets: Vec<ModelPreset>,
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
        pub api: ApiKind,
        pub default_port: u16,
        pub version: String,
        #[serde(default)]
        pub tags: Vec<String>,
        #[serde(default)]
        pub also_serves: Vec<Category>,
        #[serde(default)]
        pub docs_url: Option<String>,
        #[serde(default)]
        pub icon: Option<String>,
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
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum ApiKind {
        OpenaiCompatible,
        OllamaNative,
        SherpaTts,
        SherpaStt,
        Comfyui,
        Custom,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Variant {
        pub id: String,
        pub deploy_mode: DeployMode,
        pub target_os: OsList,
        pub target_arch: ArchList,
        pub gpu_backend: GpuBackendList,
        pub status: Status,
        #[serde(default)]
        pub vram_gb_min: Option<u16>,
        #[serde(default)]
        pub ram_gb_min: Option<u16>,
        #[serde(default)]
        pub disk_gb_min: Option<u16>,
        #[serde(default)]
        pub notes_pl: Option<String>,
        #[serde(default)]
        pub notes_en: Option<String>,
        #[serde(default)]
        pub build: Option<BuildOption>,
        #[serde(default)]
        pub download: Option<DownloadOption>,
        #[serde(default)]
        pub feature_flag: Option<FeatureFlagSpec>,
        #[serde(default)]
        pub detection: Option<DetectionSpec>,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum DeployMode {
        Native,
        Docker,
        PythonBundle,
        Embedded,
        External,
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

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "lowercase")]
    pub enum TargetArch {
        X86_64,
        Aarch64,
        Any,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "lowercase")]
    pub enum GpuBackend {
        Cpu,
        Cuda,
        Rocm,
        Vulkan,
        Metal,
        Mlx,
        Xpu,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum Status {
        Stable,
        Experimental,
        Planned,
        Deprecated,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct BuildOption {
        pub context_path: String,
        #[serde(default = "default_dockerfile")]
        pub dockerfile: String,
        #[serde(default)]
        pub build_args: HashMap<String, String>,
        #[serde(default)]
        pub tags: Vec<String>,
    }
    fn default_dockerfile() -> String {
        "Dockerfile".to_string()
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DownloadOption {
        pub image: String,
        pub digest: String,
        #[serde(default)]
        pub size_mb: Option<u64>,
        #[serde(default = "default_license_required")]
        pub license_required: RequiredLicenseTier,
        #[serde(default)]
        pub enabled: bool,
    }
    fn default_license_required() -> RequiredLicenseTier {
        RequiredLicenseTier::Pro
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum RequiredLicenseTier {
        Pro,
        Enterprise,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct FeatureFlagSpec {
        pub name: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DetectionSpec {
        pub binary: String,
        pub endpoint: String,
        #[serde(default = "default_health_path")]
        pub health_path: String,
    }
    fn default_health_path() -> String {
        "/".to_string()
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ModelPreset {
        pub id: String,
        pub display_name: String,
        pub repo: String,
        #[serde(default)]
        pub quantization: Option<String>,
        #[serde(default)]
        pub vram_gb_min: Option<u16>,
        #[serde(default)]
        pub recommended: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum OsList {
        Single(TargetOs),
        Multi(Vec<TargetOs>),
    }
    impl OsList {
        pub fn as_vec(&self) -> Vec<TargetOs> {
            match self {
                OsList::Single(o) => vec![*o],
                OsList::Multi(v) => v.clone(),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum ArchList {
        Single(TargetArch),
        Multi(Vec<TargetArch>),
    }
    impl ArchList {
        pub fn as_vec(&self) -> Vec<TargetArch> {
            match self {
                ArchList::Single(a) => vec![*a],
                ArchList::Multi(v) => v.clone(),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum GpuBackendList {
        Single(GpuBackend),
        Multi(Vec<GpuBackend>),
    }
    impl GpuBackendList {
        pub fn as_vec(&self) -> Vec<GpuBackend> {
            match self {
                GpuBackendList::Single(b) => vec![*b],
                GpuBackendList::Multi(v) => v.clone(),
            }
        }
    }

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

    /// Walidacja semantyczna identyczna z runtime — 10 regul ze SCHEMA.md.
    pub fn validate(
        manifest: &ServiceManifest,
        containers_root: &std::path::Path,
    ) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();
        let eid = &manifest.engine.id;

        // Reguła 10: engine.id whitelist regex.
        if !is_valid_engine_id(eid) {
            errors.push(format!(
                "engine id = '{}' nie spelnia wymaganego formatu \
                 '^[a-z0-9][a-z0-9_-]{{0,63}}$' (1-64 znakow, kebab/snake_case)",
                eid
            ));
        }

        let mut seen_variant_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for v in &manifest.variants {
            if !seen_variant_ids.insert(v.id.clone()) {
                errors.push(format!(
                    "engine '{}' duplikat variant.id = '{}'",
                    eid, v.id
                ));
            }
            let os_list = v.target_os.as_vec();
            let backend_list = v.gpu_backend.as_vec();

            for &b in &backend_list {
                let required: Vec<TargetOs> = match b {
                    GpuBackend::Metal => vec![TargetOs::Macos, TargetOs::Ios],
                    GpuBackend::Mlx => vec![TargetOs::Macos, TargetOs::Ios],
                    GpuBackend::Cuda => vec![TargetOs::Linux, TargetOs::Windows],
                    GpuBackend::Rocm => vec![TargetOs::Linux],
                    GpuBackend::Xpu => vec![TargetOs::Linux, TargetOs::Windows],
                    GpuBackend::Cpu | GpuBackend::Vulkan => continue,
                };
                if !os_list.iter().all(|o| required.contains(o)) {
                    errors.push(format!(
                        "engine '{}' wariant '{}': gpu_backend = {:?} wymaga \
                         target_os in {:?}, ale jest {:?}",
                        eid, v.id, b, required, os_list
                    ));
                }
            }

            if backend_list.contains(&GpuBackend::Mlx)
                && v.deploy_mode != DeployMode::Embedded
            {
                errors.push(format!(
                    "engine '{}' wariant '{}': gpu_backend = mlx wymaga \
                     deploy_mode = embedded, jest {:?}",
                    eid, v.id, v.deploy_mode
                ));
            }

            if v.deploy_mode == DeployMode::Docker {
                let invalid = os_list
                    .iter()
                    .any(|o| !matches!(o, TargetOs::Linux | TargetOs::Windows));
                if invalid {
                    errors.push(format!(
                        "engine '{}' wariant '{}': deploy_mode = docker dziala \
                         tylko na linux/windows, jest {:?}",
                        eid, v.id, os_list
                    ));
                }
            }

            match v.deploy_mode {
                DeployMode::Docker | DeployMode::Native | DeployMode::PythonBundle => {
                    if v.build.is_none() {
                        errors.push(format!(
                            "engine '{}' wariant '{}': deploy_mode = {:?} wymaga \
                             sekcji [variant.build]",
                            eid, v.id, v.deploy_mode
                        ));
                    }
                }
                DeployMode::Embedded => {
                    if v.feature_flag.is_none() {
                        errors.push(format!(
                            "engine '{}' wariant '{}': deploy_mode = embedded \
                             wymaga sekcji [variant.feature_flag]",
                            eid, v.id
                        ));
                    }
                }
                DeployMode::External => {
                    if v.detection.is_none() {
                        errors.push(format!(
                            "engine '{}' wariant '{}': deploy_mode = external \
                             wymaga sekcji [variant.detection]",
                            eid, v.id
                        ));
                    }
                }
            }

            if let Some(b) = &v.build {
                let full = containers_root.join(&b.context_path);
                if !full.is_dir() {
                    errors.push(format!(
                        "engine '{}' wariant '{}': context_path '{}' nie istnieje \
                         na dysku ({})",
                        eid,
                        v.id,
                        b.context_path,
                        full.display()
                    ));
                }
            }

            if let Some(dl) = &v.download {
                if dl.enabled {
                    let valid = dl.digest.starts_with("sha256:")
                        && dl.digest.len() == 71
                        && dl.digest[7..].chars().all(|c| c.is_ascii_hexdigit());
                    if !valid {
                        errors.push(format!(
                            "engine '{}' wariant '{}': download.enabled = true wymaga \
                             digest sha256:<64 hex znakow>",
                            eid, v.id
                        ));
                    } else {
                        let is_zero = dl.digest.len() == 71
                            && dl.digest.starts_with("sha256:")
                            && dl.digest[7..].bytes().all(|b| b == b'0');
                        if is_zero {
                            errors.push(format!(
                                "engine '{}' wariant '{}': download.enabled = true z \
                                 placeholder digest sha256:00...00 — uzupelnij \
                                 prawdziwy digest przed publikacja artefaktu",
                                eid, v.id
                            ));
                        }
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn generate_services_manifest(out_dir: &Path) {
    use services_manifest_build::{validate, ServiceManifest};
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
        write_js_module(Path::new("wwwroot/js/generated/services-manifest.js"), "[]");
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
        // Rerun-if-changed dla calego katalogu kategorii _services.
        println!("cargo:rerun-if-changed={}", services_dir.display());

        let svc_entries = match std::fs::read_dir(&services_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for svc in svc_entries.flatten() {
            let p = svc.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
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
            Err(e) => panic!(
                "Nie mozna odczytac manifestu '{}': {}",
                file.display(),
                e
            ),
        };

        let manifest: ServiceManifest = match toml::from_str(&content) {
            Ok(m) => m,
            Err(e) => panic!(
                "Bledny TOML w manifescie '{}':\n  {}",
                file.display(),
                e
            ),
        };

        // Walidacja semantyczna — 9 regul.
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

        // Reguła 9 (czesc globalna): unikalnosc engine.id cross-file.
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

    // Serializuj wszystko do JSON. pretty dla GUI, compact dla embed Rust (size).
    let json_compact = serde_json::to_string(&loaded)
        .expect("Bug: ServiceManifest powinien serializowac sie do JSON bez bledow");
    let json_pretty = serde_json::to_string_pretty(&loaded)
        .expect("Bug: ServiceManifest powinien serializowac sie do JSON bez bledow");

    write_generated(out_dir, &json_compact);

    // GUI module — zapisujemy do wwwroot, ale podajemy sciezke wzgledem build.rs CWD.
    let js_path = Path::new("wwwroot/js/generated/services-manifest.js");
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
    let now = chrono_now_iso();
    let content = format!(
        "// =============================================================================\n\
         // Plik: services-manifest.js\n\
         // Opis: AUTO-GENERATED przez build.rs — nie edytuj recznie.\n\
         //       Zrodlo: tentaflow-containers/*/_services/*.toml\n\
         // =============================================================================\n\
         \n\
         export const SCHEMA_VERSION = 1;\n\
         export const GENERATED_AT = \"{}\";\n\
         export const SERVICES = {};\n",
        now, json_pretty
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
