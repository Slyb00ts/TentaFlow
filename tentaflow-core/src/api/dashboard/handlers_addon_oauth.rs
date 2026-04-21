// =============================================================================
// Plik: api/dashboard/handlers_addon_oauth.rs
// Opis: Handlery binary protocol dla OAuth addonow: config list/set/clear,
//       authorize start, linked accounts, revoke, reauthorize. Secret NIGDY
//       nie trafia do response (tylko `client_secret_set: bool`).
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    AddonOAuthAuthorizeStartResponse, AddonOAuthConfigClearSecretResponse,
    AddonOAuthConfigListResponse, AddonOAuthConfigRow as ProtoConfig, AddonOAuthConfigSetResponse,
    AddonOAuthLinkedAccountsResponse, AddonOAuthReauthorizeResponse, AddonOAuthRevokeResponse,
    AddonOAuthTestConnectionResponse, MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
    UserOAuthAccountRow as ProtoAccount,
};

use crate::addon::{oauth, oauth_crypto};
use crate::db::repository;
use crate::dispatch::HandlerContext;

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

fn parse_epoch(s: &str) -> u64 {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return t.timestamp() as u64;
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return t.and_utc().timestamp() as u64;
    }
    0
}

fn parse_epoch_opt(s: &Option<String>) -> Option<u64> {
    s.as_deref().map(parse_epoch)
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

/// Wpis audytowy dla akcji OAuth — resource_id ma format "{addon_id}:{provider_id}"
/// albo `{account_id}` dla konta indywidualnego.
fn audit(
    ctx: &HandlerContext,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    addon_id_hint: Option<&str>,
    details_json: serde_json::Value,
    severity: &str,
) {
    let user_id = current_user_id(ctx);
    let details = details_json.to_string();
    let node_id = ctx.state.local_node_id.as_ref();
    if let Err(e) = repository::log_audit_full(
        &ctx.state.db,
        user_id,
        addon_id_hint,
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

fn to_proto_account(a: repository::DbUserOAuthAccount) -> ProtoAccount {
    ProtoAccount {
        id: a.id,
        user_id: a.user_id,
        addon_id: a.addon_id,
        provider_id: a.provider_id,
        external_account_id: a.external_account_id,
        display_name: a.display_name,
        token_type: a.token_type,
        scopes: a
            .scopes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        expires_at_epoch: parse_epoch_opt(&a.expires_at),
        created_at_epoch: parse_epoch(&a.created_at),
        last_used_at_epoch: parse_epoch_opt(&a.last_used_at),
        revoked: a.revoked,
    }
}

// =============================================================================
// 10. AddonOAuthConfigListRequest — Admin
// =============================================================================

#[handler(variant = "AddonOAuthConfigListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_oauth_config_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthConfigListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthConfigListRequestBody",
            ))
        }
    };
    // Pobieramy wszystkie konta dla addona raz i grupujemy per provider_id —
    // unikamy N+1 zapytan przy wielu providerach.
    let all_accounts =
        repository::list_user_oauth_accounts_for_addon(&ctx.state.db, &payload.addon_id)
            .map_err(db_err)?;
    let rows = repository::list_oauth_config(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .map(|c| {
            let linked_accounts_count = all_accounts
                .iter()
                .filter(|a| a.provider_id == c.provider_id && !a.revoked)
                .count() as i32;
            let shared_account_email = if c.oauth_mode == "global" {
                all_accounts
                    .iter()
                    .find(|a| a.provider_id == c.provider_id && a.user_id.is_none() && !a.revoked)
                    .map(|a| a.display_name.clone())
            } else {
                None
            };
            ProtoConfig {
                addon_id: c.addon_id,
                provider_id: c.provider_id,
                client_id: c.client_id,
                client_secret_set: c.client_secret_encrypted.is_some(),
                redirect_uri: c.redirect_uri,
                enabled: c.enabled,
                updated_at_epoch: parse_epoch(&c.updated_at),
                oauth_mode: c.oauth_mode,
                linked_accounts_count,
                shared_account_email,
            }
        })
        .collect();
    Ok(MessageBody::AddonOAuthConfigListResponseBody(
        AddonOAuthConfigListResponse {
            addon_id: payload.addon_id.clone(),
            configs: rows,
        },
    ))
}

// =============================================================================
// 11. AddonOAuthConfigSetRequest — Admin
// =============================================================================

#[handler(variant = "AddonOAuthConfigSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn addon_oauth_config_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthConfigSetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthConfigSetRequestBody",
            ))
        }
    };
    // Walidacja nowego pola oauth_mode — tylko 3 dozwolone wartosci.
    if !matches!(
        payload.oauth_mode.as_str(),
        "global" | "individual" | "none"
    ) {
        return Err(ProtocolError::bad_request(
            "oauth_mode musi byc global|individual|none",
        ));
    }
    // Snapshot starej konfiguracji przed zmiana (do audytu: client_id/redirect_uri/enabled/oauth_mode OLD).
    let old_cfg =
        repository::get_oauth_config(&ctx.state.db, &payload.addon_id, &payload.provider_id)
            .map_err(db_err)?;
    let encrypted = if let Some(secret) = &payload.client_secret {
        if secret.is_empty() {
            None
        } else {
            let key = oauth_crypto::ensure_master_key(&ctx.state.db).map_err(db_err)?;
            Some(oauth_crypto::encrypt(&key, secret.as_bytes()).map_err(db_err)?)
        }
    } else {
        None
    };
    let updated_by = current_user_id(ctx);
    repository::upsert_oauth_config(
        &ctx.state.db,
        &payload.addon_id,
        &payload.provider_id,
        &payload.client_id,
        encrypted.as_deref(),
        &payload.redirect_uri,
        payload.enabled,
        updated_by,
        &payload.oauth_mode,
    )
    .map_err(db_err)?;
    let now_set =
        repository::get_oauth_config(&ctx.state.db, &payload.addon_id, &payload.provider_id)
            .map_err(db_err)?
            .map(|c| c.client_secret_encrypted.is_some())
            .unwrap_or(false);
    // Audit — NIGDY nie logujemy plaintext sekretu, tylko flage zmiany.
    let resource_id = format!("{}:{}", payload.addon_id, payload.provider_id);
    audit(
        ctx,
        "addon_oauth_config_set",
        "addon_oauth",
        &resource_id,
        Some(&payload.addon_id),
        serde_json::json!({
            "client_id_old": old_cfg.as_ref().map(|c| c.client_id.clone()),
            "client_id_new": payload.client_id,
            "redirect_uri_old": old_cfg.as_ref().map(|c| c.redirect_uri.clone()),
            "redirect_uri_new": payload.redirect_uri,
            "enabled_old": old_cfg.as_ref().map(|c| c.enabled),
            "enabled_new": payload.enabled,
            "oauth_mode_old": old_cfg.as_ref().map(|c| c.oauth_mode.clone()),
            "oauth_mode_new": payload.oauth_mode,
            "secret_changed": payload.client_secret.as_ref().map(|s| !s.is_empty()).unwrap_or(false),
        }),
        "warning",
    );
    Ok(MessageBody::AddonOAuthConfigSetResponseBody(
        AddonOAuthConfigSetResponse {
            addon_id: payload.addon_id.clone(),
            provider_id: payload.provider_id.clone(),
            client_secret_set: now_set,
            enabled: payload.enabled,
        },
    ))
}

// =============================================================================
// 12. AddonOAuthConfigClearSecretRequest — Admin
// =============================================================================

#[handler(variant = "AddonOAuthConfigClearSecretRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_oauth_config_clear_secret(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthConfigClearSecretRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthConfigClearSecretRequestBody",
            ))
        }
    };
    let cleared = repository::clear_oauth_config_secret(
        &ctx.state.db,
        &payload.addon_id,
        &payload.provider_id,
    )
    .map_err(db_err)?;
    if cleared {
        let resource_id = format!("{}:{}", payload.addon_id, payload.provider_id);
        audit(
            ctx,
            "addon_oauth_config_clear_secret",
            "addon_oauth",
            &resource_id,
            Some(&payload.addon_id),
            serde_json::json!({ "cleared": true }),
            "warning",
        );
    }
    Ok(MessageBody::AddonOAuthConfigClearSecretResponseBody(
        AddonOAuthConfigClearSecretResponse {
            addon_id: payload.addon_id.clone(),
            provider_id: payload.provider_id.clone(),
            cleared,
        },
    ))
}

// =============================================================================
// 13. AddonOAuthAuthorizeStartRequest — UserSession (individual) / Admin (global)
// =============================================================================

#[handler(variant = "AddonOAuthAuthorizeStartRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_oauth_authorize_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthAuthorizeStartRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthAuthorizeStartRequestBody",
            ))
        }
    };
    let user_id = current_user_id(ctx);
    let cfg = repository::get_oauth_config(&ctx.state.db, &payload.addon_id, &payload.provider_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::bad_request("OAuth nie skonfigurowany — brak wpisu config")
        })?;
    if !cfg.enabled || cfg.client_id.is_empty() {
        return Err(ProtocolError::bad_request(
            "OAuth wylaczony lub brak client_id",
        ));
    }
    // Tryb OAuth pochodzi z konfiguracji admina, NIE z requestu usera.
    // Uzytkownik nie moze wymusic trybu; admin decyduje czy addon dziala global/individual/none.
    let effective_mode = cfg.oauth_mode.as_str();
    if effective_mode == "none" {
        return Err(ProtocolError::bad_request(
            "addon skonfigurowany bez OAuth (oauth_mode=none)",
        ));
    }
    if !matches!(effective_mode, "global" | "individual") {
        return Err(ProtocolError::internal(format!(
            "nieznany oauth_mode w config: {}",
            effective_mode
        )));
    }
    if effective_mode == "global" && !is_admin(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "global OAuth moze zainicjowac tylko admin",
        ));
    }
    let decls =
        repository::list_oauth_providers_decl(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let decl = decls
        .into_iter()
        .find(|d| d.provider_id == payload.provider_id)
        .ok_or_else(|| ProtocolError::not_found("provider nie zadeklarowany w manifescie"))?;
    let state = oauth::generate_state();
    let (verifier, challenge) = if decl.pkce {
        let v = oauth::generate_code_verifier();
        let c = oauth::code_challenge_from_verifier(&v);
        (v, Some(c))
    } else {
        (String::new(), None)
    };
    let store_user_id = if effective_mode == "individual" {
        user_id
    } else {
        None
    };
    repository::insert_oauth_state(
        &ctx.state.db,
        &state,
        store_user_id,
        &payload.addon_id,
        &payload.provider_id,
        effective_mode,
        &verifier,
        payload.redirect_after.as_deref().unwrap_or(""),
        600,
    )
    .map_err(db_err)?;
    let url = oauth::build_authorize_url(&cfg, &decl, &state, challenge.as_deref());
    Ok(MessageBody::AddonOAuthAuthorizeStartResponseBody(
        AddonOAuthAuthorizeStartResponse {
            authorize_url: url,
            state,
        },
    ))
}

// =============================================================================
// 14. AddonOAuthLinkedAccountsRequest — Admin (scope=all) / UserSession (scope=mine)
// =============================================================================

#[handler(variant = "AddonOAuthLinkedAccountsRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_oauth_linked_accounts(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthLinkedAccountsRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthLinkedAccountsRequestBody",
            ))
        }
    };
    let accounts = match payload.scope.as_str() {
        "all" => {
            if !is_admin(ctx) {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "scope=all wymaga admin",
                ));
            }
            repository::list_user_oauth_accounts_for_addon(&ctx.state.db, &payload.addon_id)
                .map_err(db_err)?
        }
        "mine" => {
            let uid = current_user_id(ctx).ok_or_else(|| {
                ProtocolError::new(ProtocolErrorCode::AuthRequired, "brak user_id")
            })?;
            repository::list_user_oauth_accounts_for_user(&ctx.state.db, uid)
                .map_err(db_err)?
                .into_iter()
                .filter(|a| a.addon_id == payload.addon_id)
                .collect()
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "scope musi byc 'all' lub 'mine'",
            ))
        }
    };
    let accounts: Vec<ProtoAccount> = accounts.into_iter().map(to_proto_account).collect();
    Ok(MessageBody::AddonOAuthLinkedAccountsResponseBody(
        AddonOAuthLinkedAccountsResponse {
            addon_id: payload.addon_id.clone(),
            accounts,
        },
    ))
}

// =============================================================================
// 15. AddonOAuthRevokeRequest — UserSession (admin lub wlasciciel)
// =============================================================================

#[handler(variant = "AddonOAuthRevokeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn addon_oauth_revoke(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthRevokeRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthRevokeRequestBody",
            ))
        }
    };
    let account = repository::get_oauth_account_by_id(&ctx.state.db, payload.account_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("konto OAuth nie istnieje"))?;
    let uid = current_user_id(ctx);
    let own = account.user_id.is_some() && account.user_id == uid;
    if !own && !is_admin(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "tylko wlasciciel lub admin moze revoke",
        ));
    }
    // Best-effort revoke u providera (nie blokuje gdy brak revoke_url / blad sieci).
    if let (Some(cfg), Ok(decls), Some(blob)) = (
        repository::get_oauth_config(&ctx.state.db, &account.addon_id, &account.provider_id)
            .ok()
            .flatten(),
        repository::list_oauth_providers_decl(&ctx.state.db, &account.addon_id),
        account.access_token_encrypted.as_deref(),
    ) {
        if let Some(decl) = decls
            .into_iter()
            .find(|d| d.provider_id == account.provider_id)
        {
            if let Ok(key) = oauth_crypto::ensure_master_key(&ctx.state.db) {
                if let Ok(plain) = oauth_crypto::decrypt(&key, blob) {
                    if let Ok(token) = String::from_utf8(plain) {
                        let client_secret = cfg
                            .client_secret_encrypted
                            .as_deref()
                            .and_then(|b| oauth_crypto::decrypt(&key, b).ok())
                            .and_then(|v| String::from_utf8(v).ok())
                            .unwrap_or_default();
                        let _ = oauth::revoke_token(&cfg, &decl, &client_secret, &token).await;
                    }
                }
            }
        }
    }
    let revoked =
        repository::revoke_oauth_account(&ctx.state.db, payload.account_id).map_err(db_err)?;
    if revoked {
        let target_email = match account.user_id {
            Some(uid) => repository::get_user_email_by_id(&ctx.state.db, uid)
                .map_err(db_err)?
                .unwrap_or_default(),
            None => String::new(),
        };
        let resource_id = payload.account_id.to_string();
        audit(
            ctx,
            "addon_oauth_account_revoke",
            "user_oauth_account",
            &resource_id,
            Some(&account.addon_id),
            serde_json::json!({
                "addon_id": account.addon_id,
                "provider_id": account.provider_id,
                "target_user_id": account.user_id,
                "target_user_email": target_email,
                "revoked_by": uid,
            }),
            "info",
        );
    }
    Ok(MessageBody::AddonOAuthRevokeResponseBody(
        AddonOAuthRevokeResponse {
            account_id: payload.account_id,
            revoked,
        },
    ))
}

// =============================================================================
// 16. AddonOAuthReauthorizeRequest — UserSession (wlasciciel)
// =============================================================================

#[handler(variant = "AddonOAuthReauthorizeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_oauth_reauthorize(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthReauthorizeRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthReauthorizeRequestBody",
            ))
        }
    };
    let account = repository::get_oauth_account_by_id(&ctx.state.db, payload.account_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("konto OAuth nie istnieje"))?;
    let uid = current_user_id(ctx);
    let own = account.user_id.is_some() && account.user_id == uid;
    if !own && !is_admin(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "tylko wlasciciel lub admin moze reauth",
        ));
    }
    let cfg = repository::get_oauth_config(&ctx.state.db, &account.addon_id, &account.provider_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::bad_request("brak config OAuth"))?;
    let decl = repository::list_oauth_providers_decl(&ctx.state.db, &account.addon_id)
        .map_err(db_err)?
        .into_iter()
        .find(|d| d.provider_id == account.provider_id)
        .ok_or_else(|| ProtocolError::not_found("brak deklaracji providera"))?;
    let state = oauth::generate_state();
    let (verifier, challenge) = if decl.pkce {
        let v = oauth::generate_code_verifier();
        let c = oauth::code_challenge_from_verifier(&v);
        (v, Some(c))
    } else {
        (String::new(), None)
    };
    let mode = if account.user_id.is_some() {
        "individual"
    } else {
        "global"
    };
    repository::insert_oauth_state(
        &ctx.state.db,
        &state,
        account.user_id,
        &account.addon_id,
        &account.provider_id,
        mode,
        &verifier,
        "",
        600,
    )
    .map_err(db_err)?;
    let url = oauth::build_authorize_url(&cfg, &decl, &state, challenge.as_deref());
    Ok(MessageBody::AddonOAuthReauthorizeResponseBody(
        AddonOAuthReauthorizeResponse {
            authorize_url: url,
            state,
        },
    ))
}

// =============================================================================
// 17. AddonOAuthTestConnectionRequest - Admin
// =============================================================================
//
// Probes the provider with the currently stored token for (addon_id, provider_id).
// Admin-only: tests either the global token (oauth_mode=global) or the admin's
// own individual token (oauth_mode=individual). Never tests other users' tokens.

#[handler(variant = "AddonOAuthTestConnectionRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn addon_oauth_test_connection(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonOAuthTestConnectionRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonOAuthTestConnectionRequestBody",
            ))
        }
    };

    let cfg_opt =
        repository::get_oauth_config(&ctx.state.db, &payload.addon_id, &payload.provider_id)
            .map_err(db_err)?;
    let cfg = match cfg_opt {
        Some(c) if c.enabled => c,
        _ => {
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some("not_configured".to_string()),
                    account_email: None,
                },
            ))
        }
    };

    if cfg.oauth_mode == "none" {
        return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
            AddonOAuthTestConnectionResponse {
                ok: false,
                message: Some("disabled".to_string()),
                account_email: None,
            },
        ));
    }

    // Pick target user_id: global -> NULL, individual -> admin's own id.
    let admin_uid = current_user_id(ctx);
    let target_user_id: Option<i64> = match cfg.oauth_mode.as_str() {
        "global" => None,
        "individual" => match admin_uid {
            Some(uid) => Some(uid),
            None => {
                return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                    AddonOAuthTestConnectionResponse {
                        ok: false,
                        message: Some("individual_no_admin_account".to_string()),
                        account_email: None,
                    },
                ))
            }
        },
        _ => {
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some("disabled".to_string()),
                    account_email: None,
                },
            ))
        }
    };

    // Find matching account row.
    let accounts = match target_user_id {
        Some(uid) => repository::list_user_oauth_accounts_for_user(&ctx.state.db, uid)
            .map_err(db_err)?
            .into_iter()
            .filter(|a| a.addon_id == payload.addon_id && a.provider_id == payload.provider_id)
            .collect::<Vec<_>>(),
        None => repository::list_user_oauth_accounts_for_addon(&ctx.state.db, &payload.addon_id)
            .map_err(db_err)?
            .into_iter()
            .filter(|a| a.user_id.is_none() && a.provider_id == payload.provider_id)
            .collect::<Vec<_>>(),
    };
    let account = match accounts.into_iter().find(|a| !a.revoked) {
        Some(a) => a,
        None => {
            let msg = match cfg.oauth_mode.as_str() {
                "individual" => "individual_no_admin_account",
                _ => "no_token",
            };
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some(msg.to_string()),
                    account_email: None,
                },
            ));
        }
    };

    let key = oauth_crypto::ensure_master_key(&ctx.state.db).map_err(db_err)?;
    let access_blob = match account.access_token_encrypted.as_deref() {
        Some(b) => b,
        None => {
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some("no_token".to_string()),
                    account_email: None,
                },
            ))
        }
    };
    let access_token = oauth_crypto::decrypt(&key, access_blob)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default();
    if access_token.is_empty() {
        return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
            AddonOAuthTestConnectionResponse {
                ok: false,
                message: Some("no_token".to_string()),
                account_email: None,
            },
        ));
    }

    // Attempt probe with current token; on failure, try one refresh + retry.
    match oauth::fetch_userinfo(&payload.provider_id, &access_token).await {
        Ok((_id, name)) if !name.is_empty() => {
            let _ = repository::touch_oauth_last_used(&ctx.state.db, account.id);
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: true,
                    message: None,
                    account_email: Some(name),
                },
            ));
        }
        _ => {}
    }

    // Retry path: refresh then probe once more.
    let refresh_blob = account.refresh_token_encrypted.as_deref();
    let refresh_plain = refresh_blob
        .and_then(|b| oauth_crypto::decrypt(&key, b).ok())
        .and_then(|v| String::from_utf8(v).ok());
    let Some(refresh_token) = refresh_plain else {
        return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
            AddonOAuthTestConnectionResponse {
                ok: false,
                message: Some("userinfo_failed".to_string()),
                account_email: None,
            },
        ));
    };

    let decl = match repository::list_oauth_providers_decl(&ctx.state.db, &payload.addon_id)
        .map_err(db_err)?
        .into_iter()
        .find(|d| d.provider_id == payload.provider_id)
    {
        Some(d) => d,
        None => {
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some("not_configured".to_string()),
                    account_email: None,
                },
            ))
        }
    };
    let client_secret = cfg
        .client_secret_encrypted
        .as_deref()
        .and_then(|b| oauth_crypto::decrypt(&key, b).ok())
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    let tokens = match oauth::refresh_token(&cfg, &decl, &client_secret, &refresh_token).await {
        Ok(t) => t,
        Err(e) => {
            return Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: false,
                    message: Some(format!("refresh_failed: {}", e)),
                    account_email: None,
                },
            ))
        }
    };

    // Persist refreshed tokens.
    let new_access = oauth_crypto::encrypt(&key, tokens.access_token.as_bytes()).map_err(db_err)?;
    let new_refresh = match tokens.refresh_token.as_deref() {
        Some(rt) => Some(oauth_crypto::encrypt(&key, rt.as_bytes()).map_err(db_err)?),
        None => None,
    };
    let new_exp = tokens.expires_in_secs.map(|s| {
        (chrono::Utc::now() + chrono::Duration::seconds(s as i64))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    });
    let _ = repository::upsert_user_oauth_account(
        &ctx.state.db,
        account.user_id,
        &payload.addon_id,
        &payload.provider_id,
        &account.external_account_id,
        &account.display_name,
        &new_access,
        new_refresh.as_deref(),
        &tokens.token_type,
        tokens.scope.as_deref().unwrap_or(&account.scopes),
        new_exp.as_deref(),
    );

    match oauth::fetch_userinfo(&payload.provider_id, &tokens.access_token).await {
        Ok((_id, name)) if !name.is_empty() => {
            let _ = repository::touch_oauth_last_used(&ctx.state.db, account.id);
            Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
                AddonOAuthTestConnectionResponse {
                    ok: true,
                    message: None,
                    account_email: Some(name),
                },
            ))
        }
        Ok(_) | Err(_) => Ok(MessageBody::AddonOAuthTestConnectionResponseBody(
            AddonOAuthTestConnectionResponse {
                ok: false,
                message: Some("userinfo_failed".to_string()),
                account_email: None,
            },
        )),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{self, state::AppState};
    use tentaflow_protocol::{AddonOAuthTestConnectionRequest, ProtocolErrorCode, SessionAuth};

    /// Non-admin sessions must be rejected by dispatch-level policy check,
    /// not by the handler logic.
    #[tokio::test]
    async fn test_oauth_test_connection_admin_only_policy() {
        let ctx_user = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("user".to_string()),
            },
            correlation_id: 1,
            resume_secret: None,
            state: AppState::for_test(),
        };
        let body =
            MessageBody::AddonOAuthTestConnectionRequestBody(AddonOAuthTestConnectionRequest {
                addon_id: "any".into(),
                provider_id: "microsoft".into(),
            });
        let (resp, is_err) = dispatch::dispatch(&body, &ctx_user).await;
        assert!(is_err, "non-admin session must fail policy");
        match resp {
            MessageBody::Error(e) => {
                assert_eq!(e.code, ProtocolErrorCode::PolicyDenied);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_oauth_test_connection_not_configured_returns_ok_false() {
        let ctx_admin = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            resume_secret: None,
            state: AppState::for_test(),
        };
        let body =
            MessageBody::AddonOAuthTestConnectionRequestBody(AddonOAuthTestConnectionRequest {
                addon_id: "missing-addon".into(),
                provider_id: "microsoft".into(),
            });
        let (resp, is_err) = dispatch::dispatch(&body, &ctx_admin).await;
        assert!(!is_err, "handler returned policy error: {:?}", resp);
        match resp {
            MessageBody::AddonOAuthTestConnectionResponseBody(r) => {
                assert!(!r.ok);
                assert_eq!(r.message.as_deref(), Some("not_configured"));
            }
            other => panic!("expected TestConnectionResponse, got {:?}", other),
        }
    }
}
