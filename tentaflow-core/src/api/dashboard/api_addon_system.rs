// =============================================================================
// Plik: api/dashboard/api_addon_system.rs
// Opis: Endpointy REST dla systemu uzytkownikow, grup, addonow, uprawnien i audytu.
// =============================================================================

use std::sync::Arc;

use super::auth::Claims;
use crate::db::{self, DbPool};
use anyhow::Result;
use serde::Deserialize;

// =============================================================================
// Helpery
// =============================================================================

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

/// VULN-010: Rekurencyjne zliczanie plikow w katalogu
fn count_files_recursive(dir: &std::path::Path) -> usize {
    let mut count = 0usize;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            count += 1;
            if entry.path().is_dir() {
                count += count_files_recursive(&entry.path());
            }
        }
    }
    count
}

/// VULN-010: Walidacja path traversal (zip slip) — sprawdz czy zadna sciezka
/// nie wychodzi poza katalog bazowy po canonicalize
fn validate_no_path_traversal(base_dir: &std::path::Path) -> std::result::Result<(), String> {
    fn check_dir(base: &std::path::Path, dir: &std::path::Path) -> std::result::Result<(), String> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let canonical = path
                    .canonicalize()
                    .map_err(|e| format!("Blad canonicalize sciezki {:?}: {}", path, e))?;
                if !canonical.starts_with(base) {
                    return Err(format!(
                        "Wykryto path traversal (zip slip): {:?} wychodzi poza katalog docelowy",
                        canonical
                    ));
                }
                if canonical.is_dir() {
                    check_dir(base, &canonical)?;
                }
            }
        }
        Ok(())
    }
    check_dir(base_dir, base_dir)
}

/// Wyciaga narzedzia z sekcji [tools] manifestu TOML.
/// Format manifestu: [tools.send_message] description = "..." [tools.send_message.parameters] ...
/// Zwraca tablice JSON obiektow z name, description, parameters.
fn parse_tools_from_manifest(manifest: &toml::Value, addon_id: &str) -> Vec<serde_json::Value> {
    manifest
        .get("tools")
        .and_then(|t| t.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(tool_name, tool_val)| {
                    let desc = tool_val.get("description").and_then(|v| v.as_str())?;
                    let params = tool_val
                        .get("parameters")
                        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
                        .unwrap_or(serde_json::json!({}));
                    Some(serde_json::json!({
                        "name": format!("{}.{}", addon_id, tool_name),
                        "description": desc,
                        "parameters": params,
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// VULN-004: Sprawdza czy uzytkownik jest adminem — ZAWSZE query do DB (Zero Trust).
/// Nigdy nie ufaj claims z JWT — is_admin moze byc sfalsowany.
fn is_admin(pool: &DbPool, claims: &Claims) -> bool {
    db::repository::get_user_account_by_id(pool, claims.user_id)
        .ok()
        .flatten()
        .map(|u| u.is_admin)
        .unwrap_or(false)
}

// =============================================================================
// Users API
// =============================================================================

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
}

/// POST /api/users — tworzenie nowego uzytkownika (admin only)
pub fn handle_create_user(pool: &DbPool, claims: &Claims, body: &[u8]) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: CreateUserRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // CR-011: Minimalna dlugosc hasla — 8 znakow
    if req.username.is_empty() || req.password.len() < 8 {
        return Ok((
            400,
            json_error("Nazwa użytkownika nie może być pusta, hasło min 8 znaków"),
        ));
    }

    // Sprawdz czy uzytkownik juz istnieje
    if db::repository::get_user_account_by_username(pool, &req.username)?.is_some() {
        return Ok((409, json_error("Użytkownik o tej nazwie już istnieje")));
    }

    let password_hash = crate::crypto::hash_password(&req.password)?;
    let display_name = req.display_name.as_deref().unwrap_or("");
    let email = req.email.as_deref().unwrap_or("");

    let id = db::repository::create_user_account(
        pool,
        &req.username,
        &password_hash,
        display_name,
        email,
    )?;

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        None,
        "user.create",
        Some(&req.username),
        None,
        None,
        None,
    );

    Ok((
        201,
        serde_json::json!({"id": id, "username": req.username}).to_string(),
    ))
}

#[derive(Deserialize)]
pub struct UpdateUserRequest {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_active: Option<bool>,
}

/// PUT /api/users/:id — aktualizacja uzytkownika
pub fn handle_update_user(
    pool: &DbPool,
    claims: &Claims,
    user_id: i64,
    body: &[u8],
) -> Result<(u16, String)> {
    // Admin moze edytowac kazdego, zwykly user tylko siebie
    if claims.user_id != user_id && !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień")));
    }

    let req: UpdateUserRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let existing = db::repository::get_user_account_by_id(pool, user_id)?
        .ok_or_else(|| anyhow::anyhow!("Uzytkownik nie istnieje"))?;

    let display_name = req
        .display_name
        .as_deref()
        .unwrap_or(&existing.display_name);
    let email = req.email.as_deref().unwrap_or(&existing.email);
    let is_active = req.is_active.unwrap_or(existing.is_active);

    db::repository::update_user_account(pool, user_id, display_name, email, is_active)?;

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

/// DELETE /api/users/:id — usuniecie uzytkownika (admin only)
pub fn handle_delete_user(pool: &DbPool, claims: &Claims, user_id: i64) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    // Nie pozwol usunac samego siebie
    if claims.user_id == user_id {
        return Ok((400, json_error("Nie można usunąć własnego konta")));
    }

    db::repository::delete_user_account(pool, user_id)?;

    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        None,
        "user.delete",
        Some(&user_id.to_string()),
        None,
        None,
        None,
    );

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

#[derive(Deserialize)]
pub struct ChangeUserPasswordRequest {
    pub new_password: String,
    pub current_password: Option<String>,
}

/// PUT /api/users/:id/password — zmiana hasla (user swoje z current_password, admin dowolne)
pub fn handle_change_user_password(
    pool: &DbPool,
    claims: &Claims,
    user_id: i64,
    body: &[u8],
) -> Result<(u16, String)> {
    let req: ChangeUserPasswordRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // CR-011: Minimalna dlugosc hasla — 8 znakow
    if req.new_password.len() < 8 {
        return Ok((400, json_error("Nowe hasło musi mieć minimum 8 znaków")));
    }

    let caller_is_admin = is_admin(pool, claims);

    // Zwykly user moze zmieniac tylko swoje haslo i musi podac aktualne
    if claims.user_id != user_id && !caller_is_admin {
        return Ok((403, json_error("Brak uprawnień")));
    }

    if claims.user_id == user_id && !caller_is_admin {
        // Wymaga current_password
        let current = req
            .current_password
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Wymagane aktualne haslo"))?;

        let user = db::repository::get_user_account_by_id(pool, user_id)?
            .ok_or_else(|| anyhow::anyhow!("Uzytkownik nie istnieje"))?;

        if !crate::crypto::verify_password(current, &user.password_hash) {
            return Ok((401, json_error("Niepoprawne aktualne hasło")));
        }
    }

    let new_hash = crate::crypto::hash_password(&req.new_password)?;
    db::repository::update_user_account_password(pool, user_id, &new_hash)?;

    Ok((
        200,
        serde_json::json!({"message": "Haslo zmienione pomyslnie"}).to_string(),
    ))
}

// =============================================================================
// Groups API
// =============================================================================

/// GET /api/groups — lista grup
pub fn handle_list_groups(pool: &DbPool) -> Result<(u16, String)> {
    let groups = db::repository::list_groups(pool)?;
    Ok((200, serde_json::to_string(&groups)?))
}

#[derive(Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub description: Option<String>,
}

/// POST /api/groups — tworzenie grupy (admin only)
pub fn handle_create_group(pool: &DbPool, claims: &Claims, body: &[u8]) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: CreateGroupRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.is_empty() {
        return Ok((400, json_error("Nazwa grupy nie może być pusta")));
    }

    let desc = req.description.as_deref().unwrap_or("");
    let id = db::repository::create_group(pool, &req.name, desc)?;

    Ok((
        201,
        serde_json::json!({"id": id, "name": req.name}).to_string(),
    ))
}

/// DELETE /api/groups/:id — usuniecie grupy (admin only)
pub fn handle_delete_group(pool: &DbPool, claims: &Claims, group_id: i64) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    // Nie pozwol usunac grupy admins (id=1)
    if group_id == 1 {
        return Ok((400, json_error("Nie można usunąć systemowej grupy admins")));
    }

    db::repository::delete_group(pool, group_id)?;
    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

#[derive(Deserialize)]
pub struct AddMemberRequest {
    pub user_id: i64,
}

/// POST /api/groups/:id/members — dodanie uzytkownika do grupy
pub fn handle_add_group_member(
    pool: &DbPool,
    claims: &Claims,
    group_id: i64,
    body: &[u8],
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: AddMemberRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    db::repository::add_user_to_group(pool, group_id, req.user_id)?;
    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

/// DELETE /api/groups/:id/members/:user_id — usuniecie uzytkownika z grupy
pub fn handle_remove_group_member(
    pool: &DbPool,
    claims: &Claims,
    group_id: i64,
    user_id: i64,
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    db::repository::remove_user_from_group(pool, group_id, user_id)?;
    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

// =============================================================================
// Addons API
// =============================================================================

/// GET /api/addons/:id/permissions — uprawnienia addonu (deklarowane + przyznane)
pub fn handle_get_addon_permissions(pool: &DbPool, addon_id: &str) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Pobierz manifest z DB i wyciagnij addon_permissions
    let manifest_toml: String = match conn.query_row(
        "SELECT COALESCE(manifest_json, '') FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    ) {
        Ok(m) => m,
        Err(_) => return Ok((404, json_error("Addon nie znaleziony"))),
    };

    // Parse manifest and extract granular permissions from the canonical
    // [[permission]] array. Legacy sections (e.g. [[addon_permissions]]) are
    // rejected by the addon install path, so they cannot appear here.
    let manifest: toml::Value =
        toml::from_str(&manifest_toml).unwrap_or(toml::Value::Table(toml::map::Map::new()));

    let declared_permissions: Vec<serde_json::Value> = manifest
        .get("permission")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().filter_map(|perm| {
                Some(serde_json::json!({
                    "id": perm.get("id")?.as_str()?,
                    "display_name": perm.get("display_name").and_then(|v| v.as_str()).unwrap_or(""),
                    "description": perm.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "risk": perm.get("risk").and_then(|v| v.as_str()).unwrap_or("low"),
                }))
            }).collect()
        })
        .unwrap_or_default();

    drop(conn);

    // Pobierz przyznane uprawnienia z DB (stara tabela addon_granted_permissions)
    let granted = db::repository::get_addon_permissions(pool, addon_id)?;

    // Pobierz nazwy uzytkownikow i grup dla granted
    let granted_enriched: Vec<serde_json::Value> = granted
        .iter()
        .map(|p| {
            serde_json::json!({
                "addon_id": p.addon_id,
                "subject_type": p.subject_type,
                "subject_id": p.subject_id,
                "permission_id": p.permission_id,
                "granted": p.granted,
                "created_at": p.created_at,
            })
        })
        .collect();

    Ok((
        200,
        serde_json::json!({
            "declared_permissions": declared_permissions,
            "granted": granted_enriched,
        })
        .to_string(),
    ))
}

#[derive(Deserialize)]
pub struct SetPermissionRequest {
    pub subject_type: String,
    pub subject_id: i64,
    /// Identyfikator uprawnienia z [[addon_permissions]]
    pub permission_id: String,
    /// Czy uprawnienie jest przyznane (true/false)
    #[serde(default = "default_granted")]
    pub granted: bool,
}

fn default_granted() -> bool {
    true
}

/// PUT /api/addons/:id/permissions — ustawienie uprawnien addonu (boolean: przyznane/nieprzyznane)
pub fn handle_set_addon_permissions(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    body: &[u8],
    permission_checker: Option<&Arc<crate::addon::permissions::PermissionChecker>>,
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: SetPermissionRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // Walidacja subject_type
    if req.subject_type != "user" && req.subject_type != "group" {
        return Ok((400, json_error("subject_type musi być 'user' lub 'group'")));
    }

    // Walidacja permission_id
    if req.permission_id.is_empty() {
        return Ok((400, json_error("permission_id nie może być pusty")));
    }

    db::repository::set_addon_permission(
        pool,
        addon_id,
        &req.subject_type,
        req.subject_id,
        &req.permission_id,
        req.granted,
    )?;

    // Natychmiastowe odswiezenie cache uprawnien po zmianie
    if let Some(checker) = permission_checker {
        checker.refresh_addon(addon_id);
    }

    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        "permission.set",
        Some(&req.permission_id),
        Some(&format!(
            "{}:{} -> granted={}",
            req.subject_type, req.subject_id, req.granted
        )),
        None,
        None,
    );

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

// =============================================================================
// Addons: Install, Tools, UI
// =============================================================================

/// POST /api/addons/install — instalacja addonu z ZIP (body = multipart/form-data z plikiem ZIP)
pub fn handle_install_addon(pool: &DbPool, claims: &Claims, body: &[u8]) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    // Walidacja — minimalna wielkosc pliku
    if body.len() < 64 {
        return Ok((400, json_error("Plik ZIP jest za mały lub pusty")));
    }

    // Maksymalny rozmiar addonu (50 MB)
    const MAX_ADDON_SIZE: usize = 50 * 1024 * 1024;
    if body.len() > MAX_ADDON_SIZE {
        return Ok((400, json_error("Plik ZIP przekracza limit 50 MB")));
    }

    // Sprawdz sygnature ZIP (PK\x03\x04)
    if body.len() >= 4 && &body[0..4] != b"PK\x03\x04" {
        return Ok((400, json_error("Plik nie jest poprawnym archiwum ZIP")));
    }

    // Utworz tymczasowy katalog i rozpakuj ZIP
    let temp_dir =
        std::env::temp_dir().join(format!("tentaflow_addon_install_{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        return Ok((
            500,
            json_error(&format!("Blad tworzenia katalogu tymczasowego: {}", e)),
        ));
    }

    // Zapisz ZIP do pliku tymczasowego
    let zip_path = temp_dir.join("addon.zip");
    if let Err(e) = std::fs::write(&zip_path, body) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok((500, json_error(&format!("Blad zapisu pliku ZIP: {}", e))));
    }

    // Rozpakuj — uzyj komendy unzip (dostepna na Linux/macOS/Windows)
    let extract_dir = temp_dir.join("extracted");
    if let Err(e) = std::fs::create_dir_all(&extract_dir) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok((500, json_error(&format!("Blad tworzenia katalogu: {}", e))));
    }

    let unzip_result = std::process::Command::new("unzip")
        .args(["-o", "-q"])
        .arg(zip_path.to_str().unwrap_or(""))
        .arg("-d")
        .arg(extract_dir.to_str().unwrap_or(""))
        .output();

    match unzip_result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Ok((
                400,
                json_error(&format!("Blad rozpakowywania ZIP: {}", stderr)),
            ));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Ok((
                500,
                json_error(&format!("Nie udalo sie uruchomic unzip: {}", e)),
            ));
        }
    }

    // VULN-010: Walidacja zip slip — sprawdz czy zadna sciezka nie wychodzi poza extract_dir
    let canonical_extract = match extract_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Ok((500, json_error(&format!("Blad walidacji katalogu: {}", e))));
        }
    };

    // VULN-010: Limit liczby plikow w ZIP (max 500)
    const MAX_FILES_IN_ZIP: usize = 500;
    let file_count = count_files_recursive(&canonical_extract);
    if file_count > MAX_FILES_IN_ZIP {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok((
            400,
            json_error(&format!(
                "ZIP zawiera zbyt wiele plikow ({} > {})",
                file_count, MAX_FILES_IN_ZIP
            )),
        ));
    }

    // VULN-010: Sprawdz path traversal (zip slip) — zadna sciezka nie moze wychodzic poza extract_dir
    if let Err(msg) = validate_no_path_traversal(&canonical_extract) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok((400, json_error(&msg)));
    }

    // Sprawdz czy manifest.toml istnieje w rozpakowanym katalogu
    let manifest_path = extract_dir.join("manifest.toml");
    if !manifest_path.exists() {
        // Moze byc w podkatalogu (jesli ZIP zawieral folder)
        let entries: Vec<_> = std::fs::read_dir(&extract_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();

        if entries.len() == 1 && entries[0].path().is_dir() {
            let inner_dir = entries[0].path();
            if !inner_dir.join("manifest.toml").exists() {
                let _ = std::fs::remove_dir_all(&temp_dir);
                return Ok((400, json_error("Brak manifest.toml w archiwum ZIP")));
            }
            // Uzywamy inner_dir jako addon_path — ale zapisujemy sciezke
            let addon_path = inner_dir;
            let result = install_addon_from_path(pool, claims, &addon_path);
            let _ = std::fs::remove_dir_all(&temp_dir);
            return result;
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok((400, json_error("Brak manifest.toml w archiwum ZIP")));
    }

    let result = install_addon_from_path(pool, claims, &extract_dir);
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

/// Wewnetrzna funkcja instalacji addonu ze sciezki
fn install_addon_from_path(
    pool: &DbPool,
    claims: &Claims,
    addon_path: &std::path::Path,
) -> Result<(u16, String)> {
    // Czytaj manifest.toml
    let manifest_str = std::fs::read_to_string(addon_path.join("manifest.toml"))
        .map_err(|e| anyhow::anyhow!("Blad odczytu manifest.toml: {}", e))?;

    // Parsuj manifest — wyciagnij addon_id i version
    let manifest: toml::Value = toml::from_str(&manifest_str)
        .map_err(|e| anyhow::anyhow!("Blad parsowania manifest.toml: {}", e))?;

    let addon_id = manifest
        .get("addon")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Brak addon.id w manifest.toml"))?;

    let version = manifest
        .get("addon")
        .and_then(|a| a.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0");

    let display_name = manifest
        .get("addon")
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(addon_id);

    let description = manifest
        .get("addon")
        .and_then(|a| a.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let author = manifest
        .get("addon")
        .and_then(|a| a.get("author"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Czytaj opcjonalne pliki
    let _skill_md = std::fs::read_to_string(addon_path.join("SKILL.md")).ok();
    let _blocks_json = std::fs::read_to_string(addon_path.join("blocks.json")).ok();

    // Czytaj WASM
    let wasm_path = addon_path.join("addon.wasm");
    let wasm_bytes = if wasm_path.exists() {
        std::fs::read(&wasm_path).map_err(|e| anyhow::anyhow!("Blad odczytu addon.wasm: {}", e))?
    } else {
        // Szukaj w target/wasm32-wasi/release/
        let alt_path = addon_path
            .join("target")
            .join("wasm32-wasi")
            .join("release");
        let wasm_files: Vec<_> = std::fs::read_dir(&alt_path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map_or(false, |ext| ext == "wasm"))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(wasm_entry) = wasm_files.first() {
            std::fs::read(wasm_entry.path())
                .map_err(|e| anyhow::anyhow!("Blad odczytu WASM: {}", e))?
        } else {
            return Ok((400, json_error("Brak pliku .wasm w archiwum (oczekiwany addon.wasm lub target/wasm32-wasi/release/*.wasm)")));
        }
    };

    // VULN-017: Limit rozmiaru WASM — max 100 MB
    const MAX_WASM_SIZE: usize = 100 * 1024 * 1024;
    if wasm_bytes.len() > MAX_WASM_SIZE {
        return Ok((400, json_error("WASM za duży (max 100 MB)")));
    }

    // Hash SHA-256 pliku WASM
    use sha2::{Digest, Sha256};
    let _wasm_hash = format!("{:x}", Sha256::digest(&wasm_bytes));

    // Zapisz w DB
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Sprawdz czy addon juz istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if exists {
        // Aktualizacja istniejacego addonu
        conn.execute(
            "UPDATE addons SET version = ?2, name = ?3, description = ?4, author = ?5, \
             manifest_json = ?6, is_enabled = 1, updated_at = datetime('now') \
             WHERE addon_id = ?1",
            rusqlite::params![
                addon_id,
                version,
                display_name,
                description,
                author,
                &manifest_str
            ],
        )
        .map_err(|e| anyhow::anyhow!("Blad aktualizacji addonu w DB: {}", e))?;

        // Aktualizuj WASM
        conn.execute(
            "INSERT OR REPLACE INTO addon_wasm (addon_id, wasm_bytes) VALUES (?1, ?2)",
            rusqlite::params![addon_id, &wasm_bytes],
        )
        .map_err(|e| anyhow::anyhow!("Blad zapisu WASM: {}", e))?;
    } else {
        // Nowy addon
        conn.execute(
            "INSERT INTO addons (addon_id, version, name, description, author, \
             manifest_json, is_enabled) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            rusqlite::params![
                addon_id,
                version,
                display_name,
                description,
                author,
                &manifest_str
            ],
        )
        .map_err(|e| anyhow::anyhow!("Blad zapisu addonu w DB: {}", e))?;

        // Zapisz WASM
        conn.execute(
            "INSERT INTO addon_wasm (addon_id, wasm_bytes) VALUES (?1, ?2)",
            rusqlite::params![addon_id, &wasm_bytes],
        )
        .map_err(|e| anyhow::anyhow!("Blad zapisu WASM: {}", e))?;
    }

    // Audit log
    drop(conn);
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        "addon.install",
        None,
        Some(&format!("v{}, WASM: {} bytes", version, wasm_bytes.len())),
        None,
        None,
    );

    // Domyslne limity zasobow (0 = bez limitu) — INSERT OR IGNORE nie nadpisuje istniejacych
    let _ = db::repository::create_default_addon_resource_limits(pool, addon_id);

    Ok((
        201,
        serde_json::json!({
            "addon_id": addon_id,
            "version": version,
            "display_name": display_name,
            "wasm_size_bytes": wasm_bytes.len(),
            "updated": exists,
        })
        .to_string(),
    ))
}

/// GET /api/addons/:id/tools — lista narzedzi konkretnego addonu
pub fn handle_get_addon_tools(pool: &DbPool, addon_id: &str) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Sprawdz czy addon istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !exists {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    // Pobierz manifest i sparsuj narzedzia
    let manifest_toml: String = conn
        .query_row(
            "SELECT manifest_json FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .map_err(|e| anyhow::anyhow!("Blad odczytu manifestu: {}", e))?;

    let manifest: toml::Value =
        toml::from_str(&manifest_toml).unwrap_or(toml::Value::Table(toml::map::Map::new()));

    // Wyciagnij narzedzia z sekcji [tools] manifestu (mapa klucz=nazwa, wartosc=definicja)
    let tools = parse_tools_from_manifest(&manifest, addon_id);

    let skill_md: Option<String> = conn
        .query_row(
            "SELECT skill_md FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    Ok((
        200,
        serde_json::json!({
            "addon_id": addon_id,
            "tools": tools,
            "skill_md": skill_md,
        })
        .to_string(),
    ))
}

/// GET /api/addons/:id/ui — panel UI addonu (SKILL.md + config_schema)
pub fn handle_get_addon_ui(pool: &DbPool, addon_id: &str) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Sprawdz czy addon istnieje
    let row = conn.query_row(
        "SELECT name, description, manifest_json, is_enabled, version \
         FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    );

    match row {
        Ok((display_name, description, manifest_toml, is_enabled, version)) => {
            let skill_md: Option<String> = conn
                .query_row(
                    "SELECT skill_md FROM addons WHERE addon_id = ?1",
                    rusqlite::params![addon_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten();
            let blocks_json: Option<String> = None;
            let status = if is_enabled {
                "installed".to_string()
            } else {
                "disabled".to_string()
            };
            // Wyciagnij config.schema z manifestu (zagniezdony format [config.schema])
            let manifest: toml::Value =
                toml::from_str(&manifest_toml).unwrap_or(toml::Value::Table(toml::map::Map::new()));

            // Probuj rozne sciezki: config.schema, config_schema, config.fields
            let config_schema = manifest
                .get("config")
                .and_then(|c| c.get("schema"))
                .or_else(|| manifest.get("config_schema"))
                .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
                .unwrap_or(serde_json::json!({}));

            let ui_config = manifest
                .get("ui")
                .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
                .unwrap_or(serde_json::json!({}));

            // Wyciagnij narzedzia z sekcji [tools]
            let tools = manifest
                .get("tools")
                .and_then(|t| t.as_table())
                .map(|table| {
                    table
                        .iter()
                        .filter_map(|(tool_name, tool_val)| {
                            let desc = tool_val.get("description").and_then(|v| v.as_str())?;
                            let params = tool_val
                                .get("parameters")
                                .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
                                .unwrap_or(serde_json::json!({}));
                            Some(serde_json::json!({
                                "name": format!("{}.{}", addon_id, tool_name),
                                "description": desc,
                                "parameters": params,
                            }))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            // Pobierz zapisane wartosci konfiguracji z DB (uzyj tego samego conn)
            let config_values = {
                let mut values = std::collections::HashMap::new();
                if let Ok(mut stmt) =
                    conn.prepare("SELECT key, value FROM addon_config WHERE addon_id = ?1")
                {
                    if let Ok(rows) = stmt.query_map(rusqlite::params![addon_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    }) {
                        for row in rows.flatten() {
                            values.insert(row.0, row.1);
                        }
                    }
                }
                values
            };

            Ok((200, serde_json::json!({
                "addon_id": addon_id,
                "display_name": display_name,
                "description": description,
                "version": version,
                "status": status,
                "config_schema": config_schema,
                "config_values": config_values,
                "ui": ui_config,
                "tools": tools,
                "skill_md": skill_md,
                "blocks_json": blocks_json.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            }).to_string()))
        }
        Err(_) => Ok((404, json_error("Addon nie znaleziony"))),
    }
}

/// GET /api/tools — lista wszystkich narzedzi ze wszystkich addonow (dla LLM)
pub fn handle_list_all_tools(pool: &DbPool) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Pobierz wszystkie aktywne/zainstalowane addony z ich manifestami
    let mut stmt = conn
        .prepare("SELECT addon_id, manifest_json, '' FROM addons WHERE is_enabled = 1")
        .map_err(|e| anyhow::anyhow!("Blad przygotowania zapytania: {}", e))?;

    let addons: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut all_tools: Vec<serde_json::Value> = Vec::new();

    for (addon_id, manifest_toml, _skill_md) in &addons {
        let manifest: toml::Value = match toml::from_str(manifest_toml) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let tools = parse_tools_from_manifest(&manifest, addon_id);
        for tool in tools {
            all_tools.push(serde_json::json!({
                "type": "function",
                "function": tool,
                "addon_id": addon_id,
            }));
        }
    }

    Ok((
        200,
        serde_json::json!({
            "tools": all_tools,
            "count": all_tools.len(),
        })
        .to_string(),
    ))
}

// =============================================================================
// Parsowanie query string
// =============================================================================

fn parse_query_opt_string(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        let val = parts.next()?;
        if key == name && !val.is_empty() {
            Some(val.to_string())
        } else {
            None
        }
    })
}

// =============================================================================
// SSO Providers API — flow OAuth (login redirect + callback). Zarzadzanie
// providerami (list/create/delete) odbywa sie przez binary protocol (#FAZA 4).
// =============================================================================

/// GET /api/sso/login/:provider_id — generuje auth URL i zwraca redirect
pub async fn handle_sso_login(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    provider_id: i64,
    redirect_base_url: &str,
) -> Result<(u16, String)> {
    let provider = db::repository::get_sso_provider(pool, provider_id)?
        .ok_or_else(|| anyhow::anyhow!("SSO provider nie znaleziony"))?;

    if !provider.enabled {
        return Ok((400, json_error("SSO provider jest wyłączony")));
    }

    // Odszyfruj client_secret
    let client_secret = cipher
        .decrypt(&provider.client_secret_encrypted)
        .map_err(|e| anyhow::anyhow!("Blad odszyfrowywania client_secret: {}", e))?;

    // Pobierz redirect base URL z ustawien DB (fallback na przekazany z Host header)
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| redirect_base_url.to_string());

    let config = crate::auth::sso::provider_to_config(&provider, &client_secret, &base_url);

    // Discovery
    let discovery = crate::auth::sso::discover(&config.discovery_url)
        .await
        .map_err(|e| anyhow::anyhow!("Blad OIDC discovery: {}", e))?;

    // CR-016: Generuj state (anti-CSRF) — provider_id + losowy UUID + timestamp
    let state = format!("{}:{}", provider_id, uuid::Uuid::new_v4());

    // Zapisz state z timestampem w ustawieniach (walidacja TTL przy callback)
    let state_value = format!("{}:{}", provider_id, chrono::Utc::now().timestamp());
    let _ = db::repository::set_setting(pool, &format!("sso_state:{}", state), &state_value);

    let auth_url = crate::auth::sso::build_auth_url(&config, &discovery, &state);

    Ok((
        200,
        serde_json::json!({
            "auth_url": auth_url,
            "state": state,
        })
        .to_string(),
    ))
}

/// GET /api/sso/callback?code=...&state=... — callback po zalogowaniu SSO
pub async fn handle_sso_callback(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    query: &str,
    redirect_base_url: &str,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<(u16, String)> {
    let code = parse_query_opt_string(query, "code")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'code' w callback"))?;
    let state = parse_query_opt_string(query, "state")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'state' w callback"))?;

    // CR-016: Zweryfikuj state (anti-CSRF) z walidacja TTL
    let state_key = format!("sso_state:{}", state);
    let state_value = db::repository::get_setting(pool, &state_key)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny lub wygasniety state SSO"))?;

    // Natychmiast usun zuzyty state (jednorazowe uzycie — zapobiega replay attack)
    let _ = db::repository::delete_setting(pool, &state_key);

    // Parsuj provider_id i timestamp z state_value (format: "provider_id:timestamp")
    let parts: Vec<&str> = state_value.splitn(2, ':').collect();
    let provider_id: i64 = parts
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny provider_id w state"))?;

    // CR-016: Sprawdz TTL state (max 10 minut)
    if let Some(ts_str) = parts.get(1) {
        if let Ok(ts) = ts_str.parse::<i64>() {
            let now = chrono::Utc::now().timestamp();
            let max_age_seconds = 600; // 10 minut
            if now - ts > max_age_seconds {
                return Err(anyhow::anyhow!(
                    "State SSO wygasniety (starszy niz 10 minut)"
                ));
            }
        }
    }

    let provider = db::repository::get_sso_provider(pool, provider_id)?
        .ok_or_else(|| anyhow::anyhow!("SSO provider nie znaleziony"))?;

    // Odszyfruj client_secret
    let client_secret = cipher
        .decrypt(&provider.client_secret_encrypted)
        .map_err(|e| anyhow::anyhow!("Blad odszyfrowywania client_secret: {}", e))?;

    // Pobierz redirect base URL z ustawien DB (fallback na przekazany z Host header)
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| redirect_base_url.to_string());

    let config = crate::auth::sso::provider_to_config(&provider, &client_secret, &base_url);

    // Discovery
    let discovery = crate::auth::sso::discover(&config.discovery_url)
        .await
        .map_err(|e| anyhow::anyhow!("Blad OIDC discovery: {}", e))?;

    // Pelny flow: exchange code -> get user info -> find/create user -> JWT
    let result =
        crate::auth::sso::handle_sso_callback(pool, &config, &discovery, &code, settings_cipher)
            .await?;

    // Redirect do dashboardu z tokenem JWT w query param
    let redirect_url = format!(
        "{}/?token={}",
        base_url.trim_end_matches('/'),
        urlencoding::encode(&result.token)
    );
    Ok((
        200,
        serde_json::json!({
            "redirect_url": redirect_url,
            "token": result.token,
            "username": result.username,
            "is_new_user": result.is_new_user,
        })
        .to_string(),
    ))
}

// =============================================================================
// Addon: Resource Limits (limity zasobow)
// =============================================================================

/// GET /api/addons/:id/limits — pobiera limity zasobow addonu
pub fn handle_get_addon_limits(pool: &DbPool, addon_id: &str) -> Result<(u16, String)> {
    // Sprawdz czy addon istnieje
    if db::repository::get_addon(pool, addon_id)?.is_none() {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    let limits = db::repository::get_addon_resource_limits(pool, addon_id)?;

    // Zwroc limity z labelami i presetami fuel do wyswietlenia w UI
    let response = serde_json::json!({
        "max_instances": limits.max_instances,
        "fuel_limit": limits.fuel_limit,
        "ram_limit_mb": limits.ram_limit_mb,
        "gpu_enabled": limits.gpu_enabled,
        "vram_limit_mb": limits.vram_limit_mb,
        "storage_limit_mb": limits.storage_limit_mb,
        "http_requests_per_min": limits.http_requests_per_min,
        "llm_tokens_per_min": limits.llm_tokens_per_min,
        "fuel_presets": {
            "light": {"value": 1_000_000, "label": "Lekki (1M) — proste narzedzia"},
            "standard": {"value": 10_000_000, "label": "Standardowy (10M) — typowe addony"},
            "intensive": {"value": 100_000_000, "label": "Intensywny (100M) — ciezkie obliczenia"},
            "unlimited": {"value": 0, "label": "Nieograniczony — zaufane addony"}
        },
        "labels": {
            "max_instances": "Maks. instancji (0 = bez limitu)",
            "fuel_limit": "Limit obliczen (fuel per wywolanie)",
            "ram_limit_mb": "Limit RAM (MB, 0 = bez limitu)",
            "gpu_enabled": "Dostęp do GPU",
            "vram_limit_mb": "Limit VRAM (MB, 0 = bez limitu)",
            "storage_limit_mb": "Limit storage (MB, 0 = bez limitu)",
            "http_requests_per_min": "Limit HTTP req/min (0 = bez limitu)",
            "llm_tokens_per_min": "Limit tokenów LLM/min (0 = bez limitu)"
        }
    });

    Ok((200, serde_json::to_string(&response)?))
}

/// Request body dla PUT /api/addons/:id/limits
#[derive(Deserialize)]
pub struct SetAddonLimitsRequest {
    pub max_instances: Option<i64>,
    pub cpu_limit_ms_per_min: Option<i64>,
    pub ram_limit_mb: Option<i64>,
    pub gpu_enabled: Option<bool>,
    pub vram_limit_mb: Option<i64>,
    pub storage_limit_mb: Option<i64>,
    pub http_requests_per_min: Option<i64>,
    pub llm_tokens_per_min: Option<i64>,
    /// Limit paliwa WASM per wywolanie (0 = domyslny 10M instrukcji)
    pub fuel_limit: Option<i64>,
}

/// PUT /api/addons/:id/limits — ustawia limity zasobow addonu (admin only)
pub fn handle_set_addon_limits(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    body: &[u8],
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    // Sprawdz czy addon istnieje
    if db::repository::get_addon(pool, addon_id)?.is_none() {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    let req: SetAddonLimitsRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // Pobierz aktualne limity i zastosuj zmiany (merge)
    let current = db::repository::get_addon_resource_limits(pool, addon_id)?;

    let limits = db::repository::AddonResourceLimits {
        addon_id: addon_id.to_string(),
        max_instances: req.max_instances.unwrap_or(current.max_instances),
        cpu_limit_ms_per_min: req
            .cpu_limit_ms_per_min
            .unwrap_or(current.cpu_limit_ms_per_min),
        ram_limit_mb: req.ram_limit_mb.unwrap_or(current.ram_limit_mb),
        gpu_enabled: req.gpu_enabled.unwrap_or(current.gpu_enabled),
        vram_limit_mb: req.vram_limit_mb.unwrap_or(current.vram_limit_mb),
        storage_limit_mb: req.storage_limit_mb.unwrap_or(current.storage_limit_mb),
        http_requests_per_min: req
            .http_requests_per_min
            .unwrap_or(current.http_requests_per_min),
        llm_tokens_per_min: req.llm_tokens_per_min.unwrap_or(current.llm_tokens_per_min),
        fuel_limit: req.fuel_limit.unwrap_or(current.fuel_limit),
    };

    db::repository::set_addon_resource_limits(pool, &limits)?;

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        "addon.limits.update",
        Some(addon_id),
        Some(&serde_json::to_string(&limits).unwrap_or_default()),
        None,
        None,
    );

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

// =============================================================================
// Addon: Enable/Disable, Uninstall, Config
// =============================================================================

#[derive(Deserialize)]
pub struct ToggleAddonRequest {
    pub enabled: bool,
}

/// PUT /api/addons/:id — wlaczanie/wylaczanie addonu
pub fn handle_toggle_addon(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    body: &[u8],
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: ToggleAddonRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // Sprawdz czy addon istnieje
    let addon = db::repository::get_addon(pool, addon_id)?;
    if addon.is_none() {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;
    conn.execute(
        "UPDATE addons SET is_enabled = ?2, updated_at = datetime('now') WHERE addon_id = ?1",
        rusqlite::params![addon_id, req.enabled],
    )
    .map_err(|e| anyhow::anyhow!("Blad aktualizacji addonu: {}", e))?;
    drop(conn);

    let action = if req.enabled {
        "addon.enable"
    } else {
        "addon.disable"
    };
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        action,
        None,
        None,
        None,
        None,
    );

    Ok((
        200,
        serde_json::json!({
            "addon_id": addon_id,
            "enabled": req.enabled,
        })
        .to_string(),
    ))
}

/// DELETE /api/addons/:id — odinstalowanie addonu
pub fn handle_uninstall_addon(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    // Sprawdz czy addon istnieje
    let addon = db::repository::get_addon(pool, addon_id)?;
    if addon.is_none() {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    // Sprawdz czy addon jest systemowy
    if addon.as_ref().map(|a| a.is_system).unwrap_or(false) {
        return Ok((400, json_error("Nie można odinstalować addonu systemowego")));
    }

    // Usun WASM z tabeli addon_wasm
    {
        let conn = pool
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;
        let _ = conn.execute(
            "DELETE FROM addon_wasm WHERE addon_id = ?1",
            rusqlite::params![addon_id],
        );
    }

    // Usun addon (CASCADE usunie powiazane rekordy)
    db::repository::delete_addon(pool, addon_id)?;

    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        "addon.uninstall",
        None,
        None,
        None,
        None,
    );

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

/// GET /api/addons/:id/config — konfiguracja addonu (wartosci z addon_config)
pub fn handle_get_addon_config(pool: &DbPool, addon_id: &str) -> Result<(u16, String)> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    // Sprawdz czy addon istnieje i pobierz manifest
    let manifest_toml: String = match conn.query_row(
        "SELECT manifest_json FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    ) {
        Ok(m) => m,
        Err(_) => return Ok((404, json_error("Addon nie znaleziony"))),
    };

    // Wyciagnij config.schema z manifestu (probuj rozne formaty)
    let manifest: toml::Value =
        toml::from_str(&manifest_toml).unwrap_or(toml::Value::Table(toml::map::Map::new()));

    let config_schema = manifest
        .get("config")
        .and_then(|c| c.get("schema"))
        .or_else(|| manifest.get("config_schema"))
        .map(|v| serde_json::to_value(v).unwrap_or(serde_json::json!({})))
        .unwrap_or(serde_json::json!({}));

    // Pobierz zapisane wartosci konfiguracji (tabela addon_config)
    let config_values: std::collections::HashMap<String, String> = conn
        .prepare("SELECT key, value FROM addon_config WHERE addon_id = ?1")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![addon_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();

    Ok((
        200,
        serde_json::json!({
            "addon_id": addon_id,
            "schema": config_schema,
            "values": config_values,
        })
        .to_string(),
    ))
}

#[derive(Deserialize)]
pub struct SetAddonConfigRequest {
    pub values: std::collections::HashMap<String, String>,
}

/// PUT /api/addons/:id/config — zapis konfiguracji addonu
pub fn handle_set_addon_config(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    body: &[u8],
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let req: SetAddonConfigRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // Sprawdz czy addon istnieje
    if db::repository::get_addon(pool, addon_id)?.is_none() {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;

    for (key, value) in &req.values {
        conn.execute(
            "INSERT INTO addon_config (addon_id, key, value) VALUES (?1, ?2, ?3) \
             ON CONFLICT(addon_id, key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
            rusqlite::params![addon_id, key, value],
        ).map_err(|e| anyhow::anyhow!("Blad zapisu konfiguracji: {}", e))?;
    }
    drop(conn);

    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        Some(addon_id),
        "addon.config.update",
        None,
        Some(&format!("{} kluczy", req.values.len())),
        None,
        None,
    );

    Ok((200, serde_json::json!({"ok": true}).to_string()))
}

// =============================================================================
// Addon OAuth — osobny flow OAuth per addon (np. Teams -> Graph API)
// =============================================================================

/// Pomocnik: pobiera wszystkie wartosci konfiguracji addonu z tabeli addon_config.
fn get_addon_config_map(
    pool: &DbPool,
    addon_id: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM addon_config WHERE addon_id = ?1")
        .map_err(|e| anyhow::anyhow!("Blad przygotowania zapytania: {}", e))?;
    let map: std::collections::HashMap<String, String> = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(map)
}

/// GET /api/addons/:addon_id/oauth/login — generuje auth URL dla addonu
/// Wymaga JWT (musimy wiedziec ktory uzytkownik sie loguje).
/// Buduje auth URL z client_id z addon config, scopami z manifestu,
/// redirect_uri z oauth_redirect_base_url + /api/addons/{addon_id}/oauth/callback.
pub async fn handle_addon_oauth_login(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
) -> Result<(u16, String)> {
    // Sprawdz czy addon istnieje
    let addon = db::repository::get_addon(pool, addon_id)?
        .ok_or_else(|| anyhow::anyhow!("Addon '{}' nie znaleziony", addon_id))?;

    if !addon.is_enabled {
        return Ok((400, json_error("Addon jest wyłączony")));
    }

    // Pobierz redirect base URL z ustawien DB
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://localhost:8090".to_string());

    // Pobierz konfiguracje addonu — client_id, tenant_id, scopes
    let config = get_addon_config_map(pool, addon_id)?;
    let client_id = config
        .get("client_id")
        .or_else(|| config.get("azure_client_id"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_id w konfiguracji addonu '{}'", addon_id))?;

    let tenant_id = config
        .get("tenant_id")
        .or_else(|| config.get("azure_tenant_id"))
        .cloned()
        .unwrap_or_else(|| "common".to_string());

    // Domyslne scopy per addon — Teams potrzebuje dodatkowych uprawnien Graph API
    let scopes = match addon_id {
        "teams" => "offline_access Chat.ReadWrite Calendars.Read Files.Read OnlineMeetings.ReadWrite User.Read",
        _ => "offline_access User.Read",
    };

    let redirect_uri = format!(
        "{}/api/addons/{}/oauth/callback",
        base_url.trim_end_matches('/'),
        addon_id
    );

    // Generuj state (anti-CSRF) — addon_id + user_id + losowy UUID + timestamp
    let state = format!("{}:{}:{}", addon_id, claims.user_id, uuid::Uuid::new_v4());
    let state_value = format!(
        "{}:{}:{}",
        addon_id,
        claims.user_id,
        chrono::Utc::now().timestamp()
    );
    let _ =
        db::repository::set_setting(pool, &format!("addon_oauth_state:{}", state), &state_value);

    // Buduj auth URL (Microsoft Azure AD / Entra ID)
    let auth_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        urlencoding::encode(&tenant_id),
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scopes),
        urlencoding::encode(&state),
    );

    Ok((
        200,
        serde_json::json!({
            "auth_url": auth_url,
            "state": state,
        })
        .to_string(),
    ))
}

/// GET /api/addons/:addon_id/oauth/callback?code=xxx&state=yyy
/// Callback OAuth per addon — wymienia code na tokeny, zapisuje do addon secrets per user.
/// Nie wymaga JWT — user wraca z Microsoft redirect.
pub async fn handle_addon_oauth_callback(
    pool: &DbPool,
    cipher: &crate::crypto::SecretsCipher,
    path: &str,
    query: &str,
) -> Result<(u16, String)> {
    // Wyciagnij addon_id ze sciezki: /api/addons/{addon_id}/oauth/callback
    let addon_id = path
        .strip_prefix("/api/addons/")
        .and_then(|rest| rest.strip_suffix("/oauth/callback"))
        .ok_or_else(|| anyhow::anyhow!("Niepoprawna sciezka callback"))?;

    let code = parse_query_opt_string(query, "code")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'code' w callback"))?;
    let state = parse_query_opt_string(query, "state")
        .ok_or_else(|| anyhow::anyhow!("Brak parametru 'state' w callback"))?;

    // Obsluga bledow od Microsoft
    if let Some(error) = parse_query_opt_string(query, "error") {
        let error_desc = parse_query_opt_string(query, "error_description").unwrap_or_default();
        return Err(anyhow::anyhow!("Blad OAuth: {} — {}", error, error_desc));
    }

    // Zweryfikuj state (anti-CSRF)
    let state_key = format!("addon_oauth_state:{}", state);
    let state_value = db::repository::get_setting(pool, &state_key)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny lub wygasniety state OAuth"))?;

    // Natychmiast usun zuzyty state (jednorazowe uzycie)
    let _ = db::repository::delete_setting(pool, &state_key);

    // Parsuj addon_id, user_id i timestamp z state_value
    let parts: Vec<&str> = state_value.splitn(3, ':').collect();
    let stored_addon_id = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny addon_id w state"))?;
    let user_id: i64 = parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Niepoprawny user_id w state"))?;

    if *stored_addon_id != addon_id {
        return Err(anyhow::anyhow!("Niezgodnosc addon_id w state"));
    }

    // Sprawdz TTL state (max 10 minut)
    if let Some(ts_str) = parts.get(2) {
        if let Ok(ts) = ts_str.parse::<i64>() {
            let now = chrono::Utc::now().timestamp();
            if now - ts > 600 {
                return Err(anyhow::anyhow!(
                    "State OAuth wygasniety (starszy niz 10 minut)"
                ));
            }
        }
    }

    // Pobierz konfiguracje addonu — client_id, client_secret, tenant_id
    let config = get_addon_config_map(pool, addon_id)?;
    let client_id = config
        .get("client_id")
        .or_else(|| config.get("azure_client_id"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_id w konfiguracji addonu"))?;

    let client_secret_encrypted = config
        .get("client_secret")
        .or_else(|| config.get("azure_client_secret"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Brak client_secret w konfiguracji addonu"))?;

    let client_secret = cipher
        .decrypt(&client_secret_encrypted)
        .unwrap_or_else(|_| client_secret_encrypted.clone());

    let tenant_id = config
        .get("tenant_id")
        .or_else(|| config.get("azure_tenant_id"))
        .cloned()
        .unwrap_or_else(|| "common".to_string());

    // Pobierz redirect base URL z DB
    let base_url = db::repository::get_setting(pool, "oauth_redirect_base_url")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://localhost:8090".to_string());

    let redirect_uri = format!(
        "{}/api/addons/{}/oauth/callback",
        base_url.trim_end_matches('/'),
        addon_id
    );

    // Wymien code na tokeny (server-to-server)
    let token_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant_id
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow::anyhow!("Blad tworzenia klienta HTTP: {}", e))?;

    let params = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ];

    let response = client
        .post(&token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Blad wymiany code na token: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Wymiana code na token zwrocila status {}: {}",
            status,
            body
        ));
    }

    let token_data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Blad parsowania odpowiedzi tokenowej: {}", e))?;

    let access_token = token_data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let refresh_token = token_data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if access_token.is_empty() {
        return Err(anyhow::anyhow!("Brak access_token w odpowiedzi tokenowej"));
    }

    // Zaszyfruj i zapisz tokeny do addon secrets per user
    let encrypted_access = cipher
        .encrypt(&access_token)
        .unwrap_or_else(|_| access_token.clone());
    let encrypted_refresh = cipher
        .encrypt(&refresh_token)
        .unwrap_or_else(|_| refresh_token.clone());

    // Zapisz tokeny do addon secrets per user
    db::repository::set_addon_secret(
        pool,
        addon_id,
        Some(user_id),
        "oauth_token",
        &encrypted_access,
    )?;
    if !refresh_token.is_empty() {
        db::repository::set_addon_secret(
            pool,
            addon_id,
            Some(user_id),
            "refresh_token",
            &encrypted_refresh,
        )?;
    }

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(user_id),
        None,
        "addon.oauth.authorized",
        Some(addon_id),
        None,
        None,
        None,
    );

    // Redirect do dashboardu z komunikatem sukcesu
    let redirect_url = format!(
        "{}/#/addons?oauth_success={}&addon={}",
        base_url.trim_end_matches('/'),
        addon_id,
        addon_id
    );

    Ok((
        200,
        serde_json::json!({
            "redirect_url": redirect_url,
            "ok": true,
        })
        .to_string(),
    ))
}

// =============================================================================
// Network Rules API — reguly sieciowe addonow
// =============================================================================

/// GET /api/addons/{addon_id}/network-rules — lista regul sieciowych addonu
pub fn handle_get_network_rules(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
) -> Result<(u16, String)> {
    // Tylko admin moze przegladac reguly sieciowe
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let conn = pool.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT rule_id, protocol, host, port, description, required, approved, approved_by, approved_at \
         FROM addon_network_rules WHERE addon_id = ?1"
    )?;

    let rules: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(serde_json::json!({
                "rule_id": row.get::<_, String>(0)?,
                "protocol": row.get::<_, String>(1)?,
                "host": row.get::<_, String>(2)?,
                "port": row.get::<_, i32>(3)?,
                "description": row.get::<_, String>(4).unwrap_or_default(),
                "required": row.get::<_, i32>(5).unwrap_or(0) != 0,
                "approved": row.get::<_, i32>(6).unwrap_or(0) != 0,
                "approved_by": row.get::<_, Option<i64>>(7).unwrap_or(None),
                "approved_at": row.get::<_, Option<String>>(8).unwrap_or(None),
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Wszystkie reguly (TCP/UDP + HTTP domains) sa w jednej tabeli addon_network_rules
    Ok((
        200,
        serde_json::json!({
            "addon_id": addon_id,
            "network_rules": rules,
        })
        .to_string(),
    ))
}

/// PUT /api/addons/{addon_id}/network-rules/{rule_id}/approve — zatwierdzenie reguly sieciowej
pub fn handle_approve_network_rule(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    rule_id: &str,
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let conn = pool.lock().unwrap();

    // Sprawdz czy regula istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
            rusqlite::params![addon_id, rule_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !exists {
        return Ok((404, json_error("Regula sieciowa nie znaleziona")));
    }

    conn.execute(
        "UPDATE addon_network_rules SET approved = 1, approved_by = ?1, approved_at = datetime('now') \
         WHERE addon_id = ?2 AND rule_id = ?3",
        rusqlite::params![claims.user_id, addon_id, rule_id],
    )?;

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        None,
        "addon.network_rule.approve",
        Some(addon_id),
        Some(rule_id),
        None,
        None,
    );

    Ok((
        200,
        serde_json::json!({
            "ok": true,
            "addon_id": addon_id,
            "rule_id": rule_id,
            "approved": true,
        })
        .to_string(),
    ))
}

/// PUT /api/addons/{addon_id}/network-rules/{rule_id}/revoke — cofniecie zatwierdzenia reguly
pub fn handle_revoke_network_rule(
    pool: &DbPool,
    claims: &Claims,
    addon_id: &str,
    rule_id: &str,
) -> Result<(u16, String)> {
    if !is_admin(pool, claims) {
        return Ok((403, json_error("Brak uprawnień administratora")));
    }

    let conn = pool.lock().unwrap();

    // Sprawdz czy regula istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
            rusqlite::params![addon_id, rule_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !exists {
        return Ok((404, json_error("Regula sieciowa nie znaleziona")));
    }

    conn.execute(
        "UPDATE addon_network_rules SET approved = 0, approved_by = NULL, approved_at = NULL \
         WHERE addon_id = ?1 AND rule_id = ?2",
        rusqlite::params![addon_id, rule_id],
    )?;

    // VULN-048: TODO: Powiadom AddonManager o koniecznosci zamkniecia polaczen reguly.
    // Aktualnie polaczenia zostana zamkniete przy nastepnym uzyciu (send/recv sprawdza approved).
    tracing::warn!(
        "Regula '{}' addonu '{}' cofnieta — aktywne polaczenia zostana zamkniete przy nastepnym uzyciu",
        rule_id, addon_id
    );

    // Audit log
    let _ = db::repository::log_audit(
        pool,
        Some(claims.user_id),
        None,
        "addon.network_rule.revoke",
        Some(addon_id),
        Some(rule_id),
        None,
        None,
    );

    Ok((
        200,
        serde_json::json!({
            "ok": true,
            "addon_id": addon_id,
            "rule_id": rule_id,
            "approved": false,
        })
        .to_string(),
    ))
}

/// Wywoluje narzedzie addonu — dla meeting-bot wysyla komende QUIC do kontenera
pub fn handle_invoke_addon_tool(
    pool: &DbPool,
    addon_id: &str,
    tool_name: &str,
    body: &str,
    router: Option<&Arc<crate::routing::router::Router>>,
) -> Result<(u16, String)> {
    // Sprawdz czy addon istnieje
    let conn = pool
        .lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady DB: {}", e))?;
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);
    drop(conn);

    if !exists {
        return Ok((404, json_error("Addon nie znaleziony")));
    }

    let params: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::json!({}));

    // Dla meeting-bot wysylamy komende przez QUIC do kontenera
    if addon_id == "teams-bot" {
        // Reset transcript store przy kazdej zmianie meetingu zeby GUI Bot Status
        // nie mieszal danych z poprzednich. Diarization trackery sa per-meeting
        // z persistence — nie ma tu nic do resetowania, new meeting_id dostaje
        // swoj tracker automatycznie w identify_speaker_with_profiles.
        //
        // Teams-bot sam generuje meeting_id przy JoinMeeting i wysyla go jako
        // metadata.meeting_id w kazdym STT request → router wie ktory tracker
        // wziac z mapy active_trackers.
        //
        // Przy leave_meeting kontener czysci swoje current_meeting_id, wiec
        // kolejne STT requesty nie trafia juz do tego trackera.
        if tool_name == "join_meeting" || tool_name == "leave_meeting" {
            crate::routing::transcript_store::clear();
        }

        let router = match router {
            Some(r) => r,
            None => return Ok((500, json_error("Router nie dostepny"))),
        };

        let command = serde_json::json!({
            "tool": format!("{}.{}", addon_id, tool_name),
            "params": params,
        });

        // Wyslij przez service_request do kontenera
        let service_name = "tentaflow-meeting-bot";
        let request = tentaflow_protocol::ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: tentaflow_protocol::ModelPayload::Completion(
                tentaflow_protocol::CompletionPayload {
                    model: service_name.to_string(),
                    prompt: Some(command.to_string()),
                    ..Default::default()
                },
            ),
            stream: false,
            metadata: None,
            session_id: None,
        };

        // Znajdz QUIC handle i wyslij
        let quic_services = router.service_manager.quic_llm_services.read();
        if let Some(handle) = quic_services.get(service_name) {
            let handle = handle.clone();
            drop(quic_services);

            // Async → sync bridge (ten handler jest wolany z sync kontekstu)
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = handle.get_client().await;
                    match client {
                        Some(c) => c.send_request(request).await.map_err(|e| format!("{}", e)),
                        None => Err("Klient QUIC nie polaczony".to_string()),
                    }
                })
            });

            match result {
                Ok(response) => match response.result {
                    tentaflow_protocol::ModelResult::Completion(c) => Ok((
                        200,
                        serde_json::json!({
                            "ok": true,
                            "result": c.text,
                        })
                        .to_string(),
                    )),
                    tentaflow_protocol::ModelResult::Error(e) => Ok((500, json_error(&e.message))),
                    _ => Ok((200, serde_json::json!({"ok": true}).to_string())),
                },
                Err(e) => Ok((503, json_error(&format!("Kontener niedostepny: {}", e)))),
            }
        } else {
            Ok((
                503,
                json_error(
                    "Serwis meeting-bot nie jest polaczony. Zdeplojuj kontener z Service Catalog.",
                ),
            ))
        }
    } else {
        Ok((
            501,
            json_error(
                "Wywolywanie narzedzi addonow nie jest jeszcze zaimplementowane dla tego addonu",
            ),
        ))
    }
}
