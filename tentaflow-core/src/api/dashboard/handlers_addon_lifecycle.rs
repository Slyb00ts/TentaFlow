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
    AddonDisableConsequence, AddonDisablePreviewResponse, AddonTeardownDependent,
    AddonTeardownArmResponse, AddonTeardownEntry, AddonTeardownNode, AddonTeardownPlanResponse,
    AddonTeardownStatusResponse, AddonToolParam,
    AddonToolsResponse, AddonUninstallResponse, AddonVectorConfig,
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

/// Key prefix of the rows the PLATFORM (not the admin form) keeps in an app's
/// `addon_config`: `__vector_config`, `__node_status/<node>`, and TentaNas's
/// `__share/`, `__mount/`, `__addr/`, `__nas_summary/`, `__nas_alert_forward`,
/// `__nas_four_eyes/<org>`.
const INTERNAL_CONFIG_KEY_PREFIX: &str = "__";

/// True for a platform-internal `addon_config` key, which the generic
/// config read/write must never serve or accept.
///
/// Why: a singleton app such as TentaNas is shared by every organisation on a
/// node and keeps per-organisation state in these rows (share registry with
/// export paths and owner org, alert-forward target, four-eyes settings per
/// org). The generic `AddonConfigGet/Set` requests are gated only by the admin
/// role, with no organisation check, so serving these rows there would hand one
/// tenant another tenant's data and let it forge or overwrite it. Internal rows
/// are reached only through their dedicated, scoped paths (the TentaNas
/// handlers, `AddonDetail`'s node statuses, the vector-backend picker).
pub(crate) fn is_internal_config_key(key: &str) -> bool {
    key.starts_with(INTERNAL_CONFIG_KEY_PREFIX)
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
        // A manifest cannot turn an internal key into a form field: the field
        // would make the generic read show it and the generic write accept it.
        if is_internal_config_key(id) {
            continue;
        }
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

    // Native apps: run the enable/disable hook only when the flag actually
    // flipped — a no-op toggle (same value written twice) must not restart
    // whatever the hook starts/stops.
    if prev != payload.enabled {
        if let Ok(Some(addon)) = repository::get_addon(&ctx.state.db, &payload.addon_id) {
            if let Ok(manifest) = crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json)
            {
                crate::addon::native_apps::notify_enabled(
                    &ctx.state.db,
                    &payload.addon_id,
                    &addon.package_id,
                    &manifest,
                    payload.enabled,
                );
            }
        }
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

    // Refused HERE, before anything replicates, when a teardown would refuse
    // (MAJOR 1 of the wave-9b critic): on this node, from its live plan, and
    // on every other node, from the blockers it last published. A removal
    // that went out to the fleet cannot be called back, and a node whose
    // teardown refuses after its row is gone loses the supervision the
    // refusal protects (TentaNas: its Elastic Arrays). The `refusal:` code is
    // also how the dialog knows the removal never left this node.
    let acknowledged = match teardown_preflight(ctx, &addon, &payload.acknowledged_nodes) {
        Ok(acknowledged) => acknowledged,
        Err(refusal) => {
            // Nothing went out, so no teardown will consume a password this
            // node was handed for it (MAJOR B of round 2).
            disarm_local_teardown(ctx, &addon);
            return Err(refusal);
        }
    };
    // Each node the admin proceeds without is on the record, with what that
    // means (MAJOR A of round 2).
    for peer in &acknowledged {
        audit(
            ctx,
            "addon_uninstall_node_acknowledged",
            &payload.addon_id,
            serde_json::json!({
                "node": peer.name,
                "reason": peer.reason,
                "consequence": match peer.reason {
                    "blocked" => "the node's teardown refuses when the removal reaches it: its instance row goes, its data directory and the state its refusal protects (TentaNas: Elastic Arrays) stay on that node without supervision",
                    _ => "the node never said what its teardown would refuse: when the removal reaches it, its teardown may refuse and leave its state (TentaNas: Elastic Arrays) without supervision",
                },
            }),
            "warning",
        );
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

    // Drop the removed instance's grants from the proactive permission cache.
    if let Some(checker) = ctx.state.permission_checker.as_ref() {
        checker.refresh_addon(&payload.addon_id);
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

/// The hooks of a native instance, when it is one.
fn native_hooks_of(addon: &crate::db::models::Addon) -> Option<&'static crate::addon::native_apps::NativeAppHooks> {
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json).ok()?;
    if !manifest.is_native() {
        return None;
    }
    crate::addon::native_apps::hooks_for(&addon.package_id)
}

/// See the call site in `addon_uninstall`. Only an app whose teardown can
/// refuse is judged; every other app's teardown cannot refuse. `Ok` lists
/// the peers the admin proceeds without (retyped acknowledgements).
fn teardown_preflight(
    ctx: &HandlerContext,
    addon: &crate::db::models::Addon,
    acks: &[tentaflow_protocol::AddonUninstallAck],
) -> Result<Vec<crate::addon::native_apps::AcknowledgedPeer>, ProtocolError> {
    if !native_hooks_of(addon).is_some_and(|h| h.refusable_teardown) {
        return Ok(Vec::new());
    }
    let refused = |detail: String| ProtocolError::new(ProtocolErrorCode::Conflict, detail);
    let local = crate::addon::lifecycle::teardown_plan(&addon.addon_id, &ctx.state.db)
        .map_err(|e| ProtocolError::internal(format!("teardown plan: {e}")))?;
    if let Some(block) = local.iter().find(|p| p.entry.blocks) {
        return Err(refused(format!("refusal:teardown_blocked — {}", block.entry.description)));
    }
    let published = crate::addon::native_apps::published_teardown_blocks(&ctx.state.db, &addon.addon_id);
    let peers: Vec<crate::addon::native_apps::PeerForTeardown> = instance_nodes(ctx, &addon.addon_id)
        .into_iter()
        .filter(|n| !n.local)
        .map(|n| crate::addon::native_apps::PeerForTeardown {
            node_id: n.node_id,
            name: n.name,
            status: n.status,
            unpaired: n.unpaired,
            online: n.online,
        })
        .collect();
    let acks: std::collections::BTreeMap<String, String> =
        acks.iter().map(|a| (a.node_id.clone(), a.confirm_name.clone())).collect();
    crate::addon::native_apps::peer_teardown_refusal(&published, &peers, &acks).map_err(refused)
}

/// Drops the teardown password THIS node was handed, when the uninstall
/// stops before its teardown could consume it.
fn disarm_local_teardown(ctx: &HandlerContext, addon: &crate::db::models::Addon) {
    let Some(disarm) = native_hooks_of(addon).and_then(|h| h.disarm_teardown) else { return };
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    let Ok(data_dir) = crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, &addon.addon_id) else { return };
    disarm(&crate::addon::native_apps::NativeAppContext { db: &ctx.state.db, addon_id: &addon.addon_id, org_id, data_dir });
}

// =============================================================================
// 3b. AddonTeardownPlanRequest — Admin. Side-effect-free preview backing the
//     uninstall dialog: paths (with sizes) the wipe removes or keeps, plus the
//     instances that declare `[[uses_app]]` on this package and would lose it
//     once its last instance is gone.
// =============================================================================

#[handler(variant = "AddonTeardownPlanRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_teardown_plan(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonTeardownPlanRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonTeardownPlanRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;

    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;

    let entries = crate::addon::lifecycle::teardown_plan(&payload.addon_id, &ctx.state.db)
        .map_err(|e| ProtocolError::internal(format!("teardown plan: {e}")))?
        .into_iter()
        .map(|p| AddonTeardownEntry {
            path: p.entry.path.to_string_lossy().into_owned(),
            kind: p.entry.kind.to_string(),
            description: p.entry.description.to_string(),
            removed: p.entry.removed,
            size_bytes: p.size_bytes,
            count_vars: p.entry.count_vars.clone(),
            blocks: p.entry.blocks,
        })
        .collect();

    // n18a's mode chip and backup column for THIS node.
    let node_info = native_hooks_of(&addon)
        .and_then(|hooks| hooks.teardown_node_info)
        .and_then(|info| {
            let org_id = crate::services::org::DEFAULT_ORG_ID;
            let data_dir = crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, &addon.addon_id).ok()?;
            Some(info(&crate::addon::native_apps::NativeAppContext {
                db: &ctx.state.db,
                addon_id: &addon.addon_id,
                org_id,
                data_dir,
            }))
        })
        .unwrap_or_default();

    // A dependency binds to the PACKAGE: other instances of the same package
    // keep serving the dependents, so only the last instance breaks them.
    let last_instance = !addon.package_id.is_empty()
        && repository::count_addon_instances(&ctx.state.db, &addon.package_id)
            .map_err(db_err)?
            <= 1;
    let dependents = if last_instance {
        repository::list_addons(&ctx.state.db)
            .map_err(db_err)?
            .into_iter()
            .filter(|other| other.addon_id != addon.addon_id)
            .filter_map(|other| {
                let manifest =
                    crate::addon::lifecycle::parse_manifest_toml(&other.manifest_json).ok()?;
                let dep = manifest
                    .uses_apps
                    .iter()
                    .find(|d| d.package_id == addon.package_id)?;
                Some(AddonTeardownDependent {
                    addon_id: other.addon_id,
                    display_name: if other.display_name.is_empty() {
                        other.name
                    } else {
                        other.display_name
                    },
                    optional: dep.optional,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(MessageBody::AddonTeardownPlanResponseBody(
        AddonTeardownPlanResponse {
            addon_id: addon.addon_id,
            display_name: if addon.display_name.is_empty() {
                addon.name
            } else {
                addon.display_name
            },
            entries,
            dependents,
            nodes: instance_nodes(ctx, &payload.addon_id),
            privilege: node_info.privilege.to_string(),
            backup_file: node_info.backup_file,
        },
    ))
}

/// Every node an uninstall of `addon_id` reaches, this one first: this node,
/// every trust-paired peer, and any node that recorded its own reconcile of
/// the instance (`__node_status/<node>`) although the mesh does not list it
/// now. Each by the name the fleet surfaces use; the id only routes.
fn instance_nodes(ctx: &HandlerContext, addon_id: &str) -> Vec<AddonTeardownNode> {
    let local_id = ctx.state.local_node_id.to_string();
    let statuses: std::collections::BTreeMap<String, String> = repository::list_addon_config_prefixed(
        &ctx.state.db,
        addon_id,
        crate::addon::native_apps::NODE_STATUS_KEY_PREFIX,
    )
    .unwrap_or_default()
    .into_iter()
    .map(|(node_id, value, _)| {
        let status = serde_json::from_str::<serde_json::Value>(&value)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        (node_id, status)
    })
    .collect();
    // `(node_id, online, trusted)`: every trusted peer, and every node that
    // recorded a status for the instance — a node no longer trusted is kept
    // in the list, marked unpaired, so the dialog can say the removal will
    // not reach it (and it holds nothing back).
    let mut ids: Vec<(String, bool, bool)> = vec![(local_id.clone(), true, true)];
    if let Some(iroh) = ctx.state.quic_mesh.as_ref() {
        for peer in ctx.state.mesh_peer_store.list() {
            if peer.node_id == local_id || !iroh.is_trusted(&peer.node_id) {
                continue;
            }
            ids.push((peer.node_id.clone(), peer.quic_connected, true));
        }
    }
    for node_id in statuses.keys() {
        if !ids.iter().any(|(id, _, _)| id == node_id) {
            // Without a running mesh nothing says the node is gone: it is
            // counted as paired (it holds the uninstall back until known).
            let trusted = ctx.state.quic_mesh.as_ref().is_none_or(|iroh| iroh.is_trusted(node_id));
            ids.push((node_id.clone(), false, trusted));
        }
    }
    // What each node last published as refusing its teardown. An app whose
    // teardown cannot refuse has nothing to publish and nothing to know.
    let refusable = repository::get_addon(&ctx.state.db, addon_id)
        .ok()
        .flatten()
        .and_then(|addon| native_hooks_of(&addon))
        .is_some_and(|hooks| hooks.refusable_teardown);
    let published = if refusable {
        crate::addon::native_apps::published_teardown_blocks(&ctx.state.db, addon_id)
    } else {
        Default::default()
    };
    ids.into_iter()
        .map(|(node_id, online, trusted)| {
            let last = published.get(&node_id);
            AddonTeardownNode {
                name: crate::dispatch::app_route::node_display_name(ctx, &node_id),
                local: node_id == local_id,
                online,
                status: statuses.get(&node_id).cloned().unwrap_or_else(|| "unknown".to_string()),
                last_known: !refusable || last.is_some(),
                last_blocks: last
                    .map(|blocks| {
                        blocks
                            .iter()
                            .map(|b| AddonTeardownEntry {
                                path: String::new(),
                                kind: b.kind.clone(),
                                description: String::new(),
                                removed: false,
                                size_bytes: 0,
                                count_vars: b.count_vars.clone(),
                                blocks: true,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                unpaired: !trusted,
                node_id,
            }
        })
        .collect()
}

// =============================================================================
// 3f. AddonTeardownDisarmRequest — Admin. Drops the teardown password THIS
//     node holds (the dialog's every exit before the uninstall).
// =============================================================================

#[handler(variant = "AddonTeardownDisarmRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_teardown_disarm(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonTeardownDisarmRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonTeardownDisarmRequestBody")),
    };
    validate_addon_id(&payload.addon_id)?;
    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    disarm_local_teardown(ctx, &addon);
    Ok(MessageBody::AddonTeardownArmResponseBody(AddonTeardownArmResponse {
        addon_id: addon.addon_id,
        armed_until: String::new(),
    }))
}

// =============================================================================
// 3e. AddonTeardownArmRequest — Admin. Arms THIS node's privilege channel for
//     the teardown about to start (n18a: a mode-B node's password prompt).
// =============================================================================

#[handler(variant = "AddonTeardownArmRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_teardown_arm(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonTeardownArmRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonTeardownArmRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    let arm = native_hooks_of(&addon)
        .and_then(|hooks| hooks.arm_teardown)
        .ok_or_else(|| ProtocolError::bad_request("this app's teardown takes no password"))?;
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    let data_dir = crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, &payload.addon_id)
        .map_err(|e| ProtocolError::internal(format!("instance data dir: {e:?}")))?;
    // A rejected password is the one outcome the admin acts on, so it has its
    // own code; the node's own words stay in the log.
    let armed_until = arm(
        &crate::addon::native_apps::NativeAppContext {
            db: &ctx.state.db,
            addon_id: &payload.addon_id,
            org_id,
            data_dir,
        },
        payload.sudo_password.0.clone(),
    )
    .map_err(|e| {
        tracing::info!("addon '{}': teardown arm refused: {e}", payload.addon_id);
        ProtocolError::new(ProtocolErrorCode::PolicyDenied, "refusal:teardown_password_rejected")
    })?;
    audit(ctx, "addon_teardown_arm", &payload.addon_id, serde_json::json!({}), "warning");
    Ok(MessageBody::AddonTeardownArmResponseBody(AddonTeardownArmResponse {
        addon_id: addon.addon_id,
        armed_until,
    }))
}

// =============================================================================
// 3c. AddonTeardownStatusRequest — Admin. Where the uninstall stands on THIS
//     node; the dialog forwards it to each node in turn (MAJOR 22).
// =============================================================================

#[handler(variant = "AddonTeardownStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_teardown_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonTeardownStatusRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonTeardownStatusRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    let installed = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .is_some();
    Ok(MessageBody::AddonTeardownStatusResponseBody(teardown_status_of(
        &payload.addon_id,
        installed,
        crate::addon::native_apps::teardown_status::get(&payload.addon_id),
    )))
}

/// The answer from its two sources: this process's record of a teardown, and
/// whether the instance row is still here. A record wins — a FAILED uninstall
/// on this node keeps the row, and must read failed, not "not reached yet".
fn teardown_status_of(
    addon_id: &str,
    installed: bool,
    record: Option<crate::addon::native_apps::teardown_status::Status>,
) -> AddonTeardownStatusResponse {
    match record {
        Some(status) => AddonTeardownStatusResponse {
            addon_id: addon_id.to_string(),
            state: status.state.to_string(),
            phase: status.phase,
            warnings: status.warnings,
        },
        None => AddonTeardownStatusResponse {
            addon_id: addon_id.to_string(),
            state: if installed { "installed" } else { "absent" }.to_string(),
            phase: String::new(),
            warnings: Vec::new(),
        },
    }
}

// =============================================================================
// 3d. AddonDisablePreviewRequest — Admin. What disabling the instance does on
//     THIS node (n18d), from the app's own consequence provider.
// =============================================================================

#[handler(variant = "AddonDisablePreviewRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_disable_preview(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonDisablePreviewRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonDisablePreviewRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    let addon = repository::get_addon(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json).ok();
    let background_on_disable = manifest
        .as_ref()
        .and_then(|m| m.native.as_ref().map(|n| n.background_on_disable))
        .unwrap_or(false);
    let provider = manifest
        .as_ref()
        .filter(|m| m.is_native())
        .and_then(|_| crate::addon::native_apps::hooks_for(&addon.package_id))
        .and_then(|hooks| hooks.disable_consequences);
    let consequences = match provider {
        Some(provider) => {
            let org_id = crate::services::org::DEFAULT_ORG_ID;
            let data_dir = crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, &payload.addon_id)
                .map_err(|e| ProtocolError::internal(format!("instance data dir: {e:?}")))?;
            let viewer = ctx.org_context.as_ref().map(|o| o.org_id.clone()).unwrap_or_default();
            provider(
                &crate::addon::native_apps::NativeAppContext {
                    db: &ctx.state.db,
                    addon_id: &payload.addon_id,
                    org_id,
                    data_dir,
                },
                &viewer,
            )
            .map_err(|e| ProtocolError::internal(format!("disable consequences: {e}")))?
            .into_iter()
            .map(|c| AddonDisableConsequence {
                kind: c.kind.to_string(),
                effect: c.effect.to_string(),
                count_vars: c.count_vars,
                names: c.names,
            })
            .collect()
        }
        None => Vec::new(),
    };
    Ok(MessageBody::AddonDisablePreviewResponseBody(AddonDisablePreviewResponse {
        addon_id: addon.addon_id,
        display_name: if addon.display_name.is_empty() {
            addon.name
        } else {
            addon.display_name
        },
        node_name: crate::dispatch::app_route::node_display_name(
            ctx,
            &ctx.state.local_node_id.to_string(),
        ),
        background_on_disable,
        consequences,
    }))
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

    let package_manifest = package_manifest_json(ctx, &payload.addon_id)?;
    let cloud_account_provider = package_manifest
        .as_deref()
        .and_then(crate::addon::lifecycle::parse_cloud_account_provider);

    let rows =
        repository::list_addon_config_rows(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    // Sekret wartosci — zwracamy "" aby GUI wiedzialo ze jest ustawione, ale nie widzi plaintextu.
    let secret_ids: std::collections::HashSet<&str> = schema
        .iter()
        .filter(|f| f.secret)
        .map(|f| f.id.as_str())
        .collect();
    // Internal rows are dropped entirely (not masked like secrets): their keys
    // alone name another organisation's shares and nodes.
    let values: Vec<(String, String)> = rows
        .into_iter()
        .filter(|r| !is_internal_config_key(&r.key))
        .map(|r| {
            if secret_ids.contains(r.key.as_str()) || r.is_secret {
                (r.key, String::new())
            } else {
                (r.key, r.value)
            }
        })
        .collect();

    let requirements = package_manifest
        .as_deref()
        .map(|m| vision_engine_requirements(m))
        .unwrap_or_default();

    Ok(MessageBody::AddonConfigGetResponseBody(
        AddonConfigGetResponse {
            schema,
            values,
            requirements,
            cloud_account_provider,
        },
    ))
}

/// The manifest of the PACKAGE an addon instance was installed from. Read from
/// the package, not the instance manifest: declarations (requirements, vendor
/// account) belong to the package, and an instance installed before one was
/// added still gets it without a reinstall.
fn package_manifest_json(
    ctx: &HandlerContext,
    addon_id: &str,
) -> Result<Option<String>, ProtocolError> {
    Ok(
        match repository::get_addon_instance_package_ref(&ctx.state.db, addon_id)
            .map_err(db_err)?
        {
            Some((package_id, version)) => {
                repository::get_addon_package(&ctx.state.db, &package_id, &version)
                    .map_err(db_err)?
                    .map(|pkg| pkg.manifest_json)
            }
            None => None,
        },
    )
}

// =============================================================================
// AddonRequirementInstallRequest — Admin
// =============================================================================

/// Installs one engine the addon declares as required, from the addon's own
/// settings. Refuses an engine the package does not declare: this screen
/// installs what THIS addon needs, it is not a general model installer. The
/// files land where the deploy wizard would put them (`ensure_bundle`, same
/// source resolution), so a running camera picks the model up on its own —
/// the loaders retry a missing model every 30 s.
#[handler(variant = "AddonRequirementInstallRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn addon_requirement_install(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonRequirementInstallRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonRequirementInstallRequestBody",
            ))
        }
    };
    validate_addon_id(&payload.addon_id)?;
    let manifest = package_manifest_json(ctx, &payload.addon_id)?
        .ok_or_else(|| ProtocolError::not_found("addon nie istnieje"))?;
    let declared = crate::addon::lifecycle::parse_required_vision_engines(&manifest);
    if !declared.iter().any(|e| e == &payload.engine_id) {
        return Err(ProtocolError::bad_request(format!(
            "addon {} does not declare '{}' as a requirement",
            payload.addon_id, payload.engine_id
        )));
    }
    if !gpu_vision_available() {
        return Err(ProtocolError::bad_request(
            "this node cannot run the GPU vision path, installing the model would not help",
        ));
    }
    let engine = crate::services::manifest::registry()
        .by_id(&payload.engine_id)
        .ok_or_else(|| ProtocolError::not_found("engine is not in this build's catalog"))?;
    let base_url = crate::vision::camera_cv_models::resolve_bundle_base_url(None, engine);
    crate::vision::camera_cv_models::ensure_bundle(&payload.engine_id, &base_url, None, None)
        .await
        .map_err(|e| {
            ProtocolError::new(
                tentaflow_protocol::ProtocolErrorCode::Internal,
                format!("install {}: {e:#}", payload.engine_id),
            )
        })?;
    tracing::info!(
        addon_id = %payload.addon_id,
        engine_id = %payload.engine_id,
        "addon requirement installed"
    );
    Ok(MessageBody::AddonRequirementInstallResponseBody(
        tentaflow_protocol::AddonRequirementInstallResponse {
            addon_id: payload.addon_id.clone(),
            requirements: vision_engine_requirements(&manifest),
        },
    ))
}

/// State of every vision engine the package declares it needs. `installed`
/// means every file of the engine's camera-CV bundle is in the vision model
/// directory; `unsupported_host` means this build/host cannot run the GPU path
/// at all, so installing the files would not help.
fn vision_engine_requirements(manifest_json: &str) -> Vec<tentaflow_protocol::AddonRequirement> {
    crate::addon::lifecycle::parse_required_vision_engines(manifest_json)
        .into_iter()
        .map(|engine_id| {
            let status = if !gpu_vision_available() {
                "unsupported_host"
            } else if vision_bundle_installed(&engine_id) {
                "installed"
            } else {
                "missing"
            };
            tentaflow_protocol::AddonRequirement {
                engine_id,
                status: status.to_string(),
            }
        })
        .collect()
}

/// True when this build runs the GPU vision path the camera-CV engines need.
fn gpu_vision_available() -> bool {
    cfg!(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "inference-vision-gpu",
        feature = "vision-ort",
        feature = "vision-cuda-preprocess"
    ))
}

/// True when every file of the engine's camera-CV bundle is present locally.
/// An engine that is not a camera-CV bundle has nothing to install, so it
/// counts as installed.
fn vision_bundle_installed(engine_id: &str) -> bool {
    let Some(files) = crate::vision::camera_cv_models::bundle_file_names(engine_id) else {
        return true;
    };
    let dir = crate::paths::vision_models_dir();
    files.iter().all(|name| dir.join(name).exists())
}

// =============================================================================
// 5. AddonConfigSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonConfigSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn addon_config_set(
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
    for (k, v) in payload.values.iter() {
        // Checked on its own, before the schema, so the refusal does not hinge
        // on what some manifest declares: the whole request is rejected before
        // anything is written.
        if is_internal_config_key(k) {
            return Err(ProtocolError::bad_request(format!(
                "internal configuration key cannot be set: {k}"
            )));
        }
        if !schema_map.contains_key(k.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "nieznane pole konfiguracji: {}",
                k
            )));
        }
        // Before anything is written: a robot IP lands in a network rule host.
        crate::addon::lifecycle::validate_connection_param_value(&addon.manifest_json, k, v)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
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

    // Privacy options drive the camera pipeline, so a change must reach the
    // RUNNING camera; the session rebuilds its graph rather than switching one,
    // which is what keeps an unprocessed frame from slipping out mid-change.
    #[cfg(feature = "camera")]
    let privacy_cameras_applied = if fields_changed.iter().any(|k| {
        k == crate::services::camera_ingest::privacy_options::CONFIG_FACE_BLUR
            || k == crate::services::camera_ingest::privacy_options::CONFIG_PERSON_DETECT
    }) {
        crate::addon::host_functions::camera::apply_privacy_to_addon_cameras(
            &ctx.state.db,
            &payload.addon_id,
        )
        .await
        .map_err(|e| ProtocolError::internal(format!("privacy options: {e:#}")))?
    } else {
        0
    };
    // A build without the camera pipeline (the slim edition) runs no camera
    // whose graph the options could change.
    #[cfg(not(feature = "camera"))]
    let privacy_cameras_applied: u32 = 0;

    // A connection param (robot IP) feeds the instance's network rules, which
    // were resolved at install — re-resolve them and reload the runtime.
    let pending_network_hosts = match ctx.state.addon_manager.clone() {
        Some(mgr) => mgr
            .rebind_instance_connection(&payload.addon_id, updated_by.as_deref())
            .map_err(|e| ProtocolError::internal(format!("network rules: {e:#}")))?,
        None => Vec::new(),
    };

    Ok(MessageBody::AddonConfigSetResponseBody(AddonConfigSetResponse {
        ok: true,
        pending_network_hosts,
        privacy_cameras_applied,
    }))
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
        document_storage_mb: old.document_storage_mb,
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
                let connection_params =
                    match crate::addon::lifecycle::parse_connection_params(&row.manifest_json) {
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
                // A malformed manifest was already refused above; a missing
                // [native] section simply means "not a singleton" (WASM
                // packages duplicate freely).
                let singleton = crate::addon::lifecycle::manifest_is_singleton(&row.manifest_json)
                    .unwrap_or(false);
                packages.push(AddonPackageInfo {
                    package_id: row.package_id,
                    name: row.name,
                    latest_version: row.version.clone(),
                    versions: vec![row.version],
                    source: row.source,
                    installed_instances,
                    cloud_account_provider: crate::addon::lifecycle::parse_cloud_account_provider(
                        &row.manifest_json,
                    ),
                    connection_params,
                    singleton,
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
                    // Install seeded permission defaults — the proactive cache
                    // must see them now, not after the 5-min background pass.
                    if let Some(checker) = ctx.state.permission_checker.as_ref() {
                        checker.refresh_addon(&addon_id);
                    }
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
            read_only: false,
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

#[cfg(test)]
mod teardown_status_tests {
    use super::*;
    use crate::addon::native_apps::teardown_status::Status;

    /// A record of this process wins over the instance row: a FAILED
    /// uninstall keeps the row on this node and must read failed, not
    /// "the removal has not reached this node yet". Without a record, the
    /// row alone says whether the removal is still to come.
    #[test]
    fn a_failed_teardown_reads_failed_even_with_the_row_still_there() {
        let failed = Status { state: "failed", phase: "tentanas_elastic_check".into(), warnings: Vec::new() };
        let r = teardown_status_of("tentanas-1a2b3c4d", true, Some(failed));
        assert_eq!((r.state.as_str(), r.phase.as_str()), ("failed", "tentanas_elastic_check"));
        assert_eq!(teardown_status_of("tentanas-1a2b3c4d", true, None).state, "installed");
        assert_eq!(teardown_status_of("tentanas-1a2b3c4d", false, None).state, "absent");
        let done = Status { state: "done", phase: "done".into(), warnings: vec!["tentanas_backup_failed".into()] };
        let r = teardown_status_of("tentanas-1a2b3c4d", false, Some(done));
        assert_eq!((r.state.as_str(), r.warnings.clone()), ("done", vec!["tentanas_backup_failed".to_string()]));
    }
}

#[cfg(test)]
mod uninstall_preflight_tests {
    use super::*;
    use crate::addon::native_apps::{record_teardown_blocks, test_support, PublishedBlock};
    use crate::dispatch::state::AppState;
    use std::sync::Arc;
    use tentaflow_protocol::AddonUninstallRequest;

    const ADDON: &str = "test-refusing-app-1a2b3c4d";
    const PEER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn ctx(state: &Arc<AppState>) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession { user_id: [9u8; 16], role: Some("admin".to_string()) },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: None,
        }
    }

    /// An instance of the refusing fixture, and a peer that reconciled it.
    fn seeded() -> Arc<AppState> {
        let state = AppState::for_test();
        let manifest = test_support::fixture_manifest_toml(false)
            .replace(&format!("id = \"{}\"", test_support::PACKAGE_ID), &format!("id = \"{}\"", test_support::REFUSING_PACKAGE_ID));
        state.db.write().unwrap().execute(
            "INSERT INTO addons (addon_id, name, version, package_id, package_version, runtime, is_enabled, manifest_json) \
             VALUES (?1, ?1, '1.0.0', ?2, '1.0.0', 'native', 1, ?3)",
            rusqlite::params![ADDON, test_support::REFUSING_PACKAGE_ID, manifest],
        ).expect("addon row");
        repository::upsert_addon_config_value(&state.db, ADDON, &format!("__node_status/{PEER}"), r#"{"status":"ready"}"#, false, None)
            .expect("peer status");
        state
    }

    fn uninstall(state: &Arc<AppState>) -> Result<MessageBody, ProtocolError> {
        addon_uninstall(
            &MessageBody::AddonUninstallRequestBody(AddonUninstallRequest { addon_id: ADDON.to_string(), acknowledged_nodes: Vec::new() }),
            &ctx(state),
        )
    }

    fn still_installed(state: &Arc<AppState>) -> bool {
        repository::get_addon(&state.db, ADDON).unwrap().is_some()
    }

    fn publish_for(state: &Arc<AppState>, node: &str, blocks: &[PublishedBlock]) {
        let key = format!("{}{node}", crate::addon::native_apps::TEARDOWN_BLOCKS_KEY_PREFIX);
        repository::upsert_addon_config_value(&state.db, ADDON, &key, &serde_json::to_string(blocks).unwrap(), false, None)
            .expect("publish");
    }

    /// MAJOR 1 of the wave-9b critic, the server half: an uninstall of an app
    /// whose teardown can refuse is refused BEFORE anything replicates — the
    /// instance row stays, so no peer loses what its refusal protects — when a
    /// peer published nothing, when a peer's last published plan blocks, and
    /// when this node's own plan blocks. A peer that published an empty plan
    /// lets it through.
    #[test]
    fn an_uninstall_is_refused_before_it_replicates_while_any_node_would_refuse() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        test_support::REFUSE.store(false, std::sync::atomic::Ordering::SeqCst);
        let state = seeded();

        let err = uninstall(&state).expect_err("the peer published nothing");
        assert_eq!(err.code, ProtocolErrorCode::Conflict);
        assert!(err.message.starts_with("refusal:teardown_peer_unknown"), "{}", err.message);
        assert!(still_installed(&state), "refused before the row was touched");

        publish_for(&state, PEER, &[PublishedBlock { kind: "test_blocker".into(), count_vars: Default::default() }]);
        let err = uninstall(&state).expect_err("the peer's last plan blocks");
        assert!(err.message.starts_with("refusal:teardown_peer_blocked"), "{}", err.message);
        assert!(still_installed(&state));

        publish_for(&state, PEER, &[]);
        test_support::REFUSE.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = uninstall(&state).expect_err("this node's own plan blocks");
        assert!(err.message.starts_with("refusal:teardown_blocked"), "{}", err.message);
        assert!(still_installed(&state));

        test_support::REFUSE.store(false, std::sync::atomic::Ordering::SeqCst);
        let addon = repository::get_addon(&state.db, ADDON).unwrap().unwrap();
        assert!(teardown_preflight(&ctx(&state), &addon, &[]).is_ok(), "every node known and none refuses");
        let _ = record_teardown_blocks;
    }

    fn uninstall_with(state: &Arc<AppState>, acks: Vec<tentaflow_protocol::AddonUninstallAck>) -> Result<MessageBody, ProtocolError> {
        addon_uninstall(
            &MessageBody::AddonUninstallRequestBody(AddonUninstallRequest { addon_id: ADDON.to_string(), acknowledged_nodes: acks }),
            &ctx(state),
        )
    }

    /// MAJOR A of round 2: a peer that never published would hold the
    /// uninstall back forever. The admin proceeds without it by retyping its
    /// name (`LOST` for a node without one — this peer has none); a wrong
    /// word does not count; the acknowledgement is on the audit record with
    /// its consequence; and a refusal drops the teardown password this node
    /// was handed (MAJOR B).
    #[test]
    fn a_peer_that_never_published_is_passed_only_with_its_name_retyped() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        test_support::REFUSE.store(false, std::sync::atomic::Ordering::SeqCst);
        let state = seeded();
        let before = test_support::DISARM_CALLS.load(std::sync::atomic::Ordering::SeqCst);

        let err = uninstall_with(&state, vec![]).expect_err("never published");
        assert!(err.message.starts_with("refusal:teardown_peer_unknown"), "{}", err.message);
        assert!(test_support::DISARM_CALLS.load(std::sync::atomic::Ordering::SeqCst) > before, "the refusal drops the held password");
        let err = uninstall_with(&state, vec![tentaflow_protocol::AddonUninstallAck { node_id: PEER.into(), confirm_name: "lost".into() }])
            .expect_err("a wrong word is no acknowledgement");
        assert!(err.message.starts_with("refusal:teardown_peer_unknown"), "{}", err.message);
        assert!(still_installed(&state));

        uninstall_with(&state, vec![tentaflow_protocol::AddonUninstallAck { node_id: PEER.into(), confirm_name: "LOST".into() }])
            .expect("acknowledged: the uninstall proceeds without that node");
        assert!(!still_installed(&state), "uninstalled");
        let audited: i64 = state.db.read().unwrap().query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'addon_uninstall_node_acknowledged' AND details LIKE '%supervision%'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(audited, 1, "the acknowledgement and its consequence are on the record");
    }

    /// The plan tells the dialog what an OFFLINE node last published.
    #[test]
    fn the_plan_carries_each_nodes_last_published_blockers() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        test_support::REFUSE.store(false, std::sync::atomic::Ordering::SeqCst);
        let state = seeded();
        let nodes = instance_nodes(&ctx(&state), ADDON);
        let peer = nodes.iter().find(|n| n.node_id == PEER).expect("the peer is listed");
        assert!(!peer.last_known, "nothing published yet");
        publish_for(&state, PEER, &[PublishedBlock { kind: "test_blocker".into(), count_vars: Default::default() }]);
        let nodes = instance_nodes(&ctx(&state), ADDON);
        let peer = nodes.iter().find(|n| n.node_id == PEER).unwrap();
        assert!(peer.last_known);
        assert_eq!(peer.last_blocks[0].kind, "test_blocker");
    }

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

#[cfg(test)]
mod internal_config_key_tests {
    use super::*;
    use crate::dispatch::state::AppState;
    use std::sync::Arc;
    use tentaflow_protocol::{AddonConfigGetRequest, AddonConfigSetRequest, ProtocolErrorCode};

    const ADDON: &str = "cfg-shared";

    /// A manifest that declares one ordinary field, one secret, and one field
    /// whose id uses the internal prefix — the last must never become usable.
    const MANIFEST: &str = r#"
[addon]
id = "cfg-shared"
name = "cfg-shared"
version = "1.0.0"

[config.schema.host]
label = "Host"
type = "text"

[config.schema.api_key]
label = "API key"
type = "password"

[config.schema."__nas_alert_forward"]
label = "Declared internal"
type = "text"
"#;

    /// An org admin of SOME organisation: the role the generic handlers accept,
    /// with nothing tying it to the organisations whose rows are seeded below.
    fn org_admin_ctx(state: &Arc<AppState>) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [9u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(crate::services::rbac::OrgContext {
                user_id: uuid::Uuid::from_bytes([9u8; 16]).to_string(),
                org_id: "org-a".to_string(),
                role_id: "role-admin".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    /// One shared app instance whose config already holds what TentaNas keeps
    /// there for several organisations, next to its ordinary settings.
    fn seeded_state() -> Arc<AppState> {
        let state = AppState::for_test();
        state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT INTO addons \
                 (addon_id, name, version, package_id, package_version, runtime, is_enabled, \
                  manifest_json) \
                 VALUES (?1, ?1, '1.0.0', ?1, '1.0.0', 'native', 1, ?2)",
                rusqlite::params![ADDON, MANIFEST],
            )
            .expect("test addon row");
        let db = &state.db;
        for (key, value, secret) in [
            ("host", "10.0.0.5", false),
            ("api_key", "s3cret", true),
            ("__share/node-b/share-9", r#"{"name":"finance","org":"org-b"}"#, false),
            ("__nas_alert_forward", r#"{"target":"org-b-hook"}"#, false),
            ("__nas_four_eyes/org-b", r#"{"enabled":true}"#, false),
            ("__node_status/node-b", r#"{"status":"ok"}"#, false),
        ] {
            repository::upsert_addon_config_value(db, ADDON, key, value, secret, None)
                .expect("seed config row");
        }
        state
    }

    fn stored(state: &Arc<AppState>, key: &str) -> Option<String> {
        repository::list_addon_config_rows(&state.db, ADDON)
            .unwrap()
            .into_iter()
            .find(|r| r.key == key)
            .map(|r| r.value)
    }

    fn get(state: &Arc<AppState>) -> AddonConfigGetResponse {
        let req = MessageBody::AddonConfigGetRequestBody(AddonConfigGetRequest {
            addon_id: ADDON.to_string(),
        });
        match addon_config_get(&req, &org_admin_ctx(state)).expect("config get") {
            MessageBody::AddonConfigGetResponseBody(r) => r,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    async fn set(
        state: &Arc<AppState>,
        values: &[(&str, &str)],
    ) -> Result<MessageBody, ProtocolError> {
        let req = MessageBody::AddonConfigSetRequestBody(AddonConfigSetRequest {
            addon_id: ADDON.to_string(),
            values: values
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        });
        addon_config_set(&req, &org_admin_ctx(state)).await
    }

    #[test]
    fn internal_prefix_is_what_marks_a_key_internal() {
        for key in [
            "__share/n/s",
            "__mount/n/s",
            "__addr/n",
            "__nas_summary/n",
            "__nas_alert_forward",
            "__nas_four_eyes/org",
            "__node_status/n",
            "__vector_config",
        ] {
            assert!(is_internal_config_key(key), "{key} must be internal");
        }
        for key in ["host", "api_key", "_single", "a__b", ""] {
            assert!(!is_internal_config_key(key), "{key} must not be internal");
        }
    }

    #[test]
    fn org_admin_generic_read_returns_no_internal_row() {
        let state = seeded_state();
        let resp = get(&state);
        let leaked: Vec<&String> = resp
            .values
            .iter()
            .map(|(k, _)| k)
            .filter(|k| is_internal_config_key(k))
            .collect();
        assert!(leaked.is_empty(), "internal rows leaked: {leaked:?}");
        assert!(
            resp.schema.iter().all(|f| !is_internal_config_key(&f.id)),
            "a manifest-declared internal field must not reach the form"
        );
    }

    #[test]
    fn generic_read_still_returns_ordinary_values_and_masks_secrets() {
        let state = seeded_state();
        let resp = get(&state);
        let mut values = resp.values.clone();
        values.sort();
        assert_eq!(
            values,
            vec![
                ("api_key".to_string(), String::new()),
                ("host".to_string(), "10.0.0.5".to_string()),
            ]
        );
        let mut ids: Vec<&str> = resp.schema.iter().map(|f| f.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["api_key", "host"]);
    }

    #[tokio::test]
    async fn generic_write_of_an_internal_key_is_refused_and_writes_nothing() {
        let state = seeded_state();
        for key in [
            "__nas_alert_forward",
            "__nas_four_eyes/org-b",
            "__share/node-b/forged",
        ] {
            // The ordinary key rides along to prove the request is refused as a
            // whole, not applied up to the bad key.
            let err = set(&state, &[("host", "10.9.9.9"), (key, "{}")])
                .await
                .expect_err("internal key must be refused");
            assert_eq!(err.code, ProtocolErrorCode::BadRequest, "{key}");
        }
        assert_eq!(stored(&state, "host").as_deref(), Some("10.0.0.5"));
        assert_eq!(
            stored(&state, "__nas_alert_forward").as_deref(),
            Some(r#"{"target":"org-b-hook"}"#)
        );
        assert_eq!(
            stored(&state, "__nas_four_eyes/org-b").as_deref(),
            Some(r#"{"enabled":true}"#)
        );
        assert_eq!(stored(&state, "__share/node-b/forged"), None);
    }

    #[tokio::test]
    async fn generic_write_of_ordinary_keys_still_works() {
        let state = seeded_state();
        set(&state, &[("host", "10.0.0.7"), ("api_key", "rotated")])
            .await
            .expect("ordinary write");
        assert_eq!(stored(&state, "host").as_deref(), Some("10.0.0.7"));
        assert_eq!(stored(&state, "api_key").as_deref(), Some("rotated"));
        // Internal rows are untouched by an ordinary save.
        assert_eq!(
            stored(&state, "__share/node-b/share-9").as_deref(),
            Some(r#"{"name":"finance","org":"org-b"}"#)
        );
    }
}
