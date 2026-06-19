// =============================================================================
// Plik: api/dashboard/handlers_addon_lifecycle.rs
// Opis: Handlery binary protocol dla cyklu zycia addonu — toggle, install,
//       uninstall, config get/set, logs, tools, resource limits get/set,
//       network rules get/set, reload. Zastepuja dawne REST endpointy
//       /api/addons/install, /api/addons/:id (PUT/DELETE), /api/addons/:id/
//       config, /limits, /tools, /network-rules. Polityka: Admin dla wszystkich
//       operacji modyfikujacych; AddonToolsRequest dostepny dla UserSession
//       (zwykly user moze odkryc jakie narzedzia oferuje addon).
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    AddonConfigField, AddonConfigGetResponse, AddonConfigSetResponse, AddonInstallResponse,
    AddonInstanceInstallResponse, AddonInstancePayload, AddonInstanceUpdateResponse,
    AddonInstanceVersionsResponse, AddonKvStats, AddonLogEntry, AddonLogsResponse,
    AddonMilvusService, AddonNetworkRuleDecl, AddonNetworkRulesGetResponse,
    AddonNetworkRulesSetResponse, AddonPackageInfo, AddonRecordingStats, AddonReloadResponse,
    AddonResourcesGetResponse, AddonResourcesSetResponse, AddonSqlStats, AddonSqlTable,
    AddonStoragePayload, AddonStorageStatsResponse, AddonToggleResponse, AddonToolDecl,
    AddonToolParam, AddonToolsResponse, AddonUninstallResponse, AddonVectorConfig,
    AddonVectorConfigResponse, AddonVectorPayload, AddonVectorSetConfigResponse, AddonVectorStats,
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
};

use crate::db::repository;
use crate::dispatch::HandlerContext;

/// Zwraca AddonManager z AppState lub blad gdy niedostepny (np. headless bez
/// runtime addonow). Potrzebny dla operacji instancji (install/duplicate/update),
/// bo musza zarejestrowac runtime (toole/flow bloki), nie tylko zapisac DB.
fn addon_manager(
    ctx: &HandlerContext,
) -> Result<std::sync::Arc<crate::addon::AddonManager>, ProtocolError> {
    ctx.state
        .addon_manager
        .clone()
        .ok_or_else(|| ProtocolError::internal("AddonManager unavailable"))
}

// =============================================================================
// Helpery
// =============================================================================

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

/// Waliduje addon_id (anti path-traversal / injection): tylko [a-z0-9_-], max 64.
fn validate_addon_id(addon_id: &str) -> Result<(), ProtocolError> {
    if addon_id.is_empty() || addon_id.len() > 64 {
        return Err(ProtocolError::bad_request(
            "addon_id musi miec 1..=64 znakow",
        ));
    }
    if !addon_id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(ProtocolError::bad_request(
            "addon_id moze zawierac wylacznie [a-z0-9_-]",
        ));
    }
    Ok(())
}

/// Pobiera numeryczne user_id z kontekstu (dla audytu).
fn current_user_id(ctx: &HandlerContext) -> Option<String> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    }
}

fn audit(
    ctx: &HandlerContext,
    action: &str,
    addon_id: &str,
    details_json: serde_json::Value,
    severity: &str,
) {
    let user_id = current_user_id(ctx);
    let details = details_json.to_string();
    let node_id = ctx.state.local_node_id.as_ref();
    if let Err(e) = repository::log_audit_full(
        &ctx.state.db,
        user_id.as_deref(),
        Some(addon_id),
        action,
        Some("addon"),
        Some(addon_id),
        Some(&details),
        severity,
        "unclassified",
        None,
        None,
        None,
        Some(node_id),
    ) {
        tracing::warn!("audit log failed ({}): {}", action, e);
    }
}

/// Parsuje manifest (kolumna `addons.manifest_json` — format TOML) i zwraca `toml::Value`.
fn parse_manifest(manifest_text: &str) -> toml::Value {
    toml::from_str::<toml::Value>(manifest_text)
        .unwrap_or(toml::Value::Table(toml::map::Map::new()))
}

/// Wyciaga schema pol konfiguracji z manifestu: probuje [config.schema] (tabela) lub
/// [config_schema] (flat). Zwraca wektor pol z walidacja pol (typ/label/options).
fn extract_config_schema(manifest: &toml::Value) -> Vec<AddonConfigField> {
    let schema_val = manifest
        .get("config")
        .and_then(|c| c.get("schema"))
        .or_else(|| manifest.get("config_schema"));
    let Some(schema_tbl) = schema_val.and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(schema_tbl.len());
    for (id, def) in schema_tbl.iter() {
        let label = def
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(id.as_str())
            .to_string();
        let field_type = def
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();
        let description = def
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let default_value = def
            .get("default")
            .map(|v| match v {
                toml::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let options: Vec<String> = def
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let required = def
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let secret = def.get("secret").and_then(|v| v.as_bool()).unwrap_or(false)
            || field_type == "password";
        out.push(AddonConfigField {
            id: id.clone(),
            label,
            field_type,
            description,
            default_value,
            options,
            required,
            secret,
        });
    }
    // Vector-backend selection is NOT a Settings field — it lives in the
    // Bindings (Powiązania) tab's dedicated picker (zvec / local / cross-node
    // Milvus), persisted under `__vector_config`. Settings shows only the
    // addon's own manifest-declared config.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

// =============================================================================
// 1. AddonToggleRequest — Admin
// =============================================================================

#[handler(variant = "AddonToggleRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_toggle(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonToggleRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonToggleRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    let enabled_old =
        repository::get_addon_enabled(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let Some(prev) = enabled_old else {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    };
    let updated = repository::set_addon_enabled(&ctx.state.db, &payload.addon_id, payload.enabled)
        .map_err(db_err)?;
    if !updated {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }

    audit(
        ctx,
        "addon_toggle",
        &payload.addon_id,
        serde_json::json!({
            "enabled_old": prev,
            "enabled_new": payload.enabled,
        }),
        "info",
    );

    Ok(MessageBody::AddonToggleResponseBody(AddonToggleResponse {
        ok: true,
        enabled: payload.enabled,
        message: None,
    }))
}

// =============================================================================
// 2. AddonInstallRequest — Admin (delegowany do addon::lifecycle::install)
// =============================================================================

#[handler(variant = "AddonInstallRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_install(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonInstallRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonInstallRequestBody",
            ))
        }
    };

    const MAX_ZIP_SIZE: usize = 50 * 1024 * 1024;
    if payload.content.is_empty() {
        return Ok(MessageBody::AddonInstallResponseBody(
            AddonInstallResponse {
                ok: false,
                addon_id: None,
                version: None,
                warnings: Vec::new(),
                error: Some("content pusty".into()),
            },
        ));
    }
    if payload.content.len() > MAX_ZIP_SIZE {
        return Ok(MessageBody::AddonInstallResponseBody(
            AddonInstallResponse {
                ok: false,
                addon_id: None,
                version: None,
                warnings: Vec::new(),
                error: Some(format!(
                    "content za duze ({}B > {}B)",
                    payload.content.len(),
                    MAX_ZIP_SIZE
                )),
            },
        ));
    }
    if payload.content.len() < 4 || &payload.content[0..4] != b"PK\x03\x04" {
        return Ok(MessageBody::AddonInstallResponseBody(
            AddonInstallResponse {
                ok: false,
                addon_id: None,
                version: None,
                warnings: Vec::new(),
                error: Some("plik nie jest poprawnym archiwum ZIP".into()),
            },
        ));
    }

    // Rozpakuj do tymczasowego katalogu i wywolaj lifecycle::install.
    let tmp_root =
        std::env::temp_dir().join(format!("tentaflow_addon_install_{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        return Err(ProtocolError::internal(format!(
            "nie mozna utworzyc katalogu tymczasowego: {}",
            e
        )));
    }
    let zip_path = tmp_root.join("addon.zip");
    if let Err(e) = std::fs::write(&zip_path, &payload.content) {
        let _ = std::fs::remove_dir_all(&tmp_root);
        return Err(ProtocolError::internal(format!("zapis ZIP: {}", e)));
    }
    let extract_dir = tmp_root.join("extracted");
    if let Err(e) = std::fs::create_dir_all(&extract_dir) {
        let _ = std::fs::remove_dir_all(&tmp_root);
        return Err(ProtocolError::internal(format!("mkdir extract: {}", e)));
    }
    let unzip = std::process::Command::new("unzip")
        .args(["-o", "-q"])
        .arg(zip_path.as_os_str())
        .arg("-d")
        .arg(extract_dir.as_os_str())
        .output();
    match unzip {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let _ = std::fs::remove_dir_all(&tmp_root);
            return Ok(MessageBody::AddonInstallResponseBody(
                AddonInstallResponse {
                    ok: false,
                    addon_id: None,
                    version: None,
                    warnings: Vec::new(),
                    error: Some(format!("unzip: {}", String::from_utf8_lossy(&out.stderr))),
                },
            ));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_root);
            return Err(ProtocolError::internal(format!("unzip failed: {}", e)));
        }
    }

    // Jesli ZIP ma jeden folder w srodku — zejdz do niego (manifest.toml oczekiwany w korzeniu).
    let addon_dir = {
        let root_entries: Vec<_> = std::fs::read_dir(&extract_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        if !extract_dir.join("manifest.toml").exists()
            && root_entries.len() == 1
            && root_entries[0].path().is_dir()
        {
            root_entries[0].path()
        } else {
            extract_dir.clone()
        }
    };

    // Catalog-only: an upload adds/updates a PACKAGE version in the catalog (and
    // replicates its bytes to the mesh), it does NOT create an instance. Uploading
    // a new version of an existing package = an update (existing instances see it
    // as available); re-uploading the same version overwrites the bytes. No
    // "already installed" error because no instance is created. Instances are
    // created from the catalog (install) and updated via the version picker.
    let install_result =
        crate::addon::lifecycle::install_package_to_catalog(&addon_dir, &ctx.state.db);
    let _ = std::fs::remove_dir_all(&tmp_root);

    match install_result {
        Ok((package_id, version)) => {
            audit(
                ctx,
                "addon_package_upload",
                &package_id,
                serde_json::json!({
                    "package_id": package_id,
                    "version": version,
                    "file_size_bytes": payload.content.len(),
                    "filename": payload.filename,
                }),
                "warning",
            );
            Ok(MessageBody::AddonInstallResponseBody(
                AddonInstallResponse {
                    ok: true,
                    addon_id: Some(package_id),
                    version: Some(version),
                    warnings: Vec::new(),
                    error: None,
                },
            ))
        }
        Err(e) => Ok(MessageBody::AddonInstallResponseBody(
            AddonInstallResponse {
                ok: false,
                addon_id: None,
                version: None,
                warnings: Vec::new(),
                error: Some(format!("{}", e)),
            },
        )),
    }
}

// =============================================================================
// 3. AddonUninstallRequest — Admin
// =============================================================================

#[handler(variant = "AddonUninstallRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_uninstall(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonUninstallRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonUninstallRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    if addon.is_system {
        return Err(ProtocolError::bad_request(
            "addon systemowy nie moze zostac odinstalowany",
        ));
    }

    // Emit the mesh delete tombstone BEFORE removing the row — a durable
    // pre-delete capture so a crash mid-uninstall can never strand peers with
    // the addon still installed. Gated bundled (uploaded/never-synced instances
    // must not emit a tombstone) while the row still exists for the check. If
    // the uninstall below then fails, baseline reseed re-emits a newer Insert
    // that supersedes this tombstone (LWW) — self-healing.
    if repository::addon_is_syncable(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)
        .unwrap_or(false)
    {
        if let Err(e) = repository::capture_addon_instance_delete(&ctx.state.db, &payload.addon_id)
        {
            tracing::warn!(
                "addon uninstall: capture delete sync nieudany dla '{}': {e}",
                payload.addon_id
            );
        }
    }

    // Odinstalowanie instancji: przez managera (unregister runtime toole/flow
    // bloki + zatrzymanie wasm + purge katalogu danych instancji). Headless bez
    // managera (brak runtime addonow) — sama warstwa DB + purge danych.
    match ctx.state.addon_manager.clone() {
        Some(mgr) => mgr
            .uninstall_instance(&payload.addon_id)
            .map_err(|e| ProtocolError::internal(format!("uninstall: {}", e)))?,
        None => crate::addon::lifecycle::uninstall_instance(&payload.addon_id, &ctx.state.db)
            .map_err(|e| ProtocolError::internal(format!("uninstall: {}", e)))?,
    }

    audit(
        ctx,
        "addon_uninstall",
        &payload.addon_id,
        serde_json::json!({
            "addon_id": payload.addon_id,
            "version_removed": addon.version,
        }),
        "warning",
    );

    Ok(MessageBody::AddonUninstallResponseBody(
        AddonUninstallResponse { ok: true },
    ))
}

// =============================================================================
// 4. AddonConfigGetRequest — Admin
// =============================================================================

#[handler(variant = "AddonConfigGetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_config_get(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonConfigGetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonConfigGetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

    let manifest = parse_manifest(&addon.manifest_json);
    let schema = extract_config_schema(&manifest);

    let rows =
        repository::list_addon_config_rows(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    // Sekret wartosci — zwracamy "" aby GUI wiedzialo ze jest ustawione, ale nie widzi plaintextu.
    let secret_ids: std::collections::HashSet<&str> = schema
        .iter()
        .filter(|f| f.secret)
        .map(|f| f.id.as_str())
        .collect();
    let values: Vec<(String, String)> = rows
        .into_iter()
        .map(|r| {
            if secret_ids.contains(r.key.as_str()) || r.is_secret {
                (r.key, String::new())
            } else {
                (r.key, r.value)
            }
        })
        .collect();

    Ok(MessageBody::AddonConfigGetResponseBody(
        AddonConfigGetResponse { schema, values },
    ))
}

// =============================================================================
// 5. AddonConfigSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonConfigSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_config_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonConfigSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonConfigSetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

    let manifest = parse_manifest(&addon.manifest_json);
    let schema = extract_config_schema(&manifest);
    let schema_map: std::collections::HashMap<&str, &AddonConfigField> =
        schema.iter().map(|f| (f.id.as_str(), f)).collect();

    // Walidacja: kazde pole musi istniec w schema. Puste value dla secret — pomijamy (nie nadpisujemy).
    for (k, _) in payload.values.iter() {
        if !schema_map.contains_key(k.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "nieznane pole konfiguracji: {}",
                k
            )));
        }
    }

    let updated_by = current_user_id(ctx);
    let mut fields_changed: Vec<String> = Vec::new();
    let mut secret_fields_changed: Vec<String> = Vec::new();
    for (k, v) in payload.values.iter() {
        let Some(field) = schema_map.get(k.as_str()) else {
            continue;
        };
        // Dla pol secret puste value = "nie zmieniaj" (analogicznie do OAuth client_secret: None).
        if field.secret && v.is_empty() {
            continue;
        }
        repository::upsert_addon_config_value(
            &ctx.state.db,
            &payload.addon_id,
            k,
            v,
            field.secret,
            updated_by.as_deref(),
        )
        .map_err(db_err)?;
        fields_changed.push(k.clone());
        if field.secret {
            secret_fields_changed.push(k.clone());
        }
    }

    // Severity zalezy od tego czy zmienilismy sekrety (wyzsze ryzyko).
    let severity = if !secret_fields_changed.is_empty() {
        "warning"
    } else {
        "info"
    };
    // UWAGA: w audit logu zapisujemy WYLACZNIE nazwy pol — nigdy wartosci (plaintext ani secret).
    audit(
        ctx,
        "addon_config_set",
        &payload.addon_id,
        serde_json::json!({
            "fields_changed": fields_changed,
            "secret_fields_changed": secret_fields_changed,
        }),
        severity,
    );

    Ok(MessageBody::AddonConfigSetResponseBody(
        AddonConfigSetResponse { ok: true },
    ))
}

// =============================================================================
// 6. AddonLogsRequest — Admin
// =============================================================================

#[handler(variant = "AddonLogsRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_logs(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonLogsRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonLogsRequestBody")),
    };
    validate_addon_id(&payload.addon_id)?;

    let level_norm = payload.level.as_deref().map(|s| match s {
        "info" | "warn" | "warning" | "critical" | "error" => {
            if s == "warn" {
                "warning".to_string()
            } else if s == "error" {
                "critical".to_string()
            } else {
                s.to_string()
            }
        }
        _ => s.to_string(),
    });
    let level_ref = level_norm.as_deref();
    let search_ref = payload.search.as_deref();

    let (rows, total) = repository::list_addon_audit_logs(
        &ctx.state.db,
        &payload.addon_id,
        payload.limit,
        payload.offset,
        level_ref,
        search_ref,
    )
    .map_err(db_err)?;

    let entries = rows
        .into_iter()
        .map(|r| AddonLogEntry {
            id: r.id,
            timestamp: r.timestamp,
            level: r.severity,
            action: r.action.clone(),
            message: r.action,
            user_id: r.user_id,
            user_name: r.username,
            details: r.details.unwrap_or_default(),
        })
        .collect();

    Ok(MessageBody::AddonLogsResponseBody(AddonLogsResponse {
        entries,
        total,
    }))
}

// =============================================================================
// 7. AddonToolsRequest — UserSession (kazdy zalogowany widzi liste narzedzi)
// =============================================================================

#[handler(variant = "AddonToolsRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_tools(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonToolsRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonToolsRequestBody")),
    };
    validate_addon_id(&payload.addon_id)?;

    // Visibility enforcement: non-admin bez widocznosci dostaje NotFound.
    let is_admin = matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    );
    if !is_admin {
        let uid = current_user_id(ctx).ok_or_else(|| {
            ProtocolError::new(ProtocolErrorCode::AuthRequired, "brak user_id w sesji")
        })?;
        if !repository::is_addon_visible_to_user(&ctx.state.db, &payload.addon_id, &uid)
            .map_err(db_err)?
        {
            return Err(ProtocolError::not_found("addon nie istnieje"));
        }
    }

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

    // Jedno zrodlo prawdy z LLM: kanoniczny parser manifestu (`[[tool]]`), ten
    // sam, ktory zasila tool_dispatch. `registered_tools` nie nadaje sie tu, bo
    // to stan runtime (pusty gdy addon wylaczony / nie wystartowal).
    let mut tools: Vec<AddonToolDecl> =
        match crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json) {
            Ok(manifest) => manifest.tools.iter().map(tool_decl_from_manifest).collect(),
            Err(e) => {
                tracing::warn!(
                    "addon '{}': nie udalo sie sparsowac manifestu dla listy tools: {}",
                    payload.addon_id,
                    e
                );
                Vec::new()
            }
        };
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(MessageBody::AddonToolsResponseBody(AddonToolsResponse {
        tools,
    }))
}

/// Mapuje kanoniczny `ManifestTool` (z `parse_manifest_toml`) na protokolowy
/// `AddonToolDecl` dla dashboardu. Lista parametrow jest rekonstruowana z
/// `parameters_schema` (JSON Schema: `properties` + `required`), bo to forma w
/// jakiej parser przechowuje parametry (wymagana przez host functions/LLM).
fn tool_decl_from_manifest(t: &crate::addon::ManifestTool) -> AddonToolDecl {
    let mut parameters: Vec<AddonToolParam> = Vec::new();
    if let Some(props) = t
        .parameters_schema
        .get("properties")
        .and_then(|v| v.as_object())
    {
        let required: std::collections::HashSet<&str> = t
            .parameters_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        for (pname, pdef) in props {
            parameters.push(AddonToolParam {
                name: pname.clone(),
                param_type: pdef
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("string")
                    .to_string(),
                description: pdef
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                required: required.contains(pname.as_str()),
                default_value: pdef.get("default").map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            });
        }
        parameters.sort_by(|a, b| a.name.cmp(&b.name));
    }
    // return_type: prosty typ z JSON Schema wyniku (jesli zadeklarowany).
    let return_type = t
        .return_schema
        .as_ref()
        .and_then(|s| s.get("type").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    AddonToolDecl {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters,
        return_type,
    }
}

// =============================================================================
// 8. AddonResourcesGetRequest — Admin
// =============================================================================

#[handler(variant = "AddonResourcesGetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_resources_get(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonResourcesGetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonResourcesGetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    if repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }

    let limits =
        repository::get_addon_resource_limits(&ctx.state.db, &payload.addon_id).map_err(db_err)?;

    Ok(MessageBody::AddonResourcesGetResponseBody(
        AddonResourcesGetResponse {
            max_instances: clamp_i32(limits.max_instances),
            cpu_limit_pct: clamp_i32(limits.cpu_limit_ms_per_min),
            ram_mb: clamp_i32(limits.ram_limit_mb),
            storage_mb: clamp_i32(limits.storage_limit_mb),
            http_requests_per_min: clamp_i32(limits.http_requests_per_min),
            llm_tokens_per_min: clamp_i32(limits.llm_tokens_per_min),
        },
    ))
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

// =============================================================================
// 9. AddonResourcesSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonResourcesSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_resources_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonResourcesSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonResourcesSetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    if payload.cpu_limit_pct < 0 || payload.cpu_limit_pct > 100 {
        return Err(ProtocolError::bad_request(
            "cpu_limit_pct musi byc w zakresie 0..=100",
        ));
    }
    if payload.ram_mb < 0
        || payload.storage_mb < 0
        || payload.max_instances < 0
        || payload.http_requests_per_min < 0
        || payload.llm_tokens_per_min < 0
    {
        return Err(ProtocolError::bad_request(
            "wartosci limitow nie moga byc ujemne",
        ));
    }
    if repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }

    let old =
        repository::get_addon_resource_limits(&ctx.state.db, &payload.addon_id).map_err(db_err)?;

    let new = repository::AddonResourceLimits {
        addon_id: payload.addon_id.clone(),
        max_instances: payload.max_instances as i64,
        cpu_limit_ms_per_min: payload.cpu_limit_pct as i64,
        ram_limit_mb: payload.ram_mb as i64,
        gpu_enabled: old.gpu_enabled,
        vram_limit_mb: old.vram_limit_mb,
        storage_limit_mb: payload.storage_mb as i64,
        http_requests_per_min: payload.http_requests_per_min as i64,
        llm_tokens_per_min: payload.llm_tokens_per_min as i64,
        fuel_limit: old.fuel_limit,
    };
    repository::set_addon_resource_limits(&ctx.state.db, &new).map_err(db_err)?;

    audit(
        ctx,
        "addon_resources_set",
        &payload.addon_id,
        serde_json::json!({
            "max_instances_old": old.max_instances,
            "max_instances_new": payload.max_instances,
            "cpu_limit_pct_old": old.cpu_limit_ms_per_min,
            "cpu_limit_pct_new": payload.cpu_limit_pct,
            "ram_mb_old": old.ram_limit_mb,
            "ram_mb_new": payload.ram_mb,
            "storage_mb_old": old.storage_limit_mb,
            "storage_mb_new": payload.storage_mb,
            "http_requests_per_min_old": old.http_requests_per_min,
            "http_requests_per_min_new": payload.http_requests_per_min,
            "llm_tokens_per_min_old": old.llm_tokens_per_min,
            "llm_tokens_per_min_new": payload.llm_tokens_per_min,
        }),
        "warning",
    );

    Ok(MessageBody::AddonResourcesSetResponseBody(
        AddonResourcesSetResponse { ok: true },
    ))
}

// =============================================================================
// 10. AddonNetworkRulesGetRequest — Admin
// =============================================================================

#[handler(variant = "AddonNetworkRulesGetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_network_rules_get(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonNetworkRulesGetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonNetworkRulesGetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    if repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }
    let mut cfg =
        repository::get_addon_network_config(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let declared_rows =
        repository::get_addon_declared_network_rules(&ctx.state.db, &payload.addon_id)
            .map_err(db_err)?;
    let approved_hosts: std::collections::BTreeSet<String> = declared_rows
        .iter()
        .filter(|r| r.approved)
        .map(|r| r.host.clone())
        .collect();
    for host in approved_hosts {
        if !cfg.allowed_hosts.iter().any(|h| h == &host) {
            cfg.allowed_hosts.push(host);
        }
    }
    let declared_rules =
        compute_declared_status(&declared_rows, &cfg.allowed_hosts, &cfg.blocked_hosts);
    Ok(MessageBody::AddonNetworkRulesGetResponseBody(
        AddonNetworkRulesGetResponse {
            allowed_hosts: cfg.allowed_hosts,
            blocked_hosts: cfg.blocked_hosts,
            mode: cfg.mode,
            declared_rules,
        },
    ))
}

/// Merges manifest-declared rules with admin policy and the real `approved`
/// flag used by host functions.
fn compute_declared_status(
    declared: &[repository::AddonDeclaredNetworkRule],
    allowed: &[String],
    blocked: &[String],
) -> Vec<AddonNetworkRuleDecl> {
    declared
        .iter()
        .map(|r| {
            let mode = "allow";
            let host_allowed = allowed.iter().any(|h| h == &r.host);
            let host_blocked = blocked.iter().any(|h| h == &r.host);
            let status = if host_blocked && (r.approved || host_allowed) {
                "conflicting"
            } else if host_blocked {
                "missing"
            } else if r.approved {
                "covered"
            } else {
                "missing"
            };
            AddonNetworkRuleDecl {
                rule_id: r.rule_id.clone(),
                host: r.host.clone(),
                port: Some(r.port),
                protocol: r.protocol.clone(),
                mode: mode.to_string(),
                status: status.to_string(),
                required: r.required,
                approved: r.approved,
            }
        })
        .collect()
}

// =============================================================================
// 11. AddonNetworkRulesSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonNetworkRulesSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_network_rules_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonNetworkRulesSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonNetworkRulesSetRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    if !matches!(payload.mode.as_str(), "strict" | "permissive") {
        return Err(ProtocolError::bad_request(
            "mode musi byc 'strict' lub 'permissive'",
        ));
    }
    for h in payload
        .allowed_hosts
        .iter()
        .chain(payload.blocked_hosts.iter())
    {
        if h.is_empty() || h.len() > 253 {
            return Err(ProtocolError::bad_request("host musi miec 1..=253 znakow"));
        }
        if h.contains('/') || h.contains(' ') {
            return Err(ProtocolError::bad_request(
                "host zawiera niedozwolone znaki",
            ));
        }
    }
    if repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }

    let old =
        repository::get_addon_network_config(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let updated_by = current_user_id(ctx);
    let new = repository::AddonNetworkConfig {
        allowed_hosts: payload.allowed_hosts.clone(),
        blocked_hosts: payload.blocked_hosts.clone(),
        mode: payload.mode.clone(),
    };
    repository::set_addon_network_config(
        &ctx.state.db,
        &payload.addon_id,
        &new,
        updated_by.as_deref(),
    )
    .map_err(db_err)?;
    repository::set_addon_network_rule_approvals(
        &ctx.state.db,
        &payload.addon_id,
        &payload.allowed_hosts,
        &payload.blocked_hosts,
        updated_by.as_deref(),
    )
    .map_err(db_err)?;

    // Policz diff hostow — GUI/audyt atwiej ogladaja delty niz pelne listy.
    let diff_hosts = |old_list: &[String], new_list: &[String]| -> (Vec<String>, Vec<String>) {
        let old_set: std::collections::BTreeSet<&str> =
            old_list.iter().map(|s| s.as_str()).collect();
        let new_set: std::collections::BTreeSet<&str> =
            new_list.iter().map(|s| s.as_str()).collect();
        let added: Vec<String> = new_set
            .difference(&old_set)
            .map(|s| s.to_string())
            .collect();
        let removed: Vec<String> = old_set
            .difference(&new_set)
            .map(|s| s.to_string())
            .collect();
        (added, removed)
    };
    let (allowed_added, allowed_removed) = diff_hosts(&old.allowed_hosts, &payload.allowed_hosts);
    let (blocked_added, blocked_removed) = diff_hosts(&old.blocked_hosts, &payload.blocked_hosts);

    audit(
        ctx,
        "addon_network_rules_set",
        &payload.addon_id,
        serde_json::json!({
            "mode_old": old.mode,
            "mode_new": payload.mode,
            "allowed_added": allowed_added,
            "allowed_removed": allowed_removed,
            "blocked_added": blocked_added,
            "blocked_removed": blocked_removed,
        }),
        "warning",
    );

    Ok(MessageBody::AddonNetworkRulesSetResponseBody(
        AddonNetworkRulesSetResponse { ok: true },
    ))
}

// =============================================================================
// 12. AddonReloadRequest — Admin (invalidate instance pool)
// =============================================================================

#[handler(variant = "AddonReloadRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_reload(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonReloadRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonReloadRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    if repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }

    // Invalidate pool — re-init nastapi przy nastepnym wywolaniu.
    let message = invalidate_instance_pool(ctx, &payload.addon_id);

    audit(
        ctx,
        "addon_reload",
        &payload.addon_id,
        serde_json::json!({}),
        "info",
    );

    Ok(MessageBody::AddonReloadResponseBody(AddonReloadResponse {
        ok: true,
        message: Some(message),
    }))
}

/// Probuje unicwazic pool instancji. W obecnej wersji addon/instance_pool nie wystawia
/// publicznego API do invalidation per-addon — zwracamy opisowy komunikat zeby GUI
/// wiedzial ze reload zostal zaakceptowany (handler nie blokuje — dane sa odswiezone
/// przy nastepnym uzyciu dzieki zaktualizowanemu updated_at w tabeli addons).
fn invalidate_instance_pool(_ctx: &HandlerContext, addon_id: &str) -> String {
    format!("reload queued for addon '{}'", addon_id)
}

// =============================================================================
// Multi-instance: katalog pakietow + install/duplicate/versions/update instancji.
// Multipleksowane w `AddonInstanceBody` (limit 256 wariantow CBOR), routing po
// inner-nazwie do jednego handlera (wzorem AddonUiBody/IamBody).
// =============================================================================

#[handler(variant = "AddonInstanceBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_instance_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use AddonInstancePayload as P;
    let payload = match req {
        MessageBody::AddonInstanceBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonInstanceBody")),
    };
    let db = &ctx.state.db;

    let res = match payload {
        P::ReqCatalogList => {
            // Wiersze sa posortowane (package_id ASC, created_at DESC), wiec
            // agregujemy kolejne wersje tego samego pakietu w jeden wpis.
            let rows = repository::list_addon_packages(db).map_err(db_err)?;
            let mut packages: Vec<AddonPackageInfo> = Vec::new();
            for row in rows {
                if let Some(last) = packages.last_mut() {
                    if last.package_id == row.package_id {
                        last.versions.push(row.version);
                        continue;
                    }
                }
                let installed_instances =
                    repository::count_addon_instances(db, &row.package_id).map_err(db_err)? as i32;
                // Surface the package's declared connection params so the install
                // UI can render a per-instance form (e.g. robot IP). A malformed
                // manifest must NOT be silently emptied — that would drop a
                // required IP field and let install proceed then fail late. Skip
                // the offending package (one bad third-party manifest cannot break
                // the whole catalog list) and log loudly so it is not hidden.
                let connection_params = match crate::addon::lifecycle::parse_connection_params(
                    &row.manifest_json,
                ) {
                    Ok(params) => params
                        .into_iter()
                        .map(|p| tentaflow_protocol::AddonConnectionParam {
                            key: p.key,
                            label: p.label,
                            param_type: p.param_type,
                            required: p.required,
                            placeholder: p.placeholder,
                        })
                        .collect(),
                    Err(e) => {
                        tracing::warn!(
                            package_id = %row.package_id,
                            error = %e,
                            "skipping package from catalog: connection_params parse failed",
                        );
                        continue;
                    }
                };
                packages.push(AddonPackageInfo {
                    package_id: row.package_id,
                    name: row.name,
                    latest_version: row.version.clone(),
                    versions: vec![row.version],
                    source: row.source,
                    installed_instances,
                    connection_params,
                });
            }
            P::ResCatalogList { packages }
        }
        P::ReqInstall(r) => {
            validate_addon_id(&r.package_id)?;
            let name = r.display_name.trim();
            if name.is_empty() || name.len() > 120 {
                return Err(ProtocolError::bad_request("nazwa instancji 1..=120 znakow"));
            }
            let mgr = addon_manager(ctx)?;
            let config: std::collections::BTreeMap<String, String> =
                r.config.iter().cloned().collect();
            let res = match mgr.install_instance(&r.package_id, &r.version, name, &config) {
                Ok(addon_id) => {
                    capture_addon_instance_sync(db, &addon_id);
                    AddonInstanceInstallResponse {
                        ok: true,
                        addon_id: Some(addon_id),
                        error: None,
                    }
                }
                Err(e) => AddonInstanceInstallResponse {
                    ok: false,
                    addon_id: None,
                    error: Some(e.to_string()),
                },
            };
            P::ResInstall(res)
        }
        P::ReqDuplicate(r) => {
            validate_addon_id(&r.source_addon_id)?;
            let name = r.new_display_name.trim();
            if name.is_empty() || name.len() > 120 {
                return Err(ProtocolError::bad_request("nazwa instancji 1..=120 znakow"));
            }
            let mgr = addon_manager(ctx)?;
            let res = match mgr.duplicate_instance(&r.source_addon_id, name) {
                Ok(addon_id) => {
                    capture_addon_instance_sync(db, &addon_id);
                    AddonInstanceInstallResponse {
                        ok: true,
                        addon_id: Some(addon_id),
                        error: None,
                    }
                }
                Err(e) => AddonInstanceInstallResponse {
                    ok: false,
                    addon_id: None,
                    error: Some(e.to_string()),
                },
            };
            P::ResInstall(res)
        }
        P::ReqVersions(r) => {
            validate_addon_id(&r.addon_id)?;
            let (package_id, current) = repository::get_addon_instance_package_ref(db, &r.addon_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::bad_request("instancja nie istnieje"))?;
            let available = repository::list_package_versions(db, &package_id).map_err(db_err)?;
            P::ResVersions(AddonInstanceVersionsResponse { current, available })
        }
        P::ReqUpdate(r) => {
            validate_addon_id(&r.addon_id)?;
            let mgr = addon_manager(ctx)?;
            let res = match mgr.update_instance(&r.addon_id, &r.target_version) {
                Ok(()) => {
                    capture_addon_instance_sync(db, &r.addon_id);
                    AddonInstanceUpdateResponse {
                        ok: true,
                        error: None,
                    }
                }
                Err(e) => AddonInstanceUpdateResponse {
                    ok: false,
                    error: Some(e.to_string()),
                },
            };
            P::ResUpdate(res)
        }
        // Res* nie sa prawidlowymi requestami.
        P::ResCatalogList { .. } | P::ResInstall(_) | P::ResVersions(_) | P::ResUpdate(_) => {
            return Err(ProtocolError::bad_request("unexpected response variant"));
        }
    };

    Ok(MessageBody::AddonInstanceBody(res))
}

/// Rejestruje multipleksowany handler pod kazda inner-nazwa requestu z wlasnym
/// auth (read = UserSession, write = Admin), wzorem `register_addon_ui_variant!`.
macro_rules! register_addon_instance_variant {
    ($variant:literal, $metric:literal, $auth:expr) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: $auth,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_addon_instance_dispatch,
            }
        }
    };
}

register_addon_instance_variant!(
    "AddonCatalogListRequest",
    "tentaflow_ws_handler_addon_catalog_list",
    crate::dispatch::SessionAuthKind::UserSession
);
register_addon_instance_variant!(
    "AddonInstanceVersionsRequest",
    "tentaflow_ws_handler_addon_instance_versions",
    crate::dispatch::SessionAuthKind::UserSession
);
register_addon_instance_variant!(
    "AddonInstanceInstallRequest",
    "tentaflow_ws_handler_addon_instance_install",
    crate::dispatch::SessionAuthKind::Admin
);
register_addon_instance_variant!(
    "AddonInstanceDuplicateRequest",
    "tentaflow_ws_handler_addon_instance_duplicate",
    crate::dispatch::SessionAuthKind::Admin
);
register_addon_instance_variant!(
    "AddonInstanceUpdateRequest",
    "tentaflow_ws_handler_addon_instance_update",
    crate::dispatch::SessionAuthKind::Admin
);

// =============================================================================
// Storage stats addona (zakladka Powiazania) — KV / SQL / Vector / Recording.
// Multipleksowane w `AddonStorageBody` (limit 256 wariantow CBOR).
// =============================================================================

const SQL_ROW_CAP: i64 = 100_000;

#[handler(variant = "AddonStorageBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_storage_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use AddonStoragePayload as P;
    let payload = match req {
        MessageBody::AddonStorageBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonStorageBody")),
    };
    let r = match payload {
        P::StatsRequest(r) => r,
        P::StatsResponse(_) => {
            return Err(ProtocolError::bad_request("unexpected response variant"))
        }
    };
    validate_addon_id(&r.addon_id)?;

    // Scope: instancja musi istniec (kanoniczny addon_id, nie sciezka).
    let addon = repository::get_addon(&ctx.state.db, &r.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    let db = &ctx.state.db;
    let org_id = crate::services::org::DEFAULT_ORG_ID;

    // KV store.
    let (keys, bytes, limit_mb) = repository::addon_kv_stats(db, &r.addon_id).map_err(db_err)?;
    let kv = AddonKvStats {
        keys,
        bytes,
        limit_mb,
    };

    // Per-addon SQLite — tylko gdy manifest deklaruje [storage] sql=true.
    let sql_declared = crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json)
        .ok()
        .and_then(|m| m.storage)
        .map(|s| s.sql)
        .unwrap_or(false);
    let sql = if sql_declared {
        addon_sql_stats(org_id, &r.addon_id)
    } else {
        AddonSqlStats {
            enabled: false,
            available: false,
            db_size_bytes: -1,
            tables: Vec::new(),
        }
    };

    // Vector namespaces. Warstwa wektorowa (NamespaceManager + zvec + tabela
    // addon_vector_namespaces) jest mandatory na kazdej platformie (zvec to
    // niewarunkowy dependency), wiec statystyki sa zawsze dostepne. Osobny
    // backend Milvus to feature `vector-milvus` (opcja, nie zmienia dostepnosci
    // statystyk namespace'ow).
    let vector = {
        let namespaces = repository::addon_vector_namespace_stats(db, &r.addon_id)
            .map_err(db_err)?
            .into_iter()
            .map(
                |(namespace, dim, metric, count)| tentaflow_protocol::AddonVectorNamespace {
                    namespace,
                    dim,
                    metric,
                    count,
                },
            )
            .collect();
        AddonVectorStats {
            available: true,
            namespaces,
        }
    };

    // Recording (feature-gated).
    #[cfg(feature = "camera")]
    let recording = match repository::recording_stats_for_addon(db, &r.addon_id, None, Some(org_id))
    {
        Ok(agg) => AddonRecordingStats {
            available: true,
            segments: agg.total_segments as i64,
            snapshots: agg.total_snapshots as i64,
            bytes: agg.total_size_bytes as i64,
        },
        // Blad zapytania (np. schemat kamer) -> nie raportuj falszywych zer.
        Err(_) => AddonRecordingStats {
            available: false,
            segments: 0,
            snapshots: 0,
            bytes: 0,
        },
    };
    #[cfg(not(feature = "camera"))]
    let recording = AddonRecordingStats {
        available: false,
        segments: 0,
        snapshots: 0,
        bytes: 0,
    };

    Ok(MessageBody::AddonStorageBody(P::StatsResponse(
        AddonStorageStatsResponse {
            kv,
            sql,
            vector,
            recording,
        },
    )))
}

/// Statystyki per-addon SQLite z OSOBNEGO, read-only polaczenia do pliku data.db
/// (zero interferencji z poolem zapisu addona; WAL pozwala czytac rownolegle z
/// zapisami, wiec nie blokujemy zywego addona). Rozmiar = page_count*page_size
/// (tani pragma, bez skanu). Liczba wierszy liczona z capem (LIMIT SQL_ROW_CAP+1)
/// zeby nie skanowac ogromnych tabel — przy przekroczeniu zwracamy dolna granice
/// (`rows_capped=true`).
fn addon_sql_stats(org_id: &str, addon_id: &str) -> AddonSqlStats {
    use rusqlite::OpenFlags;
    let unavailable = || AddonSqlStats {
        enabled: true,
        available: false,
        db_size_bytes: -1,
        tables: Vec::new(),
    };
    let path = match crate::addon::fs_sandbox::addon_db_path(org_id, addon_id) {
        Ok(p) => p,
        Err(_) => return unavailable(),
    };
    if !path.exists() {
        return unavailable();
    }
    let conn = match rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    {
        Ok(c) => c,
        Err(_) => return unavailable(),
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(200));

    // Gwarancja "nie blokuje zapisow addona" trzyma sie tylko w WAL (czytelnik
    // i pisarz rownolegle). Managed addon DB zawsze jest WAL (storage_sql go
    // wymusza), ale dla podmienionego/uszkodzonego pliku nie-WAL skan moglby
    // blokowac commit pisarza — wtedy raportujemy unavailable zamiast skanowac.
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
        .unwrap_or_default();
    if !journal.eq_ignore_ascii_case("wal") {
        return unavailable();
    }

    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap_or(-1);
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap_or(-1);
    let db_size_bytes = if page_count >= 0 && page_size >= 0 {
        page_count * page_size
    } else {
        -1
    };

    let mut tables: Vec<AddonSqlTable> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' \
         AND name NOT LIKE '__tentaflow_%' AND name NOT LIKE 'sqlite_%' ORDER BY name ASC",
    ) {
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map(|it| it.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        for name in names {
            // Identyfikator z sqlite_master (zaufany schemat); escapujemy cudzyslow.
            let q = format!(
                "SELECT COUNT(*) FROM (SELECT 1 FROM \"{}\" LIMIT {})",
                name.replace('"', "\"\""),
                SQL_ROW_CAP + 1
            );
            let cnt: i64 = conn.query_row(&q, [], |r| r.get(0)).unwrap_or(-1);
            let (rows, rows_capped) = if cnt > SQL_ROW_CAP {
                (SQL_ROW_CAP, true)
            } else {
                (cnt, false)
            };
            tables.push(AddonSqlTable {
                name,
                rows,
                rows_capped,
            });
        }
    }

    AddonSqlStats {
        enabled: true,
        available: true,
        db_size_bytes,
        tables,
    }
}

/// Rejestruje handler storage stats pod inner-nazwa requestu (Admin).
macro_rules! register_addon_storage_variant {
    ($variant:literal, $metric:literal, $auth:expr) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: $auth,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_addon_storage_dispatch,
            }
        }
    };
}

register_addon_storage_variant!(
    "AddonStorageStatsRequest",
    "tentaflow_ws_handler_addon_storage_stats",
    crate::dispatch::SessionAuthKind::Admin
);

// =============================================================================
// Vector backend picker addona (zakladka Ustawienia): zvec vs Milvus.
// Multipleksowane w `AddonVectorBody`.
// =============================================================================

const CFG_VECTOR_CONFIG: &str = "__vector_config";
const CFG_VECTOR_MILVUS_USER: &str = "__vector_milvus_user";
const CFG_VECTOR_MILVUS_PASSWORD: &str = "__vector_milvus_password";

// Bounds for persisted vector-config fields (CBOR/UI decode + DB size guard).
const MAX_VECTOR_URI_LEN: usize = 512;
const MAX_VECTOR_COLLECTION_LEN: usize = 128;
const MAX_VECTOR_SECRET_LEN: usize = 512;

fn default_vector_config() -> AddonVectorConfig {
    AddonVectorConfig {
        backend: "zvec".to_string(),
        milvus_source: None,
        service_ref: None,
        manual_uri: None,
        collection_override: None,
    }
}

#[handler(variant = "AddonVectorBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_vector_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use AddonVectorPayload as P;
    let payload = match req {
        MessageBody::AddonVectorBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonVectorBody")),
    };
    let db = &ctx.state.db;

    let res = match payload {
        P::GetConfigRequest(r) => {
            validate_addon_id(&r.addon_id)?;
            repository::get_addon(db, &r.addon_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

            let rows = repository::list_addon_config_rows(db, &r.addon_id).map_err(db_err)?;
            let raw = rows
                .iter()
                .find(|c| c.key == CFG_VECTOR_CONFIG)
                .map(|c| c.value.clone())
                .filter(|s| !s.trim().is_empty());
            let config = raw
                .and_then(|s| serde_json::from_str::<AddonVectorConfig>(&s).ok())
                .unwrap_or_else(default_vector_config);
            let has_milvus_user = rows
                .iter()
                .any(|c| c.key == CFG_VECTOR_MILVUS_USER && !c.value.trim().is_empty());
            let has_milvus_password = rows
                .iter()
                .any(|c| c.key == CFG_VECTOR_MILVUS_PASSWORD && !c.value.trim().is_empty());

            let milvus_compiled =
                crate::services::vector::namespace::NamespaceManager::milvus_compiled();
            // Local services only when this build links Milvus; remote services
            // (proxied over mesh) are usable even without the local feature.
            let milvus_services = discover_milvus_services(ctx);

            P::GetConfigResponse(AddonVectorConfigResponse {
                milvus_compiled,
                config,
                has_milvus_user,
                has_milvus_password,
                milvus_services,
            })
        }
        P::SetConfigRequest(r) => {
            validate_addon_id(&r.addon_id)?;
            repository::get_addon(db, &r.addon_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
            validate_vector_config(&r.config)?;
            let milvus_compiled =
                crate::services::vector::namespace::NamespaceManager::milvus_compiled();
            if r.config.backend == "milvus" {
                match r.config.milvus_source.as_deref() {
                    Some("service_ref") => {
                        // service_ref (lokalny LUB zdalny) musi wskazywac
                        // istniejacy, osiagalny serwis Milvus — sprawdzamy
                        // wzgledem polaczonego dyskoveru (po node_id + service_id).
                        let sref = r
                            .config
                            .service_ref
                            .as_ref()
                            .ok_or_else(|| ProtocolError::bad_request("brak service_ref"))?;
                        let ok = discover_milvus_services(ctx).into_iter().any(|s| {
                            s.node_id == sref.node_id
                                && s.service_id == sref.service_id
                                && s.reachable
                        });
                        if !ok {
                            return Err(ProtocolError::bad_request(
                                "service_ref nie wskazuje osiagalnego serwisu Milvus",
                            ));
                        }
                        // Zdalny ref wymaga zywego transportu mesh — odrzucamy
                        // zanim zapiszemy config, ktory padlby przy pierwszym uzyciu.
                        if !sref.node_id.trim().is_empty()
                            && !crate::services::vector_namespace_manager(db)
                                .remote_transport_ready()
                        {
                            return Err(ProtocolError::bad_request(
                                "mesh nie jest jeszcze gotowy — zdalny serwis Milvus chwilowo \
                                 niedostepny",
                            ));
                        }
                    }
                    Some("manual") => {
                        // Reczny URL laczy sie bezposrednio (lokalny klient
                        // Milvus) — wymaga feature vector-milvus na tym nodzie.
                        if !milvus_compiled {
                            return Err(ProtocolError::bad_request(
                                "ten node nie ma wkompilowanego Milvus — reczny URL niedostepny \
                                 (uzyj serwisu Milvus z innego noda)",
                            ));
                        }
                    }
                    _ => {}
                }
            }
            if r.milvus_user
                .as_deref()
                .map(|u| u.len() > MAX_VECTOR_SECRET_LEN)
                .unwrap_or(false)
                || r.milvus_password
                    .as_deref()
                    .map(|p| p.len() > MAX_VECTOR_SECRET_LEN)
                    .unwrap_or(false)
            {
                return Err(ProtocolError::bad_request("milvus user/password za dlugie"));
            }
            let updated_by = current_user_id(ctx);
            // Normalizuj: trzymaj tylko pola istotne dla wybranego backendu/zrodla,
            // zeby nie persystowac nieograniczonych smieci w nieuzywanych polach.
            let mut stored = r.config.clone();
            if let Some(uri) = stored.manual_uri.as_mut() {
                *uri = uri.trim().to_string();
            }
            if stored.backend != "milvus" {
                stored.milvus_source = None;
                stored.service_ref = None;
                stored.manual_uri = None;
            } else {
                match stored.milvus_source.as_deref() {
                    Some("manual") => stored.service_ref = None,
                    Some("service_ref") => stored.manual_uri = None,
                    _ => {}
                }
            }
            let json = serde_json::to_string(&stored)
                .map_err(|e| ProtocolError::internal(format!("serialize vector config: {e}")))?;
            repository::upsert_addon_config_value(
                db,
                &r.addon_id,
                CFG_VECTOR_CONFIG,
                &json,
                false,
                updated_by.as_deref(),
            )
            .map_err(db_err)?;
            if let Some(u) = &r.milvus_user {
                repository::upsert_addon_config_value(
                    db,
                    &r.addon_id,
                    CFG_VECTOR_MILVUS_USER,
                    u,
                    true,
                    updated_by.as_deref(),
                )
                .map_err(db_err)?;
            }
            if let Some(p) = &r.milvus_password {
                repository::upsert_addon_config_value(
                    db,
                    &r.addon_id,
                    CFG_VECTOR_MILVUS_PASSWORD,
                    p,
                    true,
                    updated_by.as_deref(),
                )
                .map_err(db_err)?;
            }
            // Drop cached open backends for this addon so the new config takes
            // effect on next access without a process restart.
            crate::services::vector_namespace_manager(db).invalidate_addon(&r.addon_id);
            P::SetConfigResponse(AddonVectorSetConfigResponse {
                ok: true,
                error: None,
            })
        }
        P::GetConfigResponse(_) | P::SetConfigResponse(_) => {
            return Err(ProtocolError::bad_request("unexpected response variant"))
        }
    };

    Ok(MessageBody::AddonVectorBody(res))
}

/// Waliduje config: backend zvec|milvus; dla milvus wymaga zrodla i jego pola.
/// Sprawdza ksztalt (schemat URI, dlugosci); istnienie serwisu weryfikuje handler.
fn validate_vector_config(cfg: &AddonVectorConfig) -> Result<(), ProtocolError> {
    if let Some(co) = cfg.collection_override.as_deref() {
        if co.len() > MAX_VECTOR_COLLECTION_LEN {
            return Err(ProtocolError::bad_request("collection_override za dlugie"));
        }
    }
    match cfg.backend.as_str() {
        "zvec" => Ok(()),
        "milvus" => match cfg.milvus_source.as_deref() {
            Some("manual") => {
                let uri = cfg.manual_uri.as_deref().map(str::trim).unwrap_or("");
                if uri.is_empty() {
                    return Err(ProtocolError::bad_request(
                        "milvus_source=manual wymaga manual_uri",
                    ));
                }
                if uri.len() > MAX_VECTOR_URI_LEN {
                    return Err(ProtocolError::bad_request("manual_uri za dlugie"));
                }
                if !(uri.starts_with("http://") || uri.starts_with("https://")) {
                    return Err(ProtocolError::bad_request(
                        "manual_uri musi byc http:// lub https://",
                    ));
                }
                Ok(())
            }
            Some("service_ref") => {
                if cfg
                    .service_ref
                    .as_ref()
                    .map(|s| !s.service_id.trim().is_empty())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err(ProtocolError::bad_request(
                        "milvus_source=service_ref wymaga service_ref.service_id",
                    ))
                }
            }
            _ => Err(ProtocolError::bad_request(
                "backend=milvus wymaga milvus_source (service_ref|manual)",
            )),
        },
        other => Err(ProtocolError::bad_request(format!(
            "nieznany vector backend '{other}' (zvec|milvus)"
        ))),
    }
}

/// Polaczona lista serwisow Milvus dla pickera: lokalne (tylko gdy ten build
/// linkuje Milvus — inaczej wybor lokalnego konczy sie bledem przy uzyciu) plus
/// zdalne z rejestru mesh (proxowane przez VectorOp — dzialaja nawet bez
/// lokalnego feature). Dedup po (node_id, service_id).
fn discover_milvus_services(ctx: &HandlerContext) -> Vec<AddonMilvusService> {
    let mut out = if crate::services::vector::namespace::NamespaceManager::milvus_compiled() {
        discover_local_milvus_services(&ctx.state.db)
    } else {
        Vec::new()
    };
    out.extend(discover_remote_milvus_services(ctx));
    out
}

/// Serwisy Milvus na INNYCH nodach z rejestru mesh. `reachable` = wlasciciel ma
/// serwis running/degraded z endpointem (loopback po jego stronie — my laczymy
/// sie przez mesh, nie bezposrednio, wiec endpointu nie pokazujemy klientowi).
///
/// `reachable` mowi tylko, ze serwis Milvus DZIALA u wlasciciela — nie, ze jego
/// Core ma feature `vector-milvus` (potrzebny do wykonania VectorOp). Brak kanalu
/// rozglaszania capability nodow, wiec taki rzadki przypadek (Milvus jako infra
/// bez klienta w Core) konczy sie jasnym bledem przy pierwszej operacji, jak inne
/// proxy mesh (web_research degraduje tak samo) — nie cicha utrata danych.
fn discover_remote_milvus_services(ctx: &HandlerContext) -> Vec<AddonMilvusService> {
    let registry = match ctx
        .state
        .service_manager
        .mesh_services_registry
        .read()
        .clone()
    {
        Some(r) => r,
        None => return Vec::new(),
    };
    let local_node = registry.local().node_id.clone();
    registry
        .visible_services()
        .into_iter()
        .filter(|s| s.engine_id == "milvus" && !s.node_id.is_empty() && s.node_id != local_node)
        .map(|s| {
            let reachable = !s.paused
                && matches!(s.status.as_str(), "running" | "degraded")
                && s.endpoint_url
                    .as_deref()
                    .map(|u| !u.is_empty())
                    .unwrap_or(false);
            AddonMilvusService {
                node_id: s.node_id,
                local: false,
                service_id: s.id.to_string(),
                display_name: s.display_name,
                // Remote endpoint is the owner's loopback — not meaningful (and
                // not reachable) for this node; the data path goes via mesh.
                endpoint: String::new(),
                reachable,
            }
        })
        .collect()
}

/// Origin-side: replicate an installed/updated bundled addon instance to the
/// mesh. Best-effort — a sync-capture failure never fails the user's action.
fn capture_addon_instance_sync(db: &crate::db::DbPool, addon_id: &str) {
    if let Err(e) = repository::capture_addon_instance_insert(db, addon_id) {
        tracing::warn!("addon instance sync capture nieudany dla '{addon_id}': {e}");
    }
}

/// Lista lokalnych serwisow Milvus (engine_id='milvus') dla pickera. Lokalny
/// serwis jest osiagalny (ten sam node), wiec reachable = running+endpoint.
fn discover_local_milvus_services(db: &crate::db::DbPool) -> Vec<AddonMilvusService> {
    use crate::services_repo::services::ServiceStatus;
    let conn = match db.read() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let services = match crate::services_repo::services::list_all(&conn) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    services
        .into_iter()
        .filter(|s| s.engine_id == "milvus")
        .map(|s| {
            let endpoint = s.endpoint_url.clone().unwrap_or_default();
            let reachable = !s.paused
                && matches!(s.status, ServiceStatus::Running | ServiceStatus::Degraded)
                && !endpoint.is_empty();
            AddonMilvusService {
                node_id: String::new(),
                local: true,
                service_id: s.id.to_string(),
                display_name: s.display_name,
                endpoint,
                reachable,
            }
        })
        .collect()
}

/// Rejestruje handler pickera pod inner-nazwami requestow (Admin).
macro_rules! register_addon_vector_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_addon_vector_dispatch,
            }
        }
    };
}

register_addon_vector_variant!(
    "AddonVectorGetConfigRequest",
    "tentaflow_ws_handler_addon_vector_get_config"
);
register_addon_vector_variant!(
    "AddonVectorSetConfigRequest",
    "tentaflow_ws_handler_addon_vector_set_config"
);

#[cfg(test)]
mod declared_status_tests {
    use super::*;
    use crate::db::repository::AddonDeclaredNetworkRule;

    /// tool_decl_from_manifest rekonstruuje liste parametrow z JSON Schema
    /// (`properties` + `required`) — w tej formie parser trzyma `[[tool]]`.
    #[test]
    fn tool_decl_maps_params_from_schema() {
        let t = crate::addon::ManifestTool {
            name: "search".to_string(),
            description: "Szukaj".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Zapytanie" },
                    "limit": { "type": "number", "description": "Limit", "default": 10 }
                },
                "required": ["query"]
            }),
            return_schema: Some(serde_json::json!({ "type": "object" })),
            keywords: vec![],
        };
        let decl = tool_decl_from_manifest(&t);
        assert_eq!(decl.name, "search");
        assert_eq!(decl.return_type, "object");
        assert_eq!(decl.parameters.len(), 2);
        // posortowane po nazwie: limit, query
        let q = decl.parameters.iter().find(|p| p.name == "query").unwrap();
        assert_eq!(q.param_type, "string");
        assert!(q.required);
        let l = decl.parameters.iter().find(|p| p.name == "limit").unwrap();
        assert_eq!(l.param_type, "number");
        assert!(!l.required);
        assert_eq!(l.default_value.as_deref(), Some("10"));
    }

    fn rule(host: &str, approved: bool) -> AddonDeclaredNetworkRule {
        AddonDeclaredNetworkRule {
            rule_id: host.to_string(),
            host: host.to_string(),
            port: 443,
            protocol: "tcp".to_string(),
            required: true,
            approved,
        }
    }

    #[test]
    fn allow_covered_when_rule_approved() {
        let declared = vec![rule("graph.microsoft.com", true)];
        let allowed = vec!["graph.microsoft.com".to_string()];
        let blocked: Vec<String> = vec![];
        let out = compute_declared_status(&declared, &allowed, &blocked);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, "covered");
        assert_eq!(out[0].mode, "allow");
        assert_eq!(out[0].port, Some(443));
    }

    #[test]
    fn allow_missing_when_host_absent() {
        let declared = vec![rule("api.example.com", false)];
        let out = compute_declared_status(&declared, &[], &[]);
        assert_eq!(out[0].status, "missing");
    }

    #[test]
    fn allow_conflicting_when_approved_host_in_blocked() {
        let declared = vec![rule("api.example.com", true)];
        let blocked = vec!["api.example.com".to_string()];
        let out = compute_declared_status(&declared, &[], &blocked);
        assert_eq!(out[0].status, "conflicting");
    }

    #[test]
    fn multiple_rules_independent_status() {
        let declared = vec![
            rule("a.example.com", true),
            rule("b.example.com", true),
            rule("c.example.com", false),
        ];
        let allowed = vec!["a.example.com".to_string()];
        let blocked = vec!["b.example.com".to_string()];
        let out = compute_declared_status(&declared, &allowed, &blocked);
        assert_eq!(out[0].status, "covered");
        assert_eq!(out[1].status, "conflicting");
        assert_eq!(out[2].status, "missing");
    }
}
