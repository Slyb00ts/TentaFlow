// =============================================================================
// Plik: addon/lifecycle.rs
// Opis: Cykl zycia addonu — instalacja, deinstalacja, upgrade. Parsowanie
//       manifest.toml, walidacja, rejestracja w DB, zarzadzanie plikami WASM.
// =============================================================================

use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::Connection;
use tracing::info;

use super::{AddonManifest, AddonDeclaredPermission, DisambiguationRule, ManifestPermission, ManifestTool, ManifestNetworkRule, ResourceRequirements};
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

    // 3. Odczytaj plik WASM
    let wasm_path = addon_dir.join(&manifest.wasm_file);

    // CR-010: Ochrona przed path traversal — sprawdz czy sciezka nie wychodzi poza katalog addonu
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = addon_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!("Path traversal wykryty w wasm_file: {:?}", manifest.wasm_file);
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    let wasm_bytes = std::fs::read(&wasm_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac pliku WASM: {e}"))?;

    let platforms_json = serde_json::to_string(&manifest.platforms)
        .unwrap_or_else(|_| "[\"all\"]".to_string());

    let wasm_size = wasm_bytes.len() as i64;

    // 5-9. Zarejestruj w DB (w jednej transakcji)
    let conn = db.lock().unwrap();

    conn.execute("BEGIN TRANSACTION", [])?;

    // Sprawdz czy addon juz istnieje
    let existing: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
        rusqlite::params![&manifest.addon_id],
        |row| row.get(0),
    ).unwrap_or(false);

    if existing {
        conn.execute("ROLLBACK", [])?;
        bail!("Addon '{}' jest juz zainstalowany. Uzyj upgrade() zamiast install()", manifest.addon_id);
    }

    // Odczytaj SKILL.md z katalogu addonu (jesli istnieje)
    let skill_md = std::fs::read_to_string(addon_dir.join("SKILL.md")).ok();

    let keywords_json = serde_json::to_string(&manifest.keywords)
        .unwrap_or_else(|_| "[]".to_string());

    let category = manifest.category.as_deref().unwrap_or("");

    let disambiguation_json = serde_json::to_string(&manifest.disambiguation)
        .unwrap_or_else(|_| "[]".to_string());

    // 5. Tabela addons — schemat z migracji 14 + 25 + 26 (skill_md, keywords_json, category, disambiguation_json)
    conn.execute(
        "INSERT INTO addons (addon_id, name, version, description, author, platforms, manifest_json, is_enabled, is_system, skill_md, keywords_json, category, disambiguation_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, ?8, ?9, ?10, ?11)",
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
        ).ok();
    }

    // Zapisz reguly sieciowe z manifestu (TCP/UDP + HTTP domains)
    // Reguly required=true sa domyslnie approved (addon jawnie ich potrzebuje)
    for rule in &manifest.network_rules {
        conn.execute(
            "INSERT OR IGNORE INTO addon_network_rules \
             (addon_id, rule_id, protocol, host, port, description, required, approved) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &manifest.addon_id, &rule.id, &rule.protocol,
                &rule.host, rule.port,
                rule.description.as_deref().unwrap_or(""),
                rule.required as i32,
                if rule.required { 1 } else { 0 }
            ],
        ).ok();
    }

    conn.execute("COMMIT", [])?;

    info!(
        "Addon '{}' v{} zainstalowany ({} bajtow WASM, {} uprawnien, {} narzedzi, {} regul sieciowych)",
        manifest.addon_id, manifest.version, wasm_size,
        manifest.permissions.len(), manifest.tools.len(), manifest.network_rules.len()
    );

    Ok(manifest)
}

/// Uzupelnia reguly sieciowe HTTP domains dla juz zainstalowanych addonow.
/// Wywoływane przy starcie — addony zainstalowane przed ta zmiana nie maja
/// regul HTTP w addon_network_rules. Parsuje manifest_json (TOML) i dodaje brakujace.
pub fn ensure_http_domain_rules(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT addon_id, manifest_json FROM addons"
    )?;

    let addons: Vec<(String, String)> = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?.filter_map(|r| r.ok()).collect();

    for (addon_id, manifest_toml) in &addons {
        let table = match manifest_toml.parse::<toml::Table>() {
            Ok(t) => t,
            Err(_) => continue,
        };

        let domains = match table.get("permissions")
            .and_then(|p| p.get("http"))
            .and_then(|h| h.get("domains"))
            .and_then(|d| d.as_array())
        {
            Some(d) => d,
            None => continue,
        };

        for domain in domains {
            if let Some(host) = domain.as_str() {
                let rule_id = format!("http-{}", host);
                conn.execute(
                    "INSERT OR IGNORE INTO addon_network_rules \
                     (addon_id, rule_id, protocol, host, port, description, required, approved) \
                     VALUES (?1, ?2, 'tcp', ?3, 443, ?4, 1, 1)",
                    rusqlite::params![
                        addon_id, &rule_id, host,
                        format!("HTTPS API — {}", host)
                    ],
                ).ok();
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
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    ).unwrap_or(false);

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
    ];

    for table in &tables {
        conn.execute(
            &format!("DELETE FROM {} WHERE addon_id = ?1", table),
            rusqlite::params![addon_id],
        ).ok(); // Ignoruj bledy — tabela moze nie istniec jeszcze
    }

    // Glowna tabela addons
    conn.execute(
        "DELETE FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
    ).map_err(|e| anyhow::anyhow!("Nie udalo sie usunac addonu z DB: {e}"))?;

    conn.execute("COMMIT", [])?;

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
            new_manifest.addon_id, addon_id
        );
    }

    // Odczytaj nowy WASM
    let wasm_path = new_dir.join(&new_manifest.wasm_file);

    // CR-010: Ochrona przed path traversal
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = new_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!("Path traversal wykryty w wasm_file: {:?}", new_manifest.wasm_file);
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    let platforms_json = serde_json::to_string(&new_manifest.platforms)?;

    let conn = db.lock().unwrap();
    conn.execute("BEGIN TRANSACTION", [])?;

    // Pobierz stara wersje
    let old_version: String = conn.query_row(
        "SELECT version FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    ).map_err(|e| anyhow::anyhow!("Addon nie znaleziony: {e}"))?;

    info!(
        "Upgrade addonu '{}': {} -> {}",
        addon_id, old_version, new_manifest.version
    );

    // Zaktualizuj metadane addonu
    conn.execute(
        "UPDATE addons SET version = ?1, name = ?2, description = ?3, author = ?4, \
         manifest_json = ?5, platforms = ?6, updated_at = datetime('now') \
         WHERE addon_id = ?7",
        rusqlite::params![
            &new_manifest.version, &new_manifest.display_name,
            &new_manifest.description.as_deref().unwrap_or(""),
            &new_manifest.author.as_deref().unwrap_or(""),
            &manifest_content, &platforms_json, addon_id,
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
        ).ok();
    }

    // Synchronizacja regul sieciowych:
    // - Zachowaj approved status istniejacych regul (juz zatwierdzonych przez admina)
    // - Dodaj nowe reguly z approved=0 (wymagaja zatwierdzenia)
    // - Usun reguly ktore nie istnieja w nowym manifescie
    sync_network_rules(&conn, addon_id, &new_manifest.network_rules)?;

    conn.execute("COMMIT", [])?;

    info!("Addon '{}' zaktualizowany do v{}", addon_id, new_manifest.version);

    Ok(())
}

// =============================================================================
// Walidacja manifestu
// =============================================================================

/// Waliduje manifest addonu — wymagane pola, poprawnosc uprawnien
fn validate_manifest(manifest: &AddonManifest) -> Result<()> {
    if manifest.addon_id.is_empty() {
        bail!("addon_id nie moze byc pusty");
    }

    if manifest.addon_id.len() > 128 {
        bail!("addon_id za dlugi (max 128 znakow)");
    }

    // addon_id moze zawierac tylko litery, cyfry, kropki i myslniki
    if !manifest.addon_id.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_') {
        bail!("addon_id zawiera niedozwolone znaki (dozwolone: a-z, 0-9, '.', '-', '_')");
    }

    if manifest.version.is_empty() {
        bail!("version nie moze byc pusta");
    }

    if manifest.display_name.is_empty() {
        bail!("display_name nie moze byc pusty");
    }

    if manifest.wasm_file.is_empty() {
        bail!("wasm_file nie moze byc pusty");
    }

    // Waliduj uprawnienia
    let valid_permission_types = [
        "llm", "llm_model", "embeddings", "rag",
        "storage", "http", "events", "ui",
        "audio", "audio_capture", "audio_play", "tts", "stt",
        "camera", "notifications", "background",
        "secrets", "user_info", "timer",
        "addon_communicate", "log",
        "network", "service",
    ];

    for perm in &manifest.permissions {
        if !valid_permission_types.contains(&perm.permission_type.as_str()) {
            bail!("Nieznany typ uprawnienia: '{}'", perm.permission_type);
        }

        // access_level usuniety — uprawnienia sa teraz boolean (przyznane/nieprzyznane)
    }

    // Waliduj narzedzia
    for tool in &manifest.tools {
        if tool.name.is_empty() {
            bail!("Nazwa narzedzia nie moze byc pusta");
        }
        if tool.description.is_empty() {
            bail!("Opis narzedzia '{}' nie moze byc pusty", tool.name);
        }
    }

    // VULN-044: Waliduj reguly sieciowe — unikalne ID, niepuste pola, poprawny protokol
    let mut rule_ids = std::collections::HashSet::new();
    for rule in &manifest.network_rules {
        if rule.id.is_empty() {
            bail!("network_rule.id pusty");
        }
        if !rule_ids.insert(&rule.id) {
            bail!("duplikat network_rule.id: '{}'", rule.id);
        }
        if rule.host.is_empty() {
            bail!("network_rule '{}': host pusty", rule.id);
        }
        if rule.port == 0 {
            bail!("network_rule '{}': port musi byc 1-65535", rule.id);
        }
        if rule.protocol != "tcp" && rule.protocol != "udp" {
            bail!("network_rule '{}': protocol musi byc 'tcp' lub 'udp'", rule.id);
        }
    }

    Ok(())
}

/// Parsuje manifest.toml obslugujac oba formaty:
/// - Nowy: [addon] id, name, version, wasm_file + [permissions] + [tools]
/// - Stary: flat addon_id, version, display_name, wasm_file
pub fn parse_manifest_toml(content: &str) -> Result<AddonManifest> {
    // Sprobuj najpierw flat format (stary)
    if let Ok(manifest) = toml::from_str::<AddonManifest>(content) {
        return Ok(manifest);
    }

    // Parsuj jako zagniezdony format [addon]
    let parsed: toml::Value = toml::from_str(content)
        .map_err(|e| anyhow::anyhow!("Niepoprawny format TOML: {e}"))?;

    let addon = parsed.get("addon")
        .ok_or_else(|| anyhow::anyhow!("Brak sekcji [addon] w manifest.toml"))?;

    let addon_id = addon.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak addon.id"))?;
    let version = addon.get("version").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak addon.version"))?;
    let display_name = addon.get("name").and_then(|v| v.as_str())
        .unwrap_or(addon_id);
    let description = addon.get("description").and_then(|v| v.as_str()).map(String::from);
    let author = addon.get("author").and_then(|v| v.as_str()).map(String::from);
    let wasm_file = addon.get("wasm_file").and_then(|v| v.as_str())
        .unwrap_or("addon.wasm");
    let platforms = addon.get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // Parsuj permissions
    let mut permissions = Vec::new();
    if let Some(perms) = parsed.get("permissions") {
        if let Some(required) = perms.get("required").and_then(|v| v.as_array()) {
            for p in required {
                if let Some(s) = p.as_str() {
                    permissions.push(ManifestPermission {
                        permission_type: s.to_string(),
                        resource_pattern: None,
                        access_level: "rw".to_string(),
                        reason: None,
                        required: true,
                    });
                }
            }
        }
        if let Some(optional) = perms.get("optional").and_then(|v| v.as_array()) {
            for p in optional {
                if let Some(s) = p.as_str() {
                    permissions.push(ManifestPermission {
                        permission_type: s.to_string(),
                        resource_pattern: None,
                        access_level: "rw".to_string(),
                        reason: None,
                        required: false,
                    });
                }
            }
        }
    }

    // Parsuj tools
    let mut tools = Vec::new();
    if let Some(tools_section) = parsed.get("tools") {
        if let Some(table) = tools_section.as_table() {
            for (tool_name, tool_val) in table {
                if let Some(desc) = tool_val.get("description").and_then(|v| v.as_str()) {
                    let params = tool_val.get("parameters")
                        .map(|v| serde_json::to_value(v).unwrap_or_default())
                        .unwrap_or(serde_json::json!({}));
                    let keywords = tool_val.get("keywords")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter()
                            .filter_map(|item| item.as_str().map(String::from))
                            .collect::<Vec<String>>())
                        .unwrap_or_default();
                    tools.push(ManifestTool {
                        name: tool_name.clone(),
                        description: desc.to_string(),
                        parameters_schema: params,
                        return_schema: None,
                        keywords,
                    });
                }
            }
        }
    }

    // Parsuj addon_permissions — granularne uprawnienia deklarowane przez addon
    let mut declared_permissions = Vec::new();
    if let Some(perms_array) = parsed.get("addon_permissions").and_then(|v| v.as_array()) {
        for perm in perms_array {
            declared_permissions.push(AddonDeclaredPermission {
                id: perm.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                name: perm.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                description: perm.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                category: perm.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            });
        }
    }

    // Parsuj network_rules — jawne reguly sieciowe TCP/UDP z [[network_rules]]
    let mut network_rules = Vec::new();
    if let Some(rules) = parsed.get("network_rules").and_then(|v| v.as_array()) {
        for rule in rules {
            network_rules.push(ManifestNetworkRule {
                id: rule.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                protocol: rule.get("protocol").and_then(|v| v.as_str()).unwrap_or("tcp").to_string(),
                host: rule.get("host").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                port: rule.get("port").and_then(|v| v.as_integer()).unwrap_or(0) as u16,
                description: rule.get("description").and_then(|v| v.as_str()).map(String::from),
                required: rule.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
    }

    // Konwertuj HTTP domains z [permissions.http].domains na reguly sieciowe
    // Kazda domena HTTP to reguła TCP:443 — widoczna w UI, domyslnie dozwolona
    if let Some(http_domains) = parsed.get("permissions")
        .and_then(|p| p.get("http"))
        .and_then(|h| h.get("domains"))
        .and_then(|d| d.as_array())
    {
        for domain in http_domains {
            if let Some(host) = domain.as_str() {
                let rule_id = format!("http-{}", host);
                // Nie dodawaj duplikatu jesli juz jest w network_rules
                if !network_rules.iter().any(|r| r.id == rule_id) {
                    network_rules.push(ManifestNetworkRule {
                        id: rule_id,
                        protocol: "tcp".to_string(),
                        host: host.to_string(),
                        port: 443,
                        description: Some(format!("HTTPS API — {}", host)),
                        required: true,
                    });
                }
            }
        }
    }

    let addon_keywords = parsed.get("addon")
        .and_then(|a| a.get("keywords"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter()
            .filter_map(|item| item.as_str().map(String::from))
            .collect::<Vec<String>>())
        .unwrap_or_default();

    let category = parsed.get("addon")
        .and_then(|a| a.get("category"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Parsuj disambiguation — reguly rozstrzygania niejednoznacznych zapytan
    let disambiguation = parsed.get("addon")
        .and_then(|a| a.get("disambiguation"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().filter_map(|item| {
                let trigger = item.get("trigger")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let prefer = item.get("prefer").and_then(|v| v.as_str())?.to_string();
                let over = item.get("over").and_then(|v| v.as_str())?.to_string();
                let when = item.get("when").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some(DisambiguationRule { trigger, prefer, over, when })
            }).collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Parsuj sekcje [resources] — wymagania zasobow deklarowane przez addon
    let resources = parsed.get("resources").map(|res| {
        ResourceRequirements {
            storage_total_mb: res.get("storage_total_mb").and_then(|v| v.as_integer()).map(|v| v as u64),
            storage_value_mb: res.get("storage_value_mb").and_then(|v| v.as_integer()).map(|v| v as u64),
            llm_tokens_per_minute: res.get("llm_tokens_per_minute").and_then(|v| v.as_integer()).map(|v| v as u64),
            http_requests_per_minute: res.get("http_requests_per_minute").and_then(|v| v.as_integer()).map(|v| v as u64),
            memory_mb: res.get("memory_mb").and_then(|v| v.as_integer()).map(|v| v as u64),
            fuel_limit: res.get("fuel_limit").and_then(|v| v.as_integer()).map(|v| v as u64),
        }
    });

    Ok(AddonManifest {
        addon_id: addon_id.to_string(),
        version: version.to_string(),
        display_name: display_name.to_string(),
        description,
        author,
        permissions,
        platforms,
        wasm_file: wasm_file.to_string(),
        skill_file: Some("SKILL.md".to_string()),
        blocks_file: Some("blocks.json".to_string()),
        icon_file: None,
        resource_limits: None,
        keywords: addon_keywords,
        category,
        tools,
        declared_permissions,
        network_rules,
        disambiguation,
        resources,
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
    let mut stmt = conn.prepare(
        "SELECT rule_id FROM addon_network_rules WHERE addon_id = ?1"
    )?;
    let existing_ids: Vec<String> = stmt.query_map(
        rusqlite::params![addon_id],
        |row| row.get::<_, String>(0),
    )?.filter_map(|r| r.ok()).collect();

    let new_ids: Vec<&str> = new_rules.iter().map(|r| r.id.as_str()).collect();

    // Usun reguly ktore nie istnieja w nowym manifescie
    for old_id in &existing_ids {
        if !new_ids.contains(&old_id.as_str()) {
            conn.execute(
                "DELETE FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
                rusqlite::params![addon_id, old_id],
            )?;
            info!("upgrade: usunieto regule sieciowa '{}' addonu '{}'", old_id, addon_id);
        }
    }

    // Upsert: zaktualizuj istniejace, dodaj nowe (approved=0)
    // VULN-042: Jesli host/port/protocol sie zmienil — reset approved=0
    for rule in new_rules {
        if existing_ids.contains(&rule.id) {
            // Sprawdz czy cel polaczenia sie zmienil (host, port, protocol)
            let (old_host, old_port, old_proto): (String, i64, String) = conn.query_row(
                "SELECT host, port, protocol FROM addon_network_rules \
                 WHERE addon_id = ?1 AND rule_id = ?2",
                rusqlite::params![addon_id, &rule.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap_or_default();

            let target_changed = old_host != rule.host
                || old_port != rule.port as i64
                || old_proto != rule.protocol;

            if target_changed {
                // Cel polaczenia sie zmienil — wymagaj ponownego zatwierdzenia
                conn.execute(
                    "UPDATE addon_network_rules \
                     SET protocol = ?1, host = ?2, port = ?3, description = ?4, required = ?5, \
                         approved = 0, approved_by = NULL, approved_at = NULL \
                     WHERE addon_id = ?6 AND rule_id = ?7",
                    rusqlite::params![
                        &rule.protocol, &rule.host, rule.port,
                        rule.description.as_deref().unwrap_or(""),
                        rule.required as i32,
                        addon_id, &rule.id,
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
                        addon_id, &rule.id,
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
                    addon_id, &rule.id, &rule.protocol,
                    &rule.host, rule.port,
                    rule.description.as_deref().unwrap_or(""),
                    rule.required as i32,
                ],
            )?;
            info!("upgrade: dodano nowa regule sieciowa '{}' addonu '{}' (wymaga zatwierdzenia)", rule.id, addon_id);
        }
    }

    Ok(())
}
