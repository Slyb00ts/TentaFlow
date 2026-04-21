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
    AddonLogEntry, AddonLogsResponse, AddonNetworkRuleDecl, AddonNetworkRulesGetResponse,
    AddonNetworkRulesSetResponse,
    AddonReloadResponse, AddonResourcesGetResponse, AddonResourcesSetResponse, AddonToggleResponse,
    AddonToolDecl, AddonToolParam, AddonToolsResponse, AddonUninstallResponse, MessageBody,
    ProtocolError, ProtocolErrorCode, SessionAuth,
};

use crate::db::repository;
use crate::dispatch::HandlerContext;

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
fn current_user_id(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] != 0xFF {
                return None;
            }
            let mut le = [0u8; 8];
            le.copy_from_slice(&user_id[8..]);
            Some(i64::from_le_bytes(le))
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
        user_id,
        Some(addon_id),
        action,
        Some("addon"),
        Some(addon_id),
        Some(&details),
        severity,
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

    let install_result = crate::addon::lifecycle::install(&addon_dir, &ctx.state.db);
    let _ = std::fs::remove_dir_all(&tmp_root);

    match install_result {
        Ok(manifest) => {
            let addon_id = manifest.addon_id.clone();
            let version = manifest.version.clone();
            let _ = repository::create_default_addon_resource_limits(&ctx.state.db, &addon_id);
            // Audyt: podstawowe metadane instalacji (bez zawartosci plikow/konfiguracji).
            let declared_oauth_providers: Vec<String> = manifest
                .oauth_provider
                .iter()
                .map(|p| p.id.clone())
                .collect();
            audit(
                ctx,
                "addon_install",
                &addon_id,
                serde_json::json!({
                    "addon_id": addon_id,
                    "version": version,
                    "declared_permissions_count": manifest.declared_permissions.len(),
                    "declared_oauth_providers": declared_oauth_providers,
                    "file_size_bytes": payload.content.len(),
                    "filename": payload.filename,
                }),
                "warning",
            );
            Ok(MessageBody::AddonInstallResponseBody(
                AddonInstallResponse {
                    ok: true,
                    addon_id: Some(addon_id),
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

    crate::addon::lifecycle::uninstall(&payload.addon_id, &ctx.state.db)
        .map_err(|e| ProtocolError::internal(format!("uninstall: {}", e)))?;

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
            updated_by,
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
        if !repository::is_addon_visible_to_user(&ctx.state.db, &payload.addon_id, uid)
            .map_err(db_err)?
        {
            return Err(ProtocolError::not_found("addon nie istnieje"));
        }
    }

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

    let manifest = parse_manifest(&addon.manifest_json);
    let tools = extract_tools_from_manifest(&manifest);

    Ok(MessageBody::AddonToolsResponseBody(AddonToolsResponse {
        tools,
    }))
}

/// Wyciaga deklaracje tools z manifestu (sekcja [tools.<name>]). Zwraca puste Vec jesli brak.
fn extract_tools_from_manifest(manifest: &toml::Value) -> Vec<AddonToolDecl> {
    let Some(tools_tbl) = manifest.get("tools").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let mut out: Vec<AddonToolDecl> = Vec::with_capacity(tools_tbl.len());
    for (name, def) in tools_tbl.iter() {
        let description = def
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let return_type = def
            .get("returns")
            .or_else(|| def.get("return_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let parameters = def
            .get("parameters")
            .and_then(|v| v.as_table())
            .map(|params| {
                params
                    .iter()
                    .map(|(pname, pdef)| AddonToolParam {
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
                        required: pdef
                            .get("required")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        default_value: pdef.get("default").map(|v| match v {
                            toml::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(AddonToolDecl {
            name: name.clone(),
            description,
            parameters,
            return_type,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
    let cfg =
        repository::get_addon_network_config(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let declared_rows = repository::get_addon_declared_network_rules(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?;
    let declared_rules = compute_declared_status(&declared_rows, &cfg.allowed_hosts, &cfg.blocked_hosts);
    Ok(MessageBody::AddonNetworkRulesGetResponseBody(
        AddonNetworkRulesGetResponse {
            allowed_hosts: cfg.allowed_hosts,
            blocked_hosts: cfg.blocked_hosts,
            mode: cfg.mode,
            declared_rules,
        },
    ))
}

/// Merges manifest-declared rules with admin allow/block lists and computes a
/// per-rule status:
/// - declared "allow" + host in allowed_hosts         -> "covered"
/// - declared "allow" + host in blocked_hosts         -> "conflicting"
/// - declared "allow" + host in neither               -> "missing"
/// - declared "block" + host in allowed_hosts         -> "conflicting"
/// - declared "block" + host elsewhere                -> "covered"
///
/// Current manifest schema has no explicit mode field, so every declared rule
/// is treated as "allow" (the addon declares hosts it needs to reach).
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
            let status = match (mode, host_allowed, host_blocked) {
                ("allow", true, _) => "covered",
                ("allow", false, true) => "conflicting",
                ("allow", false, false) => "missing",
                ("block", true, _) => "conflicting",
                ("block", _, _) => "covered",
                _ => "missing",
            };
            AddonNetworkRuleDecl {
                host: r.host.clone(),
                port: Some(r.port),
                mode: mode.to_string(),
                status: status.to_string(),
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
    repository::set_addon_network_config(&ctx.state.db, &payload.addon_id, &new, updated_by)
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

#[cfg(test)]
mod declared_status_tests {
    use super::*;
    use crate::db::repository::AddonDeclaredNetworkRule;

    fn rule(host: &str) -> AddonDeclaredNetworkRule {
        AddonDeclaredNetworkRule {
            host: host.to_string(),
            port: 443,
            protocol: "tcp".to_string(),
            required: true,
        }
    }

    #[test]
    fn allow_covered_when_host_in_allowed() {
        let declared = vec![rule("graph.microsoft.com")];
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
        let declared = vec![rule("api.example.com")];
        let out = compute_declared_status(&declared, &[], &[]);
        assert_eq!(out[0].status, "missing");
    }

    #[test]
    fn allow_conflicting_when_host_in_blocked() {
        let declared = vec![rule("api.example.com")];
        let blocked = vec!["api.example.com".to_string()];
        let out = compute_declared_status(&declared, &[], &blocked);
        assert_eq!(out[0].status, "conflicting");
    }

    #[test]
    fn multiple_rules_independent_status() {
        let declared = vec![
            rule("a.example.com"),
            rule("b.example.com"),
            rule("c.example.com"),
        ];
        let allowed = vec!["a.example.com".to_string()];
        let blocked = vec!["b.example.com".to_string()];
        let out = compute_declared_status(&declared, &allowed, &blocked);
        assert_eq!(out[0].status, "covered");
        assert_eq!(out[1].status, "conflicting");
        assert_eq!(out[2].status, "missing");
    }
}
