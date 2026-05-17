// =============================================================================
// Plik: tentaflow-cli/src/commands/addon.rs
// Opis: Podkomenda `tentaflow-cli addon validate <path>` — wczytuje
//       manifest.toml addonu, parsuje, waliduje rozszerzenia F1a
//       (sekcje storage / alias / gate / vector_namespace / flow_template
//       / ui_component), sprawdza obecnosc plikow referowanych (wasm_file,
//       migrations_dir, flow_template.path, ui_component.src) oraz
//       kompatybilnosc sdk_version z rdzeniem. Output po polsku.
//       Exit 0 = OK, 1 = bledy walidacji.
// =============================================================================

use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tentaflow_core::addon::lifecycle::parse_manifest_toml;
use tentaflow_core::addon::manifest::{validate_manifest_extensions, validate_publisher_pk_b64};
use tentaflow_core::addon::sdk_version::{check_compatibility, CORE_SDK_VERSION};
use tentaflow_core::addon::signature::verify_ui_component_bundle;
use tentaflow_core::addon::AddonManifest;
use tentaflow_core::db;
use tentaflow_core::db::repository as repo;
use tentaflow_core::util::path_safety::safe_resolve;

#[derive(Subcommand, Debug)]
pub enum AddonCommand {
    /// Waliduje manifest addonu i strukture katalogu.
    Validate {
        /// Sciezka do katalogu addonu (z manifest.toml) lub do samego pliku
        /// manifestu.
        path: PathBuf,
    },
    /// Dodaje klucz publiczny Ed25519 wydawcy do trust store (F1c P2).
    TrustKey {
        /// Klucz publiczny Ed25519, 32 bajty zakodowane base64 (44 znaki).
        key_b64: String,
        /// Czytelna nazwa wydawcy (np. "ACME Sp. z o.o.").
        #[arg(long)]
        label: String,
        /// Opcjonalny kanal kontaktu (email lub URL).
        #[arg(long)]
        contact: Option<String>,
        /// Sciezka do pliku DB tentaflow.db.
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Listuje wszystkich zaufanych wydawcow z trust store.
    ListTrusted {
        /// Sciezka do pliku DB tentaflow.db.
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Usuwa klucz wydawcy z trust store (nie wplywa na juz zainstalowane addony).
    UntrustKey {
        /// Klucz publiczny Ed25519 do usuniecia (base64).
        key_b64: String,
        /// Sciezka do pliku DB tentaflow.db.
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
    /// Weryfikuje bundle UI bez instalacji addona (dry-run signature check).
    VerifyBundle {
        /// Sciezka do pliku bundle (JS / archive).
        bundle_path: PathBuf,
        /// Klucz publiczny wydawcy (base64).
        #[arg(long = "publisher-key")]
        publisher_key: String,
        /// Sygnatura `ed25519:<base64>` lub samo base64 (64 bajty).
        #[arg(long)]
        signature: String,
        /// Sciezka do pliku DB tentaflow.db (trust store sprawdza, czy klucz jest zaufany).
        #[arg(long, default_value = "tentaflow.db")]
        db: PathBuf,
    },
}

pub fn run(cmd: AddonCommand) -> ExitCode {
    match cmd {
        AddonCommand::Validate { path } => match validate(&path) {
            Ok(report) => {
                print_report(&report);
                if report.errors.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => {
                eprintln!("Blad krytyczny: {e}");
                ExitCode::from(1)
            }
        },
        AddonCommand::TrustKey {
            key_b64,
            label,
            contact,
            db,
        } => run_trust_key(&key_b64, &label, contact.as_deref(), &db),
        AddonCommand::ListTrusted { db } => run_list_trusted(&db),
        AddonCommand::UntrustKey { key_b64, db } => run_untrust_key(&key_b64, &db),
        AddonCommand::VerifyBundle {
            bundle_path,
            publisher_key,
            signature,
            db,
        } => run_verify_bundle(&bundle_path, &publisher_key, &signature, &db),
    }
}

fn run_trust_key(key_b64: &str, label: &str, contact: Option<&str>, db_path: &Path) -> ExitCode {
    if label.trim().is_empty() {
        eprintln!("Blad: --label nie moze byc pusty");
        return ExitCode::from(1);
    }
    if let Err(e) = validate_publisher_pk_b64(key_b64) {
        eprintln!("Blad: {e}");
        return ExitCode::from(1);
    }
    let pool = match db::init(db_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Nie mozna otworzyc DB {}: {e}", db_path.display());
            return ExitCode::from(1);
        }
    };
    match repo::insert_trusted_publisher(&pool, key_b64, label, contact, None) {
        Ok(1) => {
            println!("OK: klucz dodany do trust store ({label})");
            ExitCode::SUCCESS
        }
        Ok(_) => {
            println!("UWAGA: klucz juz byl w trust store (bez zmian)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Blad insert: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_list_trusted(db_path: &Path) -> ExitCode {
    let pool = match db::init(db_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Nie mozna otworzyc DB {}: {e}", db_path.display());
            return ExitCode::from(1);
        }
    };
    match repo::list_trusted_publishers(&pool) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("(trust store pusty — zaden zewnetrzny addon z UI nie zainstaluje sie)");
            } else {
                println!("{:<46} {:<32} {:<25} {}", "key_b64", "label", "added_at", "contact");
                for r in rows {
                    println!(
                        "{:<46} {:<32} {:<25} {}",
                        r.key_b64,
                        r.label,
                        r.added_at,
                        r.contact.as_deref().unwrap_or("-")
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Blad zapytania: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_untrust_key(key_b64: &str, db_path: &Path) -> ExitCode {
    let pool = match db::init(db_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Nie mozna otworzyc DB {}: {e}", db_path.display());
            return ExitCode::from(1);
        }
    };
    match repo::remove_trusted_publisher(&pool, key_b64) {
        Ok(true) => {
            println!("OK: klucz usuniety z trust store");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("UWAGA: klucza nie bylo w trust store");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Blad delete: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_verify_bundle(
    bundle_path: &Path,
    publisher_key: &str,
    signature: &str,
    db_path: &Path,
) -> ExitCode {
    if !bundle_path.exists() {
        eprintln!("Blad: bundle '{}' nie istnieje", bundle_path.display());
        return ExitCode::from(1);
    }
    let pool = match db::init(db_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Nie mozna otworzyc DB {}: {e}", db_path.display());
            return ExitCode::from(1);
        }
    };
    match verify_ui_component_bundle(bundle_path, publisher_key, signature, &pool) {
        Ok(()) => {
            println!("OK: signature zweryfikowana, wydawca zaufany");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("BLAD weryfikacji: {e}");
            ExitCode::from(1)
        }
    }
}

/// Raport z walidacji manifestu.
pub struct ValidationReport {
    pub addon_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: Option<AddonManifest>,
    pub infos: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Uruchamia pelna walidacje manifestu i zwraca raport.
///
/// `path` moze wskazywac:
/// - katalog addonu zawierajacy `manifest.toml`,
/// - bezposrednio na plik `manifest.toml` (uzywane przez testy z fixtures).
pub fn validate(path: &Path) -> anyhow::Result<ValidationReport> {
    let (manifest_path, addon_dir) = resolve_paths(path)?;

    let mut report = ValidationReport {
        addon_dir: addon_dir.clone(),
        manifest_path: manifest_path.clone(),
        manifest: None,
        infos: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            report
                .errors
                .push(format!("Nie mozna odczytac {manifest_path:?}: {e}"));
            return Ok(report);
        }
    };

    let manifest = match parse_manifest_toml(&content) {
        Ok(m) => m,
        Err(e) => {
            report.errors.push(format!("Blad parsowania manifestu: {e}"));
            return Ok(report);
        }
    };

    report.infos.push(format!(
        "Manifest wczytany: {} v{}",
        manifest.addon_id, manifest.version
    ));
    report
        .infos
        .push(format!("Permissions: {} zadeklarowane", manifest.declared_permissions.len()));
    let gated_aliases = manifest
        .aliases
        .iter()
        .filter(|a| a.gate.is_some())
        .count();
    report.infos.push(format!(
        "Aliasy AI: {} zadeklarowane ({gated_aliases} z gate)",
        manifest.aliases.len()
    ));
    report
        .infos
        .push(format!("Network rules: {}", manifest.network_rules.len()));
    report.infos.push(format!("Gates: {}", manifest.gates.len()));
    let class_c = manifest
        .vector_namespaces
        .iter()
        .filter(|v| v.data_class == "C")
        .count();
    report.infos.push(format!(
        "Vector namespaces: {} ({class_c} klasa C)",
        manifest.vector_namespaces.len()
    ));
    report
        .infos
        .push(format!("Flow templates: {}", manifest.flow_templates.len()));
    report
        .infos
        .push(format!("UI components: {}", manifest.ui_components.len()));

    // Walidacja cross-sekcyjna F1a (duplicate ids, enumy, signature ed25519).
    if let Err(e) = validate_manifest_extensions(
        manifest.storage.as_ref(),
        &manifest.aliases,
        &manifest.gates,
        &manifest.vector_namespaces,
        &manifest.flow_templates,
        &manifest.ui_components,
        manifest.sdk_version.as_deref(),
        &manifest.uses_aliases,
        &manifest.uses_models,
        manifest.publisher.as_ref(),
    ) {
        report
            .errors
            .push(format!("Walidacja rozszerzen manifestu: {e}"));
    }

    // SDK compat — pomijamy jesli walidacja juz zlapala bledny semver.
    match check_compatibility(manifest.sdk_version.as_deref()) {
        Ok(()) => {
            let label = manifest
                .sdk_version
                .clone()
                .unwrap_or_else(|| "(brak — kompatybilnosc zalozona)".to_string());
            report.infos.push(format!(
                "SDK version: {label} kompatybilne z core {CORE_SDK_VERSION}"
            ));
        }
        Err(e) => {
            report.errors.push(format!("SDK version: {e}"));
        }
    }

    // Pliki referowane: wasm, migrations_dir, flow_template.path, ui_component.src.
    // wasm_file traktujemy jako soft warning — to build artifact (target/wasm32-wasip1/release/),
    // walidacja zrodel addonu nie powinna padac gdy uzytkownik jeszcze nie zbudowal.
    check_file_soft(&addon_dir, &manifest.wasm_file, "addon.wasm_file", &mut report);

    if let Some(storage) = &manifest.storage {
        if storage.sql {
            let mig_dir = addon_dir.join(&storage.migrations_dir);
            if !mig_dir.is_dir() {
                report.errors.push(format!(
                    "storage.sql=true ale katalog migracji '{}' nie istnieje",
                    storage.migrations_dir
                ));
            } else {
                let n_sql = std::fs::read_dir(&mig_dir)
                    .map(|it| {
                        it.filter_map(|e| e.ok())
                            .filter(|e| {
                                e.path().extension().and_then(|s| s.to_str()) == Some("sql")
                            })
                            .count()
                    })
                    .unwrap_or(0);
                report
                    .infos
                    .push(format!("Migracje SQL: {n_sql} plikow w '{}'", storage.migrations_dir));
            }
        }
    }

    for ft in &manifest.flow_templates {
        check_file(
            &addon_dir,
            &ft.path,
            &format!("flow_template '{}'", ft.id),
            &mut report,
        );
    }

    for uic in &manifest.ui_components {
        check_file(
            &addon_dir,
            &uic.src,
            &format!("ui_component '{}'", uic.id),
            &mut report,
        );
    }

    report.manifest = Some(manifest);
    Ok(report)
}

fn resolve_paths(path: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    if path.is_file() {
        let dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok((path.to_path_buf(), dir))
    } else if path.is_dir() {
        let mp = path.join("manifest.toml");
        Ok((mp, path.to_path_buf()))
    } else {
        anyhow::bail!("sciezka '{}' nie istnieje", path.display());
    }
}

fn check_file(dir: &Path, rel: &str, label: &str, report: &mut ValidationReport) {
    match safe_resolve(dir, rel) {
        Ok(_) => report.infos.push(format!("{label}: '{rel}' istnieje")),
        Err(e) => report.errors.push(format!("{label}: {e}")),
    }
}

fn check_file_soft(dir: &Path, rel: &str, label: &str, report: &mut ValidationReport) {
    match safe_resolve(dir, rel) {
        Ok(_) => report.infos.push(format!("{label}: '{rel}' istnieje")),
        Err(e) => report.warnings.push(format!(
            "{label}: plik '{rel}' nie dostepny ({e}); zbuduj addon przed pakowaniem"
        )),
    }
}

fn print_report(r: &ValidationReport) {
    // Kolorowanie przez ANSI codes — gdy stdout nie jest tty, terminal ignoruje.
    const GREEN: &str = "\x1b[32m";
    const RED: &str = "\x1b[31m";
    const RESET: &str = "\x1b[0m";
    const BOLD: &str = "\x1b[1m";

    println!("{BOLD}Walidacja addonu: {}{RESET}", r.manifest_path.display());
    println!("Katalog: {}", r.addon_dir.display());
    println!();

    for info in &r.infos {
        println!("{GREEN}OK{RESET}  {info}");
    }

    const YELLOW: &str = "\x1b[33m";
    for w in &r.warnings {
        println!("{YELLOW}UWAGA{RESET} {w}");
    }

    if !r.errors.is_empty() {
        println!();
        for err in &r.errors {
            println!("{RED}BLAD{RESET} {err}");
        }
        println!();
        println!(
            "{RED}{}{RESET} {} — manifest niepoprawny, NIE instaluj.",
            "Wynik:",
            plural_errors(r.errors.len())
        );
    } else {
        println!();
        println!("{GREEN}Wynik:{RESET} manifest poprawny. Mozna instalowac.");
    }
}

fn plural_errors(n: usize) -> String {
    match n {
        1 => "1 blad".to_string(),
        2..=4 => format!("{n} bledy"),
        _ => format!("{n} bledow"),
    }
}
