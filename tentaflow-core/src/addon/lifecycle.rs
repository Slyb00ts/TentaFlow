// =============================================================================
// Plik: addon/lifecycle.rs
// Opis: Cykl zycia addonu — instalacja, deinstalacja, upgrade. Parsowanie
//       manifest.toml, walidacja, rejestracja w DB, zarzadzanie plikami WASM.
// =============================================================================

use std::path::Path;

use anyhow::{bail, Result};
use tracing::info;

use super::{
    AddonDeclaredPermission, AddonManifest, AddonOAuthProviderSection, AddonVisibilitySection,
    DisambiguationRule, ManifestNetworkRule, ManifestTool, ManifestToolParameter,
    ResourceRequirements,
};
use crate::db::DbPool;

// =============================================================================
// install — instalacja addonu
// =============================================================================

/// Instaluje addon z podanego katalogu.
///
/// Kroki:
/// 1. Odczytaj manifest.toml
/// 2. Waliduj manifest (wymagane pola, poprawnosc)
/// 3. Odczytaj plik WASM (walidacja istnienia + rozmiar do logowania)
/// 4. Zarejestruj addon w DB (tabela addons — manifest_json zawiera pelny manifest)
/// 5. Ustaw domyslne limity zasobow (addon_resource_limits)
pub fn install(addon_dir: &Path, db: &DbPool) -> Result<AddonManifest> {
    // 1. Odczytaj manifest.toml
    let manifest_path = addon_dir.join("manifest.toml");
    if !manifest_path.exists() {
        bail!("Brak pliku manifest.toml w {:?}", addon_dir);
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac manifest.toml: {e}"))?;

    let manifest = parse_manifest_toml(&manifest_content)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie sparsowac manifest.toml: {e}"))?;

    // 2. Walidacja
    validate_manifest(&manifest)?;

    // Sprawdzenie kompatybilnosci SDK addona z rdzeniem (F1a §6.2.Y).
    // None → kompatybilny (addon nie deklaruje wymagan); Some(req) → musi
    // matchowac CORE_SDK_VERSION.
    if let Err(e) = crate::addon::sdk_version::check_compatibility(manifest.sdk_version.as_deref()) {
        bail!("Addon '{}': {}", manifest.addon_id, e);
    }

    // 3. Odczytaj plik WASM
    let wasm_path = addon_dir.join(&manifest.wasm_file);

    // CR-010: Ochrona przed path traversal — sprawdz czy sciezka nie wychodzi poza katalog addonu
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = addon_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!(
                    "Path traversal wykryty w wasm_file: {:?}",
                    manifest.wasm_file
                );
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    let wasm_bytes = std::fs::read(&wasm_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac pliku WASM: {e}"))?;

    let platforms_json =
        serde_json::to_string(&manifest.platforms).unwrap_or_else(|_| "[\"all\"]".to_string());

    let wasm_size = wasm_bytes.len() as i64;

    // 5-9. Zarejestruj w DB (w jednej transakcji)
    let conn = db.lock().unwrap();

    conn.execute("BEGIN TRANSACTION", [])?;

    // Sprawdz czy addon juz istnieje
    let existing: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![&manifest.addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if existing {
        conn.execute("ROLLBACK", [])?;
        bail!(
            "Addon '{}' jest juz zainstalowany. Uzyj upgrade() zamiast install()",
            manifest.addon_id
        );
    }

    // Odczytaj SKILL.md z katalogu addonu (jesli istnieje)
    let skill_md = std::fs::read_to_string(addon_dir.join("SKILL.md")).ok();

    let keywords_json =
        serde_json::to_string(&manifest.keywords).unwrap_or_else(|_| "[]".to_string());

    let category = manifest.category.as_deref().unwrap_or("");

    let disambiguation_json =
        serde_json::to_string(&manifest.disambiguation).unwrap_or_else(|_| "[]".to_string());

    let icon = manifest.icon.as_deref().unwrap_or("");
    let runtime = manifest.runtime.as_deref().unwrap_or("wasmtime");
    let license = manifest.license.as_deref().unwrap_or("");
    let show_in_catalog = manifest.show_in_catalog.unwrap_or(true) as i64;

    // 5. Tabela addons — schemat z migracji 14 + 25 + 26 + 43 + 44
    // (skill_md, keywords_json, category, disambiguation_json, icon, runtime,
    //  wasm_size_bytes, license, show_in_catalog)
    conn.execute(
        "INSERT INTO addons (addon_id, name, version, description, author, platforms, manifest_json, is_enabled, is_system, skill_md, keywords_json, category, disambiguation_json, icon, runtime, wasm_size_bytes, license, show_in_catalog) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            &manifest.addon_id,
            &manifest.display_name,
            &manifest.version,
            &manifest.description.as_deref().unwrap_or(""),
            &manifest.author.as_deref().unwrap_or(""),
            &platforms_json,
            &manifest_content,
            &skill_md,
            &keywords_json,
            category,
            &disambiguation_json,
            icon,
            runtime,
            wasm_size,
            license,
            show_in_catalog,
        ],
    ).map_err(|e| anyhow::anyhow!("Nie udalo sie zarejestrowac addonu w DB: {e}"))?;

    // Uprawnienia, narzedzia i limity sa przechowywane w manifest_json
    // (tabela addons.manifest_json zawiera pelny manifest)

    // Limity zasobow — jesli manifest deklaruje [resources], uzyj ich; inaczej domyslne (0 = bez limitu)
    if let Some(ref res) = manifest.resources {
        conn.execute(
            "INSERT OR REPLACE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min, fuel_limit) \
             VALUES (?1, 0, 0, ?2, 1, 0, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                &manifest.addon_id,
                res.memory_mb.unwrap_or(0) as i64,
                res.storage_total_mb.unwrap_or(0) as i64,
                res.http_requests_per_minute.unwrap_or(0) as i64,
                res.llm_tokens_per_minute.unwrap_or(0) as i64,
                res.fuel_limit.unwrap_or(0) as i64,
            ],
        ).ok();
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES (?1, 0, 0, 0, 1, 0, 0, 0, 0)",
            rusqlite::params![&manifest.addon_id],
        )
        .ok();
    }

    // Zapisz reguly sieciowe z manifestu (TCP/UDP + HTTP domains)
    // Reguly required=true sa domyslnie approved (addon jawnie ich potrzebuje)
    for rule in &manifest.network_rules {
        conn.execute(
            "INSERT OR IGNORE INTO addon_network_rules \
             (addon_id, rule_id, protocol, host, port, description, required, approved) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &manifest.addon_id,
                &rule.id,
                &rule.protocol,
                &rule.host,
                rule.port,
                rule.description.as_deref().unwrap_or(""),
                rule.required as i32,
                if rule.required { 1 } else { 0 }
            ],
        )
        .ok();
    }

    conn.execute("COMMIT", [])?;
    drop(conn);

    // Synchronizacja metadanych z manifestu (permission catalog, oauth providers, visibility)
    sync_manifest_metadata(db, &manifest)?;

    // F1a §6.5 M1.W4: jezeli addon deklaruje [storage] sql=true — utworz
    // per-addon SQLite (przez fs_sandbox::addon_data_dir) i zaaplikuj migracje.
    // Bez deklaracji storage.sql nic sie nie dzieje — backward compat z istniejacymi
    // addonami (test-app, teams-bot).
    if matches!(manifest.storage.as_ref(), Some(s) if s.sql) {
        apply_addon_sql_migrations(&manifest, addon_dir, db)?;
    }

    info!(
        "Addon '{}' v{} installed ({} WASM bytes, {} permissions, {} tools, {} network rules)",
        manifest.addon_id,
        manifest.version,
        wasm_size,
        manifest.declared_permissions.len(),
        manifest.tools.len(),
        manifest.network_rules.len()
    );

    Ok(manifest)
}

/// Otwiera per-addon SQLite i aplikuje migracje z `<bundle>/<migrations_dir>/`.
/// Wywolywane tylko gdy `manifest.storage.sql == true`. Migration fail =
/// install fail z rollbackiem: czyscimy zarejestrowanego addona z core DB
/// oraz purgujemy pool, zeby kolejna proba install nie kolidowala.
fn apply_addon_sql_migrations(
    manifest: &AddonManifest,
    addon_dir: &Path,
    db: &DbPool,
) -> Result<()> {
    let storage = manifest.storage.as_ref().expect("checked by caller");
    let migrations_dir = storage.migrations_dir.as_str();

    if storage.encryption == "at-rest" {
        // F1a: deklaracja akceptowana, ale SQLCipher integracja przyjdzie w F8.
        tracing::warn!(
            "addon '{}': [storage].encryption='at-rest' — F1a nie wymusza szyfrowania (planowane F8 SQLCipher)",
            manifest.addon_id
        );
    }

    match crate::addon::migrations::apply_migrations(
        &manifest.addon_id,
        &manifest.version,
        migrations_dir,
        addon_dir,
        db,
    ) {
        Ok(n) => {
            info!(
                "addon '{}': SQL storage gotowy ({} migracji zaaplikowanych w tej sesji)",
                manifest.addon_id, n
            );
            Ok(())
        }
        Err(e) => {
            // Rollback rejestracji addonu — usuwamy go z DB i zamykamy pool,
            // zeby kolejny install_addon nie trafil na "addon juz istnieje".
            tracing::error!(
                "addon '{}': migracje SQL FAILED ({}) — rollback install",
                manifest.addon_id,
                e.as_i32()
            );
            crate::addon::storage_sql::close_addon_db(&manifest.addon_id);
            // Usun z DB (best-effort, install i tak juz failuje).
            let _ = uninstall(&manifest.addon_id, db);
            bail!(
                "addon '{}': blad migracji SQL (kod {})",
                manifest.addon_id,
                e.as_i32()
            );
        }
    }
}

// =============================================================================
// Synchronizacja katalogu uprawnien, providerow OAuth i widocznosci z manifestu
// =============================================================================

/// Synchronizuje wpisy pomocnicze po install/upgrade addona:
/// - permission_catalog (upsert + diff delete)
/// - oauth_providers_decl (upsert per wpis)
/// - visibility (admin_only + default_groups)
pub fn sync_manifest_metadata(db: &crate::db::DbPool, manifest: &AddonManifest) -> Result<()> {
    use crate::db::repository;

    // 1. Permission catalog — zrodlem prawdy sa declared_permissions
    let addon_id = &manifest.addon_id;
    let mut keep_ids: Vec<String> = Vec::with_capacity(manifest.declared_permissions.len());
    for (idx, perm) in manifest.declared_permissions.iter().enumerate() {
        if perm.id.is_empty() {
            continue;
        }
        let entry = repository::DbAddonPermissionCatalogEntry {
            addon_id: addon_id.clone(),
            permission_id: perm.id.clone(),
            display_name: if perm.display_name.is_empty() {
                perm.id.clone()
            } else {
                perm.display_name.clone()
            },
            description: perm.description.clone(),
            risk: if perm.risk.is_empty() {
                "low".to_string()
            } else {
                perm.risk.clone()
            },
            sort_order: idx as i32,
        };
        repository::upsert_permission_catalog(db, &entry)?;
        keep_ids.push(perm.id.clone());
    }
    repository::delete_permission_catalog_missing(db, addon_id, &keep_ids)?;

    // 2. OAuth providers — upsert deklaracji
    for prov in &manifest.oauth_provider {
        if prov.id.is_empty() {
            continue;
        }
        let decl = repository::DbAddonOAuthProviderDecl {
            addon_id: addon_id.clone(),
            provider_id: prov.id.clone(),
            display_name: prov.display_name.clone(),
            authorize_url: prov.authorize_url.clone(),
            token_url: prov.token_url.clone(),
            revoke_url: prov.revoke_url.clone(),
            scopes: prov.scopes.join(" "),
            mode: prov.mode.clone(),
            pkce: prov.pkce,
        };
        repository::upsert_oauth_providers_decl(db, &decl)?;
    }

    // 3. Widocznosc: admin_only + domyslne grupy
    if let Some(v) = &manifest.visibility {
        repository::set_addon_admin_only(db, addon_id, v.admin_only)?;
        for group_name in &v.default_groups {
            if let Some(gid) = repository::get_group_id_by_name(db, group_name)? {
                repository::set_addon_visibility(db, addon_id, gid, true, None)?;
            }
        }
    }

    Ok(())
}

// =============================================================================
// uninstall — deinstalacja addonu
// =============================================================================

/// Odinstalowuje addon — usuwa z DB i czysci storage.
///
/// Kroki:
/// 1. Sprawdz czy addon istnieje
/// 2. Usun z tabel powiazanych (addon_permissions, addon_secrets, addon_resource_limits, addon_config)
/// 3. Usun z addons
pub fn uninstall(addon_id: &str, db: &DbPool) -> Result<()> {
    let conn = db.lock().unwrap();

    // Sprawdz czy addon istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !exists {
        bail!("Addon '{}' nie jest zainstalowany", addon_id);
    }

    conn.execute("BEGIN TRANSACTION", [])?;

    // Usun w kolejnosci (foreign keys CASCADE powinno to zalatwic,
    // ale robimy explicite dla pewnosci)
    // VULN-039: Dodano addon_storage — pełne czyszczenie danych przy deinstalacji
    let tables = [
        "addon_storage",
        "addon_permissions",
        "addon_secrets",
        "addon_resource_limits",
        "addon_config",
        "addon_network_rules",
        // Bez tego ponowny install innej wersji o tej samej nazwie pliku
        // migracji ale roznym hashu trafia na "hash mismatch" guard.
        "addon_migrations_applied",
    ];

    for table in &tables {
        conn.execute(
            &format!("DELETE FROM {} WHERE addon_id = ?1", table),
            rusqlite::params![addon_id],
        )
        .ok(); // Ignoruj bledy — tabela moze nie istniec jeszcze
    }

    // Glowna tabela addons
    conn.execute(
        "DELETE FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
    )
    .map_err(|e| anyhow::anyhow!("Nie udalo sie usunac addonu z DB: {e}"))?;

    conn.execute("COMMIT", [])?;

    // F1a §6.5 M1.W4: zamknij per-addon SQLite pool. Plik data.db pozostaje
    // na dysku (user moze chciec backup) — czyszczenie tylko manualne.
    crate::addon::storage_sql::close_addon_db(addon_id);

    info!("Addon '{}' odinstalowany", addon_id);

    Ok(())
}

// =============================================================================
// upgrade — aktualizacja addonu
// =============================================================================

/// Aktualizuje addon do nowej wersji.
///
/// Kroki:
/// 1. Odczytaj nowy manifest i waliduj
/// 2. Sprawdz plik WASM (walidacja istnienia + path traversal)
/// 3. Zaktualizuj metadane w tabeli addons (manifest_json zawiera pelny manifest)
/// 4. Ustaw domyslne limity zasobow jesli jeszcze nie istnieja (INSERT OR IGNORE)
pub fn upgrade(addon_id: &str, new_dir: &Path, db: &DbPool) -> Result<()> {
    // Odczytaj nowy manifest
    let manifest_path = new_dir.join("manifest.toml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac nowego manifest.toml: {e}"))?;

    let new_manifest = parse_manifest_toml(&manifest_content)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie sparsowac nowego manifest.toml: {e}"))?;

    validate_manifest(&new_manifest)?;

    if new_manifest.addon_id != addon_id {
        bail!(
            "addon_id w manifescie ('{}') nie zgadza sie z '{}' ",
            new_manifest.addon_id,
            addon_id
        );
    }

    // Odczytaj nowy WASM
    let wasm_path = new_dir.join(&new_manifest.wasm_file);

    // CR-010: Ochrona przed path traversal
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = new_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!(
                    "Path traversal wykryty w wasm_file: {:?}",
                    new_manifest.wasm_file
                );
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    // Size is captured from the WASM file on disk; metadata() avoids reading
    // the module contents twice (install() does a full read for validation,
    // upgrade() trusts the lifecycle path traversal check above).
    let wasm_size = std::fs::metadata(&wasm_path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    let platforms_json = serde_json::to_string(&new_manifest.platforms)?;

    let icon = new_manifest.icon.as_deref().unwrap_or("");
    let runtime = new_manifest.runtime.as_deref().unwrap_or("wasmtime");
    let category = new_manifest.category.as_deref().unwrap_or("");
    let license = new_manifest.license.as_deref().unwrap_or("");
    let show_in_catalog = new_manifest.show_in_catalog.unwrap_or(true) as i64;

    let conn = db.lock().unwrap();
    conn.execute("BEGIN TRANSACTION", [])?;

    // Pobierz stara wersje
    let old_version: String = conn
        .query_row(
            "SELECT version FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .map_err(|e| anyhow::anyhow!("Addon nie znaleziony: {e}"))?;

    info!(
        "Upgrade addonu '{}': {} -> {}",
        addon_id, old_version, new_manifest.version
    );

    // Zaktualizuj metadane addonu (w tym UI metadata z migracji 43 + 44).
    conn.execute(
        "UPDATE addons SET version = ?1, name = ?2, description = ?3, author = ?4, \
         manifest_json = ?5, platforms = ?6, category = ?7, icon = ?8, runtime = ?9, \
         wasm_size_bytes = ?10, license = ?11, show_in_catalog = ?12, \
         updated_at = datetime('now') \
         WHERE addon_id = ?13",
        rusqlite::params![
            &new_manifest.version,
            &new_manifest.display_name,
            &new_manifest.description.as_deref().unwrap_or(""),
            &new_manifest.author.as_deref().unwrap_or(""),
            &manifest_content,
            &platforms_json,
            category,
            icon,
            runtime,
            wasm_size,
            license,
            show_in_catalog,
            addon_id,
        ],
    )?;

    // Limity zasobow — jesli nowy manifest deklaruje [resources], zaktualizuj; inaczej zachowaj istniejace
    if let Some(ref res) = new_manifest.resources {
        conn.execute(
            "INSERT OR REPLACE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min, fuel_limit) \
             VALUES (?1, 0, 0, ?2, 1, 0, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                addon_id,
                res.memory_mb.unwrap_or(0) as i64,
                res.storage_total_mb.unwrap_or(0) as i64,
                res.http_requests_per_minute.unwrap_or(0) as i64,
                res.llm_tokens_per_minute.unwrap_or(0) as i64,
                res.fuel_limit.unwrap_or(0) as i64,
            ],
        ).ok();
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES (?1, 0, 0, 0, 1, 0, 0, 0, 0)",
            rusqlite::params![addon_id],
        )
        .ok();
    }

    // Synchronizacja regul sieciowych:
    // - Zachowaj approved status istniejacych regul (juz zatwierdzonych przez admina)
    // - Dodaj nowe reguly z approved=0 (wymagaja zatwierdzenia)
    // - Usun reguly ktore nie istnieja w nowym manifescie
    sync_network_rules(&conn, addon_id, &new_manifest.network_rules)?;

    conn.execute("COMMIT", [])?;
    drop(conn);

    // Synchronizacja metadanych z manifestu (permission catalog, oauth providers, visibility)
    sync_manifest_metadata(db, &new_manifest)?;

    info!(
        "Addon '{}' zaktualizowany do v{}",
        addon_id, new_manifest.version
    );

    Ok(())
}

// =============================================================================
// Walidacja manifestu
// =============================================================================

/// Valid risk levels for declared permissions.
const VALID_RISK: &[&str] = &["low", "medium", "high", "critical"];

/// Legacy manifest sections that are explicitly rejected to prevent silent
/// acceptance of mixed formats. Addons must be rewritten to the canonical
/// format (see SCHEMA in repository docs).
const LEGACY_SECTIONS: &[&str] = &[
    "permissions",       // old [permissions] with required/optional category lists
    "addon_permissions", // old [[addon_permissions]] array
    "network_rules",     // old [[network_rules]] (singular in new format)
    "tools",             // old [tools.name] nested subtables
];

/// Validates a parsed manifest — required fields, permission risk levels,
/// unique network rule ids, non-empty tool fields.
fn validate_manifest(manifest: &AddonManifest) -> Result<()> {
    if manifest.addon_id.is_empty() {
        bail!("addon.id is empty");
    }
    if manifest.addon_id.len() > 128 {
        bail!("addon.id too long (max 128 chars)");
    }
    if !manifest
        .addon_id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        bail!("addon.id contains disallowed characters (allowed: a-z, 0-9, '.', '-', '_')");
    }
    if manifest.version.is_empty() {
        bail!("addon.version is empty");
    }
    if manifest.display_name.is_empty() {
        bail!("addon.name is empty");
    }
    if manifest.wasm_file.is_empty() {
        bail!("addon.wasm_file is empty");
    }

    let mut perm_ids = std::collections::HashSet::new();
    for perm in &manifest.declared_permissions {
        if perm.id.is_empty() {
            bail!("permission.id is empty");
        }
        if !perm_ids.insert(&perm.id) {
            bail!("duplicate permission.id: '{}'", perm.id);
        }
        if perm.display_name.is_empty() {
            bail!("permission '{}': display_name is empty", perm.id);
        }
        if !VALID_RISK.contains(&perm.risk.as_str()) {
            bail!(
                "permission '{}': risk must be low|medium|high|critical (got '{}')",
                perm.id,
                perm.risk
            );
        }
    }

    for tool in &manifest.tools {
        if tool.name.is_empty() {
            bail!("tool.id is empty");
        }
        if tool.description.is_empty() {
            bail!("tool '{}': description is empty", tool.name);
        }
    }

    let mut rule_ids = std::collections::HashSet::new();
    for rule in &manifest.network_rules {
        if rule.id.is_empty() {
            bail!("network_rule.id is empty");
        }
        if !rule_ids.insert(&rule.id) {
            bail!("duplicate network_rule.id: '{}'", rule.id);
        }
        if rule.host.is_empty() {
            bail!("network_rule '{}': host is empty", rule.id);
        }
        if rule.port == 0 {
            bail!("network_rule '{}': port must be 1-65535", rule.id);
        }
        if rule.protocol != "tcp" && rule.protocol != "udp" {
            bail!(
                "network_rule '{}': protocol must be 'tcp' or 'udp'",
                rule.id
            );
        }
    }

    for prov in &manifest.oauth_provider {
        if prov.id.is_empty() {
            bail!("oauth_provider.id is empty");
        }
        if prov.authorize_url.is_empty() || prov.token_url.is_empty() {
            bail!(
                "oauth_provider '{}': authorize_url and token_url must be set",
                prov.id
            );
        }
        if !matches!(prov.mode.as_str(), "global" | "individual" | "none") {
            bail!(
                "oauth_provider '{}': mode must be global|individual|none",
                prov.id
            );
        }
    }

    Ok(())
}

/// Parses the canonical manifest format:
/// - `[addon]` section holding id/name/version/wasm_file/... (required).
/// - `[[permission]]` array of declared granular permissions.
/// - `[[tool]]` array with optional nested `[[tool.parameter]]` items.
/// - `[[oauth_provider]]`, `[[network_rule]]` arrays.
/// - Sections `[visibility]`, `[resources]`, `[lifecycle]`, `[config.schema]`.
///
/// Legacy sections (`[permissions]`, `[[addon_permissions]]`, singular `[tools.X]`,
/// `[[network_rules]]`) are rejected with a clear error — addons must migrate to
/// the canonical format instead of relying on backward-compat shims.
pub fn parse_manifest_toml(content: &str) -> Result<AddonManifest> {
    let parsed: toml::Value =
        toml::from_str(content).map_err(|e| anyhow::anyhow!("invalid TOML: {e}"))?;

    let top = parsed
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("manifest root must be a TOML table"))?;

    for legacy in LEGACY_SECTIONS {
        if top.contains_key(*legacy) {
            bail!(
                "manifest uses legacy section '[{}]' — migrate to the canonical format \
                 ([[permission]], [[tool]], [[network_rule]])",
                legacy
            );
        }
    }

    let addon = top
        .get("addon")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("missing [addon] section"))?;

    let addon_id = addon
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing addon.id"))?
        .to_string();
    let version = addon
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing addon.version"))?
        .to_string();
    let display_name = addon
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&addon_id)
        .to_string();
    let description = addon
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let author = addon
        .get("author")
        .and_then(|v| v.as_str())
        .map(String::from);
    let wasm_file = addon
        .get("wasm_file")
        .and_then(|v| v.as_str())
        .unwrap_or("addon.wasm")
        .to_string();
    let platforms = addon
        .get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let keywords = addon
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let category = addon
        .get("category")
        .and_then(|v| v.as_str())
        .map(String::from);
    let icon = addon.get("icon").and_then(|v| v.as_str()).map(String::from);
    let runtime = addon
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(String::from);
    let license = addon
        .get("license")
        .and_then(|v| v.as_str())
        .map(String::from);

    let declared_permissions = top
        .get("permission")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| AddonDeclaredPermission {
                    id: p
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    display_name: p
                        .get("display_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    description: p
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    risk: p
                        .get("risk")
                        .and_then(|v| v.as_str())
                        .unwrap_or("low")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let oauth_provider = top
        .get("oauth_provider")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(|v| v.as_str())?.to_string();
                    Some(AddonOAuthProviderSection {
                        id,
                        display_name: p
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        authorize_url: p
                            .get("authorize_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        token_url: p
                            .get("token_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        revoke_url: p
                            .get("revoke_url")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        scopes: p
                            .get("scopes")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        mode: p
                            .get("mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or("individual")
                            .to_string(),
                        pkce: p.get("pkce").and_then(|v| v.as_bool()).unwrap_or(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let network_rules = top
        .get("network_rule")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| ManifestNetworkRule {
                    id: r
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    protocol: r
                        .get("protocol")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tcp")
                        .to_string(),
                    host: r
                        .get("host")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    port: r.get("port").and_then(|v| v.as_integer()).unwrap_or(443) as u16,
                    description: r
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    required: r.get("required").and_then(|v| v.as_bool()).unwrap_or(true),
                })
                .collect()
        })
        .unwrap_or_default();

    let tools = top
        .get("tool")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    let id = t
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let keywords_t = t
                        .get("keywords")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let parameters: Vec<ManifestToolParameter> = t
                        .get("parameter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|p| ManifestToolParameter {
                                    name: p
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    param_type: p
                                        .get("param_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("string")
                                        .to_string(),
                                    description: p
                                        .get("description")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    required: p
                                        .get("required")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ManifestTool {
                        name: id,
                        description,
                        parameters_schema: build_parameters_schema(&parameters),
                        return_schema: None,
                        keywords: keywords_t,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let visibility = top.get("visibility").map(|v| AddonVisibilitySection {
        admin_only: v
            .get("admin_only")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        default_groups: v
            .get("default_groups")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        show_in_catalog: v.get("show_in_catalog").and_then(|x| x.as_bool()),
    });

    // `[visibility].show_in_catalog` controls the top-level flag stored in the
    // addons table; falls back to `[addon].show_in_catalog` if someone puts it there.
    let show_in_catalog = visibility
        .as_ref()
        .and_then(|v| v.show_in_catalog)
        .or_else(|| addon.get("show_in_catalog").and_then(|v| v.as_bool()));

    let disambiguation = addon
        .get("disambiguation")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let trigger = item
                        .get("trigger")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let prefer = item.get("prefer").and_then(|v| v.as_str())?.to_string();
                    let over = item.get("over").and_then(|v| v.as_str())?.to_string();
                    let when = item
                        .get("when")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(DisambiguationRule {
                        trigger,
                        prefer,
                        over,
                        when,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let service = top
        .get("service")
        .and_then(|v| v.as_table())
        .map(|svc| crate::addon::AddonServiceSection {
            enabled: svc.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            tick_interval_ms: svc
                .get("tick_interval_ms")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
            tick_fuel_budget: svc
                .get("tick_fuel_budget")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
            tick_timeout_ms: svc
                .get("tick_timeout_ms")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
        });

    let application = top
        .get("application")
        .and_then(|v| v.as_table())
        .and_then(|app| {
            let entry_panel = app.get("entry_panel").and_then(|v| v.as_str())?.to_string();
            let title = app
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&entry_panel)
                .to_string();
            Some(crate::addon::AddonApplicationSection {
                entry_panel,
                title,
                icon: app.get("icon").and_then(|v| v.as_str()).map(String::from),
                sort_order: app
                    .get("sort_order")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as i32),
            })
        });

    let sdk_version = addon
        .get("sdk_version")
        .and_then(|v| v.as_str())
        .map(String::from);

    let storage = parse_storage_section(top.get("storage"))?;
    let aliases = parse_aliases(top.get("alias"))?;
    let gates = parse_gates(top.get("gate"))?;
    let vector_namespaces = parse_vector_namespaces(top.get("vector_namespace"))?;
    let flow_templates = parse_flow_templates(top.get("flow_template"))?;
    let ui_components = parse_ui_components(top.get("ui_component"))?;
    let gpu = parse_gpu_section(top.get("gpu"));
    let uses_aliases = parse_uses_aliases(top.get("uses_alias"))?;
    let uses_models = parse_uses_models(top.get("uses_model"))?;

    crate::addon::manifest::validate_manifest_extensions(
        storage.as_ref(),
        &aliases,
        &gates,
        &vector_namespaces,
        &flow_templates,
        &ui_components,
        sdk_version.as_deref(),
        &uses_aliases,
        &uses_models,
    )?;

    let resources = top.get("resources").map(|res| ResourceRequirements {
        storage_total_mb: res
            .get("storage_total_mb")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("storage_mb").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        storage_value_mb: res
            .get("storage_value_mb")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64),
        llm_tokens_per_minute: res
            .get("llm_tokens_per_minute")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("llm_tokens_per_min").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        http_requests_per_minute: res
            .get("http_requests_per_minute")
            .and_then(|v| v.as_integer())
            .or_else(|| {
                res.get("http_requests_per_min")
                    .and_then(|v| v.as_integer())
            })
            .map(|v| v as u64),
        memory_mb: res
            .get("memory_mb")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("ram_mb").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        fuel_limit: res
            .get("fuel_limit")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64),
    });

    Ok(AddonManifest {
        addon_id,
        version,
        display_name,
        description,
        author,
        platforms,
        wasm_file,
        keywords,
        category,
        icon,
        runtime,
        tools,
        declared_permissions,
        network_rules,
        disambiguation,
        resources,
        visibility,
        oauth_provider,
        license,
        show_in_catalog,
        service,
        application,
        storage,
        aliases,
        gates,
        vector_namespaces,
        flow_templates,
        ui_components,
        gpu,
        sdk_version,
        uses_aliases,
        uses_models,
    })
}

// Parsery sekcji rozszerzonych (F1a). Trzymamy je w lifecycle.rs zeby utrzymac
// jeden punkt wejscia parsowania (parse_manifest_toml) i nie dublowac iteracji
// po toml::Value w manifest.rs.

fn parse_storage_section(
    val: Option<&toml::Value>,
) -> Result<Option<crate::addon::manifest::StorageConfig>> {
    let Some(v) = val else {
        return Ok(None);
    };
    let tbl = v
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("[storage] must be a table"))?;
    let cfg = crate::addon::manifest::StorageConfig {
        kv: tbl.get("kv").and_then(|v| v.as_bool()).unwrap_or(true),
        sql: tbl.get("sql").and_then(|v| v.as_bool()).unwrap_or(false),
        sql_backends: tbl
            .get("sql_backends")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        sql_dialect: tbl
            .get("sql_dialect")
            .and_then(|v| v.as_str())
            .unwrap_or("ansi")
            .to_string(),
        migrations_dir: tbl
            .get("migrations_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("migrations")
            .to_string(),
        encryption: tbl
            .get("encryption")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string(),
    };
    Ok(Some(cfg))
}

fn parse_aliases(val: Option<&toml::Value>) -> Result<Vec<crate::addon::manifest::AliasSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[alias]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let methods = item
            .get("methods")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let suggested_default = item
            .get("suggested_default")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let gate = item
            .get("gate")
            .and_then(|v| v.as_str())
            .map(String::from);
        let visibility = match item.get("visibility").and_then(|v| v.as_str()) {
            Some(s) => crate::addon::manifest::AliasVisibility::parse(s)?,
            None => crate::addon::manifest::AliasVisibility::Private,
        };
        let allowed_consumers = item
            .get("allowed_consumers")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push(crate::addon::manifest::AliasSpec {
            id,
            display_name,
            methods,
            suggested_default,
            gate,
            visibility,
            allowed_consumers,
        });
    }
    Ok(out)
}

fn parse_uses_aliases(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UsesAliasSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[uses_alias]][{idx}] missing 'id'"))?
            .to_string();
        let required = item.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::UsesAliasSpec { id, required, reason });
    }
    Ok(out)
}

fn parse_uses_models(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UsesModelSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[uses_model]][{idx}] missing 'id'"))?
            .to_string();
        let required = item.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::UsesModelSpec { id, required, reason });
    }
    Ok(out)
}

fn parse_gates(val: Option<&toml::Value>) -> Result<Vec<crate::addon::manifest::GateSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[gate]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let required_claims = item
            .get("required_claims")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(parse_claim_requirement)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        out.push(crate::addon::manifest::GateSpec {
            id,
            display_name,
            required_claims,
        });
    }
    Ok(out)
}

fn parse_claim_requirement(
    val: &toml::Value,
) -> Result<crate::addon::manifest::ClaimRequirement> {
    let claim_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("claim requirement missing 'type'"))?
        .to_string();
    Ok(crate::addon::manifest::ClaimRequirement {
        claim_type,
        subject: val.get("subject").and_then(|v| v.as_str()).map(String::from),
        scope: val.get("scope").and_then(|v| v.as_str()).map(String::from),
        status: val.get("status").and_then(|v| v.as_str()).map(String::from),
        value: val.get("value").and_then(|v| v.as_str()).map(String::from),
        oneof: val
            .get("oneof")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        valid: val.get("valid").and_then(|v| v.as_bool()),
        has_expiry: val.get("has_expiry").and_then(|v| v.as_bool()),
    })
}

fn parse_vector_namespaces(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::VectorNamespaceSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'name'"))?
            .to_string();
        let dimensions = item
            .get("dimensions")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| {
                anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'dimensions'")
            })? as u32;
        let distance = item
            .get("distance")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'distance'"))?
            .to_string();
        let data_class = item
            .get("data_class")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'data_class'"))?
            .to_string();
        let gate = item.get("gate").and_then(|v| v.as_str()).map(String::from);
        out.push(crate::addon::manifest::VectorNamespaceSpec {
            name,
            dimensions,
            distance,
            data_class,
            gate,
        });
    }
    Ok(out)
}

fn parse_flow_templates(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::FlowTemplateSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[flow_template]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let path = item
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[flow_template]][{idx}] missing 'path'"))?
            .to_string();
        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::FlowTemplateSpec {
            id,
            display_name,
            path,
            description,
        });
    }
    Ok(out)
}

fn parse_ui_components(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UiComponentSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let slot = item
            .get("slot")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'slot'"))?
            .to_string();
        let src = item
            .get("src")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'src'"))?
            .to_string();
        let signature = item
            .get("signature")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'signature'"))?
            .to_string();
        let risk = item
            .get("risk")
            .and_then(|v| v.as_str())
            .unwrap_or("low")
            .to_string();
        let host_permissions = item
            .get("host_permissions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push(crate::addon::manifest::UiComponentSpec {
            id,
            display_name,
            slot,
            src,
            signature,
            risk,
            host_permissions,
        });
    }
    Ok(out)
}

fn parse_gpu_section(val: Option<&toml::Value>) -> Option<crate::addon::manifest::GpuInfo> {
    let tbl = val?.as_table()?;
    Some(crate::addon::manifest::GpuInfo {
        recommended_vram_mb: tbl
            .get("recommended_vram_mb")
            .and_then(|v| v.as_integer())
            .map(|v| v as u32),
        notes: tbl.get("notes").and_then(|v| v.as_str()).map(String::from),
    })
}

/// Builds a JSON Schema `object` from `[[tool.parameter]]` entries. Keeps the
/// `parameters_schema` field shape that existing tool_dispatch/host code expects.
fn build_parameters_schema(params: &[ManifestToolParameter]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        if p.name.is_empty() {
            continue;
        }
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String(p.param_type.clone()),
        );
        if !p.description.is_empty() {
            prop.insert(
                "description".to_string(),
                serde_json::Value::String(p.description.clone()),
            );
        }
        properties.insert(p.name.clone(), serde_json::Value::Object(prop));
        if p.required {
            required.push(serde_json::Value::String(p.name.clone()));
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

// =============================================================================
// Synchronizacja regul sieciowych (upgrade)
// =============================================================================

/// Synchronizuje reguly sieciowe przy upgrade addonu.
///
/// Logika:
/// - Istniejace reguly (ten sam rule_id): zachowaj approved/approved_by/approved_at,
///   zaktualizuj host/port/protocol/description/required
/// - Nowe reguly (nie istnieja w DB): dodaj z approved=0
/// - Usuniete reguly (nie istnieja w nowym manifescie): usun z DB
fn sync_network_rules(
    conn: &rusqlite::Connection,
    addon_id: &str,
    new_rules: &[ManifestNetworkRule],
) -> Result<()> {
    // Pobierz istniejace rule_id z DB
    let mut stmt = conn.prepare("SELECT rule_id FROM addon_network_rules WHERE addon_id = ?1")?;
    let existing_ids: Vec<String> = stmt
        .query_map(rusqlite::params![addon_id], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let new_ids: Vec<&str> = new_rules.iter().map(|r| r.id.as_str()).collect();

    // Usun reguly ktore nie istnieja w nowym manifescie
    for old_id in &existing_ids {
        if !new_ids.contains(&old_id.as_str()) {
            conn.execute(
                "DELETE FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
                rusqlite::params![addon_id, old_id],
            )?;
            info!(
                "upgrade: usunieto regule sieciowa '{}' addonu '{}'",
                old_id, addon_id
            );
        }
    }

    // Upsert: zaktualizuj istniejace, dodaj nowe (approved=0)
    // VULN-042: Jesli host/port/protocol sie zmienil — reset approved=0
    for rule in new_rules {
        if existing_ids.contains(&rule.id) {
            // Sprawdz czy cel polaczenia sie zmienil (host, port, protocol)
            let (old_host, old_port, old_proto): (String, i64, String) = conn
                .query_row(
                    "SELECT host, port, protocol FROM addon_network_rules \
                 WHERE addon_id = ?1 AND rule_id = ?2",
                    rusqlite::params![addon_id, &rule.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap_or_default();

            let target_changed =
                old_host != rule.host || old_port != rule.port as i64 || old_proto != rule.protocol;

            if target_changed {
                // Cel polaczenia sie zmienil — wymagaj ponownego zatwierdzenia
                conn.execute(
                    "UPDATE addon_network_rules \
                     SET protocol = ?1, host = ?2, port = ?3, description = ?4, required = ?5, \
                         approved = 0, approved_by = NULL, approved_at = NULL \
                     WHERE addon_id = ?6 AND rule_id = ?7",
                    rusqlite::params![
                        &rule.protocol,
                        &rule.host,
                        rule.port,
                        rule.description.as_deref().unwrap_or(""),
                        rule.required as i32,
                        addon_id,
                        &rule.id,
                    ],
                )?;
                info!(
                    "upgrade: regula '{}' addonu '{}' — cel zmieniony ({}:{} -> {}:{}), reset approved",
                    rule.id, addon_id, old_host, old_port, rule.host, rule.port
                );
            } else {
                // Cel nie zmieniony — zachowaj approved status
                conn.execute(
                    "UPDATE addon_network_rules \
                     SET description = ?1, required = ?2 \
                     WHERE addon_id = ?3 AND rule_id = ?4",
                    rusqlite::params![
                        rule.description.as_deref().unwrap_or(""),
                        rule.required as i32,
                        addon_id,
                        &rule.id,
                    ],
                )?;
            }
        } else {
            // Nowa regula — wymaga zatwierdzenia admina
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                rusqlite::params![
                    addon_id,
                    &rule.id,
                    &rule.protocol,
                    &rule.host,
                    rule.port,
                    rule.description.as_deref().unwrap_or(""),
                    rule.required as i32,
                ],
            )?;
            info!(
                "upgrade: dodano nowa regule sieciowa '{}' addonu '{}' (wymaga zatwierdzenia)",
                rule.id, addon_id
            );
        }
    }

    Ok(())
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn minimal_wasm_bytes() -> Vec<u8> {
        // Minimal valid WASM module header: magic "\0asm" + version 1.
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    #[test]
    fn parses_service_section_with_tick_interval_and_fuel() {
        let toml = r#"
[addon]
id = "cam-watcher"
name = "Camera Watcher"
version = "0.1.0"
wasm_file = "addon.wasm"

[service]
enabled = true
tick_interval_ms = 500
tick_fuel_budget = 20000000
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let svc = m.service.expect("[service] sekcja wczytana");
        assert!(svc.enabled);
        assert_eq!(svc.tick_interval_ms, Some(500));
        assert_eq!(svc.tick_fuel_budget, Some(20_000_000));
    }

    #[test]
    fn missing_service_section_yields_none() {
        let toml = r#"
[addon]
id = "no-service"
name = "No Service"
version = "0.1.0"
wasm_file = "addon.wasm"
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        assert!(m.service.is_none());
    }

    #[test]
    fn service_section_defaults_enabled_true_when_omitted() {
        let toml = r#"
[addon]
id = "default-enabled"
name = "Default Enabled"
version = "0.1.0"
wasm_file = "addon.wasm"

[service]
tick_interval_ms = 1000
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let svc = m.service.expect("[service] sekcja wczytana");
        assert!(svc.enabled, "enabled default to true when section present");
        assert_eq!(svc.tick_interval_ms, Some(1000));
        assert!(svc.tick_fuel_budget.is_none());
    }

    #[test]
    fn test_lifecycle_install_persists_wasm_size_and_ui_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let addon_dir = tmp.path();

        let manifest = r#"
[addon]
id = "size-test"
name = "Size Test"
version = "0.1.0"
description = "lifecycle install size/icon/runtime round-trip"
author = "tests"
platforms = ["linux"]
wasm_file = "addon.wasm"
category = "communication"
icon = "i-meeting"
runtime = "wasmtime"
"#;
        std::fs::write(addon_dir.join("manifest.toml"), manifest).unwrap();

        let wasm = minimal_wasm_bytes();
        let mut f = std::fs::File::create(addon_dir.join("addon.wasm")).unwrap();
        f.write_all(&wasm).unwrap();
        drop(f);

        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init in-memory db");
        let installed = install(addon_dir, &db).expect("install should succeed");
        assert_eq!(installed.icon.as_deref(), Some("i-meeting"));
        assert_eq!(installed.runtime.as_deref(), Some("wasmtime"));
        assert_eq!(installed.category.as_deref(), Some("communication"));

        let row = crate::db::repository::get_addon(&db, "size-test")
            .unwrap()
            .expect("addon row present");
        assert_eq!(row.icon, "i-meeting");
        assert_eq!(row.runtime, "wasmtime");
        assert_eq!(row.category, "communication");
        assert_eq!(row.wasm_size_bytes, wasm.len() as i64);
    }
}
