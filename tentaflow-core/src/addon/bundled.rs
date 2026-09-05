// =============================================================================
// Plik: addon/bundled.rs
// Opis: Obsluga wbudowanych addonow — osadzonych w binarce przez build.rs.
//       Automatycznie instaluje lub aktualizuje bundled addony przy starcie
//       aplikacji (Router, Desktop, Mobile).
// =============================================================================

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tracing::{error, info};

use crate::db::DbPool;

// Wlacz wygenerowany plik z osadzonymi addonami
include!(concat!(env!("OUT_DIR"), "/bundled_addons.rs"));

// =============================================================================
// Instalacja wbudowanych addonow
// =============================================================================

/// Reconcile wbudowanych addonow przy starcie. CATALOG-ONLY: materializuje
/// kazdy bundled pakiet (szablon) na dysk i wpisuje wersje do katalogu
/// `addon_packages`. NIE tworzy instancji — te user instaluje z katalogu
/// (`lifecycle::install_instance`), kazda do wlasnego addon_id + danych.
///
/// Dodatkowo czysci pozostalosci sprzed splitu pakiet/instancja (gdy bundled
/// addony byly auto-instalowane wprost do `addons`) — patrz
/// [`prune_pre_split_bundled_installs`].
pub fn install_bundled_addons(db: &DbPool) -> Result<()> {
    if BUNDLED_ADDONS.is_empty() {
        info!("Brak wbudowanych addonow do zainstalowania");
        return Ok(());
    }

    info!(
        "Reconcile {} wbudowanych pakietow do katalogu (WASM total: {} bytes)...",
        BUNDLED_ADDONS.len(),
        BUNDLED_ADDONS
            .iter()
            .map(|a| a.wasm_bytes.len())
            .sum::<usize>()
    );

    std::fs::create_dir_all(packages_root())
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu pakietow addonow: {e}"))?;

    let mut reconciled: std::collections::HashSet<String> = std::collections::HashSet::new();
    for addon in BUNDLED_ADDONS {
        match install_single_bundled_addon(addon, db) {
            Ok(package_id) => {
                reconciled.insert(package_id);
            }
            Err(e) => {
                error!("Blad reconcile wbudowanego pakietu '{}': {}", addon.name, e);
                // Kontynuuj z nastepnym addonem — nie przerywaj calego procesu
            }
        }
    }

    // Prune tylko pakietow, ktorych katalog faktycznie sie odswiezyl — inaczej
    // mozna by usunac legacy instalacje zanim szablon trafi do katalogu, czyniac
    // reinstall niemozliwym.
    prune_pre_split_bundled_installs(db, &reconciled);

    Ok(())
}

/// Native core applications registered into the same package catalog as WASM
/// addons (app-platform). Each entry is `(package name, manifest.toml)` — the
/// code lives in core, so the "bundle" is the manifest alone. The list grows
/// as Studios are retrofitted (plan-01 P2) and new native apps land.
const NATIVE_APP_PACKAGES: &[(&str, &str)] = &[
    (
        "Benchmark Studio",
        include_str!("../benchmark/app-manifest.toml"),
    ),
    ("ML Studio", include_str!("../ml_studio/app-manifest.toml")),
    (
        "Projekty",
        include_str!("../project_studio/app-manifest.toml"),
    ),
    (
        "Code Studio",
        include_str!("../code_studio/app-manifest.toml"),
    ),
    ("Meeting Bot", include_str!("../meeting/app-manifest.toml")),
    ("TentaNas", include_str!("../tentanas/app-manifest.toml")),
    (
        "TentaQuant",
        include_str!("../tentaquant/app-manifest.toml"),
    ),
    ("TentaBus", include_str!("../bus/app-manifest.toml")),
];

/// Manifest of a bundled native package by its `[addon].id`. Test fixtures
/// persist it on instance rows the way `lifecycle::install_instance` does, so
/// instance-manifest readers (`app_db::open`) meet the real `native.db_file`.
#[cfg(test)]
pub(crate) fn native_manifest(package_id: &str) -> Option<&'static str> {
    NATIVE_APP_PACKAGES.iter().map(|(_, m)| *m).find(|m| {
        crate::addon::lifecycle::parse_manifest_toml(m)
            .map(|parsed| parsed.addon_id == package_id)
            .unwrap_or(false)
    })
}

/// Reconcile native app packages into the catalog at boot. CATALOG-ONLY, the
/// exact mirror of [`install_bundled_addons`]: materializes `manifest.toml`
/// under `packages/{id}/{version}/` and upserts the `addon_packages` row with
/// `source = 'native'`. Instance lifecycle is a separate platform path.
pub fn install_native_packages(db: &DbPool) -> Result<()> {
    std::fs::create_dir_all(packages_root())
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu pakietow addonow: {e}"))?;

    for (name, manifest_toml) in NATIVE_APP_PACKAGES {
        if let Err(e) = install_single_native_package(db, name, manifest_toml) {
            error!("Blad reconcile natywnego pakietu '{}': {}", name, e);
            // Continue with the next package — one broken manifest must not
            // hide the rest of the native catalog.
        }
    }
    Ok(())
}

fn install_single_native_package(db: &DbPool, name: &str, manifest_toml: &str) -> Result<String> {
    let manifest = crate::addon::lifecycle::parse_manifest_toml(manifest_toml)?;
    if !manifest.is_native() {
        anyhow::bail!(
            "pakiet '{}' zarejestrowany jako natywny, ale manifest nie ma runtime = \"native\"",
            name
        );
    }
    // The package id becomes a path segment of the instance data dir, so it
    // must satisfy the stricter fs containment rule, not just the manifest one.
    crate::addon::fs_sandbox::validate_addon_id(&manifest.addon_id)
        .map_err(|e| anyhow::anyhow!("native package id '{}': {e:?}", manifest.addon_id))?;

    let dir = package_dir(&manifest.addon_id, &manifest.version);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("manifest.toml"), manifest_toml)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac manifest.toml: {e}"))?;

    let hash = {
        let mut hasher = Sha256::new();
        hasher.update(manifest_toml.as_bytes());
        hex::encode(hasher.finalize())
    };
    crate::db::repository::upsert_addon_package(
        db,
        &manifest.addon_id,
        &manifest.version,
        name,
        manifest_toml,
        &hash,
        "native",
    )?;
    info!(
        "Pakiet natywny '{}' v{} dostepny w katalogu",
        manifest.addon_id, manifest.version
    );
    Ok(manifest.addon_id)
}

/// Jednorazowa migracja przejsciowa: usuwa pozostalosci sprzed splitu
/// pakiet/instancja. Przed splitem bundled addony byly instalowane wprost do
/// tabeli `addons` z `addon_id == <id pakietu>` (bez sufiksu instancji). W nowym
/// modelu bundled to TYLKO katalog — wiec taki wiersz usuwamy wraz z danymi
/// scoped (per-instancyjny SQLite/katalog + config/flow). Instancje uzytkownika
/// (`<id pakietu>-<hex>`) nie pasuja do warunku i pozostaja nietkniete.
///
/// Best-effort: blad jednej instancji nie przerywa reszty. `reconciled` to
/// pakiety, ktorych katalog sie odswiezyl w tym starcie — tylko ich dotykamy.
fn prune_pre_split_bundled_installs(db: &DbPool, reconciled: &std::collections::HashSet<String>) {
    for package_id in reconciled {
        // Defensywnie: bundled id NIE moze wygladac jak instancja (`<base>-<8hex>`),
        // inaczej skasowalibysmy realna instancje uzytkownika o tym ksztalcie.
        if looks_like_instance_id(package_id) {
            continue;
        }
        // Pre-split sygnatura: zainstalowany addon o addon_id DOKLADNIE rownym id
        // pakietu. Instancje maja sufiks `-<hex>`, wiec nigdy tu nie wpadaja.
        match crate::db::repository::get_addon(db, package_id) {
            Ok(Some(_)) => match crate::addon::lifecycle::uninstall_instance(package_id, db) {
                Ok(()) => info!(
                    "Migracja pakiet/instancja: usunieto pre-split instalacje bundled '{}' \
                         (teraz dostepna tylko w katalogu)",
                    package_id
                ),
                Err(e) => error!(
                    "Nie udalo sie usunac pre-split instalacji bundled '{}': {}",
                    package_id, e
                ),
            },
            Ok(None) => {}
            Err(e) => error!(
                "Blad sprawdzania pre-split instalacji '{}': {}",
                package_id, e
            ),
        }
    }
}

/// True gdy id ma ksztalt instancji `<base>-<8 hex>` (sufiks z
/// `unique_instance_id`). Bundled package id (np. `company-lookup`) nie pasuje —
/// jego ostatni segment nie jest dokladnie 8 znakami hex.
fn looks_like_instance_id(id: &str) -> bool {
    match id.rsplit_once('-') {
        Some((base, suffix)) => {
            !base.is_empty() && suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// Reconcile pojedynczego bundled pakietu do katalogu. Zwraca jego `package_id`
/// (potrzebny, by prune dotykal tylko pakietow, ktorych katalog faktycznie sie
/// odswiezyl).
fn install_single_bundled_addon(addon: &BundledAddon, db: &DbPool) -> Result<String> {
    // Parsuj manifest — wyciagnij addon_id i wersje
    let (addon_id, bundled_version) = match parse_addon_id_and_version(addon.manifest_toml) {
        Ok(v) => v,
        Err(e) => {
            error!("Nie udalo sie sparsowac manifest.toml dla '{}': {}\nManifest (pierwsze 200 znakow): {}", addon.name, e, &addon.manifest_toml[..addon.manifest_toml.len().min(200)]);
            return Err(anyhow::anyhow!(
                "Nie udalo sie sparsowac manifest.toml: {e}"
            ));
        }
    };

    let bundle_hash = compute_bundle_hash(addon);

    // Reconciler jest CATALOG-ONLY: materializuje pakiet (szablon) na dysku w
    // wersjonowanym ukladzie packages/{id}/{version}/ i wpisuje wersje do
    // katalogu `addon_packages`. NIE tworzy ani nie aktualizuje zadnej instancji
    // — instancje instaluje/aktualizuje user (lifecycle::install_instance /
    // update_instance). Dzieki temu kazda instancja przypina wlasna wersje i
    // aktualizuje sie niezaleznie (test przed prod).
    let dir = package_dir(&addon_id, &bundled_version);
    write_bundled_addon_files(&dir, addon)?;
    crate::db::repository::upsert_addon_package(
        db,
        &addon_id,
        &bundled_version,
        addon.name,
        addon.manifest_toml,
        &bundle_hash,
        "bundled",
    )?;
    info!(
        "Pakiet wbudowany '{}' v{} dostepny w katalogu",
        addon_id, bundled_version
    );

    Ok(addon_id)
}

fn write_bundled_addon_files(addon_dir: &std::path::Path, addon: &BundledAddon) -> Result<()> {
    std::fs::create_dir_all(&addon_dir)?;

    std::fs::write(addon_dir.join("addon.wasm"), addon.wasm_bytes)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac pliku WASM: {e}"))?;

    std::fs::write(addon_dir.join("manifest.toml"), addon.manifest_toml)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac manifest.toml: {e}"))?;

    if !addon.skill_md.is_empty() {
        std::fs::write(addon_dir.join("SKILL.md"), addon.skill_md).ok();
    }
    if !addon.description_md.is_empty() {
        std::fs::write(addon_dir.join("DESCRIPTION.md"), addon.description_md).ok();
    }
    if !addon.blocks_json.is_empty() {
        std::fs::write(addon_dir.join("blocks.json"), addon.blocks_json).ok();
    }
    write_bundled_migrations(addon_dir, addon)?;
    write_bundled_flows(addon_dir, addon)?;
    Ok(())
}

// =============================================================================
// Parsowanie manifestu — minimalne wyciagniecie addon_id i version
// =============================================================================

/// Parsuje manifest.toml i zwraca (addon_id, version).
/// Obsluguje dwa formaty manifestu:
/// - Nowy: [addon] id = "..." version = "..."
/// - Stary: addon_id = "..." version = "..."
fn parse_addon_id_and_version(manifest_toml: &str) -> Result<(String, String)> {
    let parsed: toml::Value = toml::from_str(manifest_toml)
        .map_err(|e| anyhow::anyhow!("Niepoprawny format manifest.toml: {e}"))?;

    // Nowy format: [addon] id = "...", version = "..."
    if let Some(addon_section) = parsed.get("addon") {
        let id = addon_section
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Brak pola addon.id w manifest.toml"))?;

        let version = addon_section
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Brak pola addon.version w manifest.toml"))?;

        return Ok((id.to_string(), version.to_string()));
    }

    // Stary format: addon_id = "...", version = "..."
    let id = parsed
        .get("addon_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak pola addon_id ani addon.id w manifest.toml"))?;

    let version = parsed
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak pola version w manifest.toml"))?;

    Ok((id.to_string(), version.to_string()))
}

fn compute_bundle_hash(addon: &BundledAddon) -> String {
    let mut hasher = Sha256::new();
    hash_chunk(&mut hasher, b"addon.wasm", addon.wasm_bytes);
    hash_chunk(
        &mut hasher,
        b"manifest.toml",
        addon.manifest_toml.as_bytes(),
    );
    hash_chunk(&mut hasher, b"SKILL.md", addon.skill_md.as_bytes());
    hash_chunk(
        &mut hasher,
        b"DESCRIPTION.md",
        addon.description_md.as_bytes(),
    );
    hash_chunk(&mut hasher, b"blocks.json", addon.blocks_json.as_bytes());
    for (name, sql) in addon.migrations {
        hash_chunk(&mut hasher, name.as_bytes(), sql.as_bytes());
    }
    for (name, content) in addon.flows {
        hash_chunk(&mut hasher, name.as_bytes(), content.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn write_bundled_migrations(addon_dir: &std::path::Path, addon: &BundledAddon) -> Result<()> {
    if addon.migrations.is_empty() {
        return Ok(());
    }
    let migrations_dir = addon_dir.join("migrations");
    std::fs::create_dir_all(&migrations_dir)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu migracji addonu: {e}"))?;
    for (name, sql) in addon.migrations {
        std::fs::write(migrations_dir.join(name), sql)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac migracji addonu '{name}': {e}"))?;
    }
    Ok(())
}

fn write_bundled_flows(addon_dir: &std::path::Path, addon: &BundledAddon) -> Result<()> {
    if addon.flows.is_empty() {
        return Ok(());
    }
    let flows_dir = addon_dir.join("flows");
    std::fs::create_dir_all(&flows_dir)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu flows addonu: {e}"))?;
    for (name, content) in addon.flows {
        std::fs::write(flows_dir.join(name), content)
            .map_err(|e| anyhow::anyhow!("Nie udalo sie zapisac flow addonu '{name}': {e}"))?;
    }
    Ok(())
}

fn hash_chunk(hasher: &mut Sha256, name: &[u8], bytes: &[u8]) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

// =============================================================================
// Sciezka do katalogu wbudowanych addonow
// =============================================================================

/// Bazowy katalog danych dla store'u pakietow. Ustawiany raz przy boocie przez
/// binarke (set_packages_base). `dirs::data_dir()` (default) NIE wskazuje
/// sandboxa na iOS/Android, dlatego mobile/desktop/main wstrzykuja swoj wlasny
/// katalog danych — pakiety leza wtedy obok bazy i zawsze w zapisywalnym miejscu.
static PACKAGES_BASE: OnceLock<PathBuf> = OnceLock::new();

/// Ustawia bazowy katalog danych store'u pakietow. Wolac RAZ przy boocie, PRZED
/// `install_bundled_addons` i startem runtime addonow. Idempotentne (kolejne
/// wywolania ignorowane). Bez ustawienia uzywany jest fallback `dirs::data_dir()`
/// (zachowanie dla testow / starych sciezek).
pub fn set_packages_base(base: PathBuf) {
    let _ = PACKAGES_BASE.set(base);
}

/// Korzen katalogu pakietow addonow (szablonow) na dysku: `<base>/packages/`.
/// Kazdy pakiet ma podkatalog `{package_id}/{version}/` z addon.wasm +
/// manifest.toml + migrations/. Wersjonowanie pozwala instancjom przypiac
/// konkretna wersje.
pub fn packages_root() -> PathBuf {
    PACKAGES_BASE
        .get()
        .cloned()
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("tentaflow-ai")
        })
        .join("packages")
}

/// Sciezka do konkretnej wersji pakietu: `packages/{package_id}/{version}/`.
pub fn package_dir(package_id: &str, version: &str) -> PathBuf {
    packages_root().join(package_id).join(version)
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_shape_guard() {
        // Real instance ids (base-<8hex>) — must be recognized as instances.
        assert!(looks_like_instance_id("eureka-a1b2c3d4"));
        assert!(looks_like_instance_id("company-lookup-0f0f0f0f"));
        // Bundled package ids — must NOT look like instances (never pruned-wrong).
        assert!(!looks_like_instance_id("eureka"));
        assert!(!looks_like_instance_id("company-lookup"));
        assert!(!looks_like_instance_id("deep-research"));
        assert!(!looks_like_instance_id("embeddings-chunker"));
        // Edge: 7 or 9 hex, non-hex tail, empty base.
        assert!(!looks_like_instance_id("x-a1b2c3d"));
        assert!(!looks_like_instance_id("x-a1b2c3d4e"));
        assert!(!looks_like_instance_id("x-zzzzzzzz"));
        assert!(!looks_like_instance_id("-a1b2c3d4"));
    }

    #[test]
    fn test_parse_new_format_manifest() {
        let manifest = r#"
[addon]
id = "teams"
name = "Microsoft Teams"
version = "0.1.0"
"#;
        let (id, version) = parse_addon_id_and_version(manifest).unwrap();
        assert_eq!(id, "teams");
        assert_eq!(version, "0.1.0");
    }

    #[test]
    fn test_parse_old_format_manifest() {
        let manifest = r#"
addon_id = "old-addon"
version = "1.2.3"
display_name = "Old Addon"
"#;
        let (id, version) = parse_addon_id_and_version(manifest).unwrap();
        assert_eq!(id, "old-addon");
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn test_parse_invalid_manifest_fails() {
        let manifest = "[addon]\nname = \"no-id\"";
        assert!(parse_addon_id_and_version(manifest).is_err());
    }

    #[test]
    fn test_bundle_hash_changes_when_manifest_changes() {
        let addon_a = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[],
            flows: &[],
        };
        let addon_b = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.1\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[],
            flows: &[],
        };

        assert_ne!(compute_bundle_hash(&addon_a), compute_bundle_hash(&addon_b));
    }

    #[test]
    fn test_bundle_hash_changes_when_wasm_changes() {
        let addon_a = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[],
            flows: &[],
        };
        let addon_b = BundledAddon {
            name: "outlook",
            wasm_bytes: &[1, 2, 4],
            manifest_toml: "[addon]\nid=\"outlook\"\nversion=\"0.1.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[],
            flows: &[],
        };

        assert_ne!(compute_bundle_hash(&addon_a), compute_bundle_hash(&addon_b));
    }

    #[test]
    fn test_bundle_hash_changes_when_migration_changes() {
        let addon_a = BundledAddon {
            name: "eureka",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"eureka\"\nversion=\"1.0.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[("001_init.sql", "CREATE TABLE eureka_entries (id INTEGER);")],
            flows: &[],
        };
        let addon_b = BundledAddon {
            name: "eureka",
            wasm_bytes: &[1, 2, 3],
            manifest_toml: "[addon]\nid=\"eureka\"\nversion=\"1.0.0\"\n",
            skill_md: "",
            description_md: "",
            blocks_json: "",
            migrations: &[(
                "001_init.sql",
                "CREATE TABLE eureka_entries (id INTEGER PRIMARY KEY);",
            )],
            flows: &[],
        };

        assert_ne!(compute_bundle_hash(&addon_a), compute_bundle_hash(&addon_b));
    }

    #[test]
    fn test_eureka_bundle_contains_sql_migrations() {
        let eureka = BUNDLED_ADDONS
            .iter()
            .find(|addon| addon.name == "eureka")
            .expect("eureka bundled addon");

        assert!(eureka
            .migrations
            .iter()
            .any(|(name, sql)| *name == "001_init.sql" && sql.contains("eureka_sync_state")));
        assert!(eureka
            .migrations
            .iter()
            .any(|(name, sql)| *name == "002_fetch_status.sql"
                && sql.contains("eureka_fetch_status")));
    }

    #[test]
    fn test_bundled_addons_constant_exists() {
        // Sprawdz ze stala BUNDLED_ADDONS jest dostepna
        let _ = BUNDLED_ADDONS.len();
    }

    /// Every bundled manifest parses cleanly in the canonical format and
    /// declares at least one permission with a valid risk level and non-empty
    /// display name. Guards against manifests drifting from the format.
    #[test]
    fn bundled_manifests_use_canonical_format() {
        use crate::addon::lifecycle::parse_manifest_toml;

        const VALID_RISK: &[&str] = &["low", "medium", "high", "critical"];

        assert!(!BUNDLED_ADDONS.is_empty(), "no bundled addons to validate");

        for addon in BUNDLED_ADDONS {
            let manifest = parse_manifest_toml(addon.manifest_toml)
                .unwrap_or_else(|e| panic!("manifest parse failed for '{}': {}", addon.name, e));

            assert!(
                !manifest.declared_permissions.is_empty(),
                "addon '{}' declares no permissions",
                manifest.addon_id
            );

            for perm in &manifest.declared_permissions {
                assert!(
                    !perm.id.is_empty(),
                    "addon '{}' has empty permission id",
                    manifest.addon_id
                );
                assert!(
                    !perm.display_name.is_empty(),
                    "addon '{}' permission '{}' has empty display_name",
                    manifest.addon_id,
                    perm.id
                );
                assert!(
                    VALID_RISK.contains(&perm.risk.as_str()),
                    "addon '{}' permission '{}' has invalid risk '{}'",
                    manifest.addon_id,
                    perm.id,
                    perm.risk
                );
            }
        }
    }
}
