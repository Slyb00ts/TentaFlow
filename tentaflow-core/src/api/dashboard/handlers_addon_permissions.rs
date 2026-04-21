// =============================================================================
// Plik: api/dashboard/handlers_addon_permissions.rs
// Opis: Handlery binary protocol dla zarzadzania uprawnieniami i widocznoscia
//       addonow: detail, visibility list/set, admin_only, permission catalog/
//       matrix/set/default/check. Migracja 38. Polityka domyslna: Admin,
//       wyjatki dla per-user check (UserSession).
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    AddonAdminOnlySetResponse, AddonDetailResponse, AddonOAuthProviderDecl as ProtoOAuthDecl,
    AddonPermissionCatalogResponse, AddonPermissionChangedEvent, AddonPermissionCheckResponse,
    AddonPermissionDecl, AddonPermissionDefault as ProtoDefault, AddonPermissionDefaultSetResponse,
    AddonPermissionMatrixResponse, AddonPermissionRow as ProtoRow, AddonPermissionSetResponse,
    AddonShowInCatalogSetResponse, AddonVisibilityListResponse, AddonVisibilityRow as ProtoVis,
    AddonVisibilitySetResponse, MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
};

use crate::db::repository;
use crate::dispatch::{addon_perm_broadcast, HandlerContext};

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

/// Parsuje timestamp SQLite do epoch sekund (0 przy bledzie).
fn parse_epoch(s: &str) -> u64 {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return t.timestamp() as u64;
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return t.and_utc().timestamp() as u64;
    }
    0
}

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

fn is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

/// Krotki helper do emitowania wpisow audytowych — severity decyduje kto widzi alert.
fn audit(
    ctx: &HandlerContext,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    details_json: serde_json::Value,
    severity: &str,
) {
    let user_id = current_user_id(ctx);
    let details = details_json.to_string();
    let node_id = ctx.state.local_node_id.as_ref();
    if let Err(e) = repository::log_audit_full(
        &ctx.state.db,
        user_id,
        Some(resource_id),
        action,
        Some(resource_type),
        Some(resource_id),
        Some(&details),
        severity,
        None,
        Some(node_id),
    ) {
        tracing::warn!("audit log failed ({}): {}", action, e);
    }
}

// =============================================================================
// 1. AddonDetailRequest — UserSession (kazdy zalogowany moze prosic o detail)
// =============================================================================

#[handler(variant = "AddonDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_detail(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonDetailRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonDetailRequestBody",
            ))
        }
    };
    // Visibility enforcement: non-admin bez widocznosci dostaje NotFound (nie Forbidden —
    // nie zdradzamy ze addon istnieje).
    if !is_admin(ctx) {
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
    let admin_only =
        repository::get_addon_admin_only(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let permissions = repository::list_permission_catalog(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .map(|e| AddonPermissionDecl {
            permission_id: e.permission_id,
            display_name: e.display_name,
            description: e.description,
            risk: e.risk,
            sort_order: e.sort_order,
        })
        .collect();
    let oauth_providers = repository::list_oauth_providers_decl(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .map(|d| ProtoOAuthDecl {
            addon_id: d.addon_id,
            provider_id: d.provider_id,
            display_name: d.display_name,
            authorize_url: d.authorize_url,
            token_url: d.token_url,
            revoke_url: d.revoke_url,
            scopes: d
                .scopes
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            mode: d.mode,
            pkce: d.pkce,
        })
        .collect();
    let license =
        repository::get_addon_license(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let file_size_bytes =
        repository::get_addon_wasm_size(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let runtime =
        repository::get_addon_runtime(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let icon = repository::get_addon_icon(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let oauth_mode =
        repository::compute_addon_oauth_mode(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let (visibility_visible, visibility_total) =
        repository::count_visibility_groups(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let tools_count =
        repository::count_addon_tools(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let linked_accounts_count =
        repository::count_linked_accounts_for_addon(&ctx.state.db, &payload.addon_id)
            .map_err(db_err)?;
    let show_in_catalog =
        repository::get_addon_show_in_catalog(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    Ok(MessageBody::AddonDetailResponseBody(AddonDetailResponse {
        addon_id: addon.addon_id,
        name: addon.name,
        version: addon.version,
        description: addon.description,
        author: addon.author,
        is_enabled: addon.is_enabled,
        is_system: addon.is_system,
        admin_only,
        category: addon.category,
        permissions,
        oauth_providers,
        license,
        file_size_bytes,
        runtime,
        icon,
        oauth_mode,
        visibility_groups_visible: visibility_visible as i32,
        visibility_groups_total: visibility_total as i32,
        tools_count: tools_count as i32,
        linked_accounts_count: linked_accounts_count as i32,
        show_in_catalog,
    }))
}

// =============================================================================
// 2. AddonVisibilityListRequest — Admin
// =============================================================================

#[handler(variant = "AddonVisibilityListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_visibility_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonVisibilityListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonVisibilityListRequestBody",
            ))
        }
    };
    let rows = repository::list_addon_visibility(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .map(|r| ProtoVis {
            addon_id: r.addon_id,
            group_id: r.group_id,
            group_name: r.group_name,
            visible: r.visible,
            group_description: r.group_description,
            user_count: r.user_count,
        })
        .collect();
    let show_in_catalog =
        repository::get_addon_show_in_catalog(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    Ok(MessageBody::AddonVisibilityListResponseBody(
        AddonVisibilityListResponse {
            addon_id: payload.addon_id.clone(),
            rows,
            show_in_catalog,
        },
    ))
}

// =============================================================================
// 3. AddonVisibilitySetRequest — Admin
// =============================================================================

#[handler(variant = "AddonVisibilitySetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_visibility_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonVisibilitySetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonVisibilitySetRequestBody",
            ))
        }
    };
    // Snapshot starych wartosci przed upsert — potrzebne do audytu.
    let visible_old =
        repository::get_addon_visibility(&ctx.state.db, &payload.addon_id, payload.group_id)
            .map_err(db_err)?;
    let group_name = repository::get_group_name_by_id(&ctx.state.db, payload.group_id)
        .map_err(db_err)?
        .unwrap_or_default();
    let updated_by = current_user_id(ctx);
    repository::set_addon_visibility(
        &ctx.state.db,
        &payload.addon_id,
        payload.group_id,
        payload.visible,
        updated_by,
    )
    .map_err(db_err)?;
    audit(
        ctx,
        "addon_visibility_set",
        "addon",
        &payload.addon_id,
        serde_json::json!({
            "group_id": payload.group_id,
            "group_name": group_name,
            "visible_old": visible_old,
            "visible_new": payload.visible,
        }),
        "info",
    );
    addon_perm_broadcast::publish(AddonPermissionChangedEvent {
        addon_id: payload.addon_id.clone(),
        subject_type: Some("group".to_string()),
        subject_id: Some(payload.group_id),
        permission_id: None,
    });
    Ok(MessageBody::AddonVisibilitySetResponseBody(
        AddonVisibilitySetResponse {
            addon_id: payload.addon_id.clone(),
            group_id: payload.group_id,
            visible: payload.visible,
        },
    ))
}

// =============================================================================
// 4. AddonAdminOnlySetRequest — Admin
// =============================================================================

#[handler(variant = "AddonAdminOnlySetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_admin_only_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonAdminOnlySetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonAdminOnlySetRequestBody",
            ))
        }
    };
    let admin_only_old =
        repository::peek_addon_admin_only(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    repository::set_addon_admin_only(&ctx.state.db, &payload.addon_id, payload.admin_only)
        .map_err(db_err)?;
    audit(
        ctx,
        "addon_admin_only_set",
        "addon",
        &payload.addon_id,
        serde_json::json!({
            "admin_only_old": admin_only_old,
            "admin_only_new": payload.admin_only,
        }),
        "warning",
    );
    addon_perm_broadcast::publish(AddonPermissionChangedEvent {
        addon_id: payload.addon_id.clone(),
        subject_type: None,
        subject_id: None,
        permission_id: None,
    });
    Ok(MessageBody::AddonAdminOnlySetResponseBody(
        AddonAdminOnlySetResponse {
            addon_id: payload.addon_id.clone(),
            admin_only: payload.admin_only,
        },
    ))
}

// =============================================================================
// 5. AddonPermissionCatalogRequest — Admin
// =============================================================================

#[handler(variant = "AddonPermissionCatalogRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_permission_catalog(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonPermissionCatalogRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonPermissionCatalogRequestBody",
            ))
        }
    };
    let entries = repository::list_permission_catalog(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .map(|e| AddonPermissionDecl {
            permission_id: e.permission_id,
            display_name: e.display_name,
            description: e.description,
            risk: e.risk,
            sort_order: e.sort_order,
        })
        .collect();
    Ok(MessageBody::AddonPermissionCatalogResponseBody(
        AddonPermissionCatalogResponse {
            addon_id: payload.addon_id.clone(),
            entries,
        },
    ))
}

// =============================================================================
// 6. AddonPermissionMatrixRequest — Admin
// =============================================================================

#[handler(variant = "AddonPermissionMatrixRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_permission_matrix(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonPermissionMatrixRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonPermissionMatrixRequestBody",
            ))
        }
    };
    let (rows_db, defaults_db) =
        repository::list_permission_matrix(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let rows = rows_db
        .into_iter()
        .map(|r| ProtoRow {
            addon_id: r.addon_id,
            subject_type: r.subject_type,
            subject_id: r.subject_id,
            permission_id: r.permission_id,
            grant_mode: r.grant_mode,
            updated_at_epoch: parse_epoch(&r.updated_at),
        })
        .collect();
    let defaults = defaults_db
        .into_iter()
        .map(|d| ProtoDefault {
            addon_id: d.addon_id,
            permission_id: d.permission_id,
            grant_mode: d.grant_mode,
            updated_at_epoch: parse_epoch(&d.updated_at),
        })
        .collect();
    let (last_change_by, last_change_at_epoch) =
        match repository::last_permission_change(&ctx.state.db, &payload.addon_id)
            .map_err(db_err)?
        {
            Some((user, ts)) => (user, parse_epoch(&ts)),
            None => (String::new(), 0),
        };
    Ok(MessageBody::AddonPermissionMatrixResponseBody(
        AddonPermissionMatrixResponse {
            addon_id: payload.addon_id.clone(),
            rows,
            defaults,
            last_change_by,
            last_change_at_epoch,
        },
    ))
}

// =============================================================================
// 7. AddonPermissionSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonPermissionSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_permission_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonPermissionSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonPermissionSetRequestBody",
            ))
        }
    };
    if !matches!(payload.subject_type.as_str(), "user" | "group") {
        return Err(ProtocolError::bad_request(
            "subject_type musi byc 'user' lub 'group'",
        ));
    }
    if !matches!(payload.grant_mode.as_str(), "allow" | "deny" | "inherit") {
        return Err(ProtocolError::bad_request(
            "grant_mode musi byc allow|deny|inherit",
        ));
    }
    let grant_mode_old = repository::get_permission_grant_mode(
        &ctx.state.db,
        &payload.addon_id,
        &payload.subject_type,
        payload.subject_id,
        &payload.permission_id,
    )
    .map_err(db_err)?;
    let risk = repository::get_permission_catalog_risk(
        &ctx.state.db,
        &payload.addon_id,
        &payload.permission_id,
    )
    .map_err(db_err)?
    .unwrap_or_else(|| "low".to_string());
    let subject_name = match payload.subject_type.as_str() {
        "user" => repository::get_user_account_by_id(&ctx.state.db, payload.subject_id)
            .map_err(db_err)?
            .map(|u| u.username)
            .unwrap_or_default(),
        "group" => repository::get_group_name_by_id(&ctx.state.db, payload.subject_id)
            .map_err(db_err)?
            .unwrap_or_default(),
        _ => String::new(),
    };
    let updated_by = current_user_id(ctx);
    repository::upsert_permission(
        &ctx.state.db,
        &payload.addon_id,
        &payload.subject_type,
        payload.subject_id,
        &payload.permission_id,
        &payload.grant_mode,
        updated_by,
    )
    .map_err(db_err)?;
    let severity = if matches!(risk.as_str(), "high" | "critical") {
        "warning"
    } else {
        "info"
    };
    audit(
        ctx,
        "addon_permission_set",
        "addon",
        &payload.addon_id,
        serde_json::json!({
            "subject_type": payload.subject_type,
            "subject_id": payload.subject_id,
            "subject_name": subject_name,
            "permission_id": payload.permission_id,
            "grant_mode_old": grant_mode_old,
            "grant_mode_new": payload.grant_mode,
            "risk": risk,
        }),
        severity,
    );
    addon_perm_broadcast::publish(AddonPermissionChangedEvent {
        addon_id: payload.addon_id.clone(),
        subject_type: Some(payload.subject_type.clone()),
        subject_id: Some(payload.subject_id),
        permission_id: Some(payload.permission_id.clone()),
    });
    Ok(MessageBody::AddonPermissionSetResponseBody(
        AddonPermissionSetResponse {
            addon_id: payload.addon_id.clone(),
            subject_type: payload.subject_type.clone(),
            subject_id: payload.subject_id,
            permission_id: payload.permission_id.clone(),
            grant_mode: payload.grant_mode.clone(),
        },
    ))
}

// =============================================================================
// 8. AddonPermissionDefaultSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonPermissionDefaultSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_permission_default_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonPermissionDefaultSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonPermissionDefaultSetRequestBody",
            ))
        }
    };
    if !matches!(payload.grant_mode.as_str(), "allow" | "deny") {
        return Err(ProtocolError::bad_request("grant_mode musi byc allow|deny"));
    }
    let grant_mode_old = repository::get_permission_default_grant_mode(
        &ctx.state.db,
        &payload.addon_id,
        &payload.permission_id,
    )
    .map_err(db_err)?;
    let updated_by = current_user_id(ctx);
    repository::upsert_permission_default(
        &ctx.state.db,
        &payload.addon_id,
        &payload.permission_id,
        &payload.grant_mode,
        updated_by,
    )
    .map_err(db_err)?;
    audit(
        ctx,
        "addon_permission_default_set",
        "addon",
        &payload.addon_id,
        serde_json::json!({
            "permission_id": payload.permission_id,
            "grant_mode_old": grant_mode_old,
            "grant_mode_new": payload.grant_mode,
        }),
        "info",
    );
    addon_perm_broadcast::publish(AddonPermissionChangedEvent {
        addon_id: payload.addon_id.clone(),
        subject_type: None,
        subject_id: None,
        permission_id: Some(payload.permission_id.clone()),
    });
    Ok(MessageBody::AddonPermissionDefaultSetResponseBody(
        AddonPermissionDefaultSetResponse {
            addon_id: payload.addon_id.clone(),
            permission_id: payload.permission_id.clone(),
            grant_mode: payload.grant_mode.clone(),
        },
    ))
}

// =============================================================================
// 9. AddonPermissionCheckRequest — UserSession
// =============================================================================
// Kazdy moze pytac o WLASNE uprawnienia; nie-admin z innym user_id → odrzucenie.

#[handler(variant = "AddonPermissionCheckRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_permission_check(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonPermissionCheckRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonPermissionCheckRequestBody",
            ))
        }
    };
    let my_id = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "brak user_id w sesji")
    })?;
    let target_id = payload.user_id.unwrap_or(my_id);
    if target_id != my_id && !is_admin(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "mozna sprawdzac tylko wlasne uprawnienia",
        ));
    }
    // Visibility enforcement: non-admin pytajacy o ukryty addon dostaje NotFound.
    if !is_admin(ctx)
        && !repository::is_addon_visible_to_user(&ctx.state.db, &payload.addon_id, my_id)
            .map_err(db_err)?
    {
        return Err(ProtocolError::not_found("addon nie istnieje"));
    }
    let (allowed, reason) = repository::resolve_permission(
        &ctx.state.db,
        &payload.addon_id,
        &payload.permission_id,
        target_id,
    )
    .map_err(db_err)?;
    Ok(MessageBody::AddonPermissionCheckResponseBody(
        AddonPermissionCheckResponse {
            addon_id: payload.addon_id.clone(),
            permission_id: payload.permission_id.clone(),
            allowed,
            reason,
        },
    ))
}

// =============================================================================
// 10. AddonShowInCatalogSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonShowInCatalogSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_show_in_catalog_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonShowInCatalogSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonShowInCatalogSetRequestBody",
            ))
        }
    };
    let old_value =
        repository::get_addon_show_in_catalog(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    repository::set_addon_show_in_catalog(
        &ctx.state.db,
        &payload.addon_id,
        payload.show_in_catalog,
    )
    .map_err(db_err)?;
    audit(
        ctx,
        "addon_show_in_catalog_set",
        "addon",
        &payload.addon_id,
        serde_json::json!({
            "show_in_catalog_old": old_value,
            "show_in_catalog_new": payload.show_in_catalog,
        }),
        "info",
    );
    addon_perm_broadcast::publish(AddonPermissionChangedEvent {
        addon_id: payload.addon_id.clone(),
        subject_type: None,
        subject_id: None,
        permission_id: None,
    });
    Ok(MessageBody::AddonShowInCatalogSetResponseBody(
        AddonShowInCatalogSetResponse {
            addon_id: payload.addon_id.clone(),
            show_in_catalog: payload.show_in_catalog,
        },
    ))
}
