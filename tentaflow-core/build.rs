// =============================================================================
// Plik: build.rs
// Opis: Build script — kompiluje addony do WASM (wasm32-wasip1) i pakuje je
//       jako dane osadzone w binarce (include_bytes!/include_str!).
//       Aktywny tylko gdy feature addon-runtime jest wlaczony.
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out_dir_env = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Generuj wwwroot_embed.rs — pliki statyczne wbudowane w binarie
    generate_wwwroot_embed(&out_dir_env);

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
