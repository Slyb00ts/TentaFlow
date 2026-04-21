// ============ File: oauth.rs - OAuth token retrieval host function for WASM addons ============
//
// Permission id: "oauth" (resource = provider_id).
// ABI: oauth_get_token(provider_ptr, provider_len, out_ptr, out_cap, out_len_ptr) -> i32
// Output: JSON `{ "access_token": "...", "token_type": "Bearer",
//                 "expires_at": <u64 epoch | null>, "scopes": ["..."] }`.
//
// Behavior:
//  - Resolves the right user_oauth_accounts row for (addon_id, provider_id)
//    based on `addon_oauth_config.oauth_mode`:
//      - "global"     -> user_id IS NULL
//      - "individual" -> user_id = state.user_id
//      - "none"       -> permission denied
//  - Decrypts access_token (and refresh_token if present) with oauth_crypto.
//  - If access_token is expired (or within 60s), acquires the per-account
//    refresh guard mutex, re-reads the DB row (another caller may have just
//    refreshed), and if still stale calls the provider's refresh endpoint.
//  - On refresh success, persists the new tokens and returns the new access_token.
//  - On refresh failure, marks the account revoked and returns an error.
//  - touch_oauth_last_used on every successful return.
//
// Never logs plaintext tokens. Audit records include provider_id + addon_id only.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;

use super::{
    audit_log, check_permission, get_memory, read_guest_string, write_guest_output, AddonState,
    WasmCaller, ABI_ERR_NOT_FOUND, ABI_ERR_OPERATION, ABI_ERR_PERMISSION,
};
use crate::addon::{oauth, oauth_crypto};
use crate::db::repository;

/// Refresh when remaining lifetime is below this many seconds.
const REFRESH_SKEW_SECS: i64 = 60;

#[derive(Serialize)]
struct OAuthTokenOut {
    access_token: String,
    token_type: String,
    expires_at: Option<u64>,
    scopes: Vec<String>,
}

/// Parses a DB datetime (RFC3339 or "YYYY-MM-DD HH:MM:SS") into a UTC epoch.
fn parse_expires_at(s: &str) -> Option<i64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp());
    }
    if let Ok(t) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(t.and_utc().timestamp());
    }
    None
}

/// Computes the DB-format expires_at string from a relative TTL.
fn format_expires_at(expires_in_secs: u64) -> String {
    let t = Utc::now() + chrono::Duration::seconds(expires_in_secs as i64);
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Host function: returns the caller addon's current OAuth access token for
/// `provider_id`, refreshing transparently when expired.
pub fn oauth_get_token(
    mut caller: WasmCaller<'_, AddonState>,
    provider_ptr: i32,
    provider_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return ABI_ERR_OPERATION,
    };

    let provider_id = match read_guest_string(&memory, &caller, provider_ptr, provider_len) {
        Some(s) => s.to_string(),
        None => return ABI_ERR_OPERATION,
    };

    if !check_permission(caller.data(), "oauth", Some(&provider_id)) {
        audit_log(
            caller.data(),
            "oauth.get_token",
            Some("oauth"),
            Some(&provider_id),
            "denied",
            None,
        );
        return ABI_ERR_PERMISSION;
    }

    let addon_id = caller.data().addon_id.clone();
    let user_id = caller.data().user_id;

    // Resolve token blob + account row. The whole resolve/refresh flow is run
    // in a helper to keep ABI logic clean; map errors to audit entries + ABI codes.
    let outcome = resolve_and_refresh(caller.data(), &addon_id, &provider_id, user_id);

    match outcome {
        Ok(out) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "ok",
                None,
            );
            let json = match serde_json::to_vec(&out) {
                Ok(j) => j,
                Err(e) => {
                    let msg = format!("serialize: {}", e);
                    audit_log(
                        caller.data(),
                        "oauth.get_token",
                        Some("oauth"),
                        Some(&provider_id),
                        "error",
                        Some(&msg),
                    );
                    return ABI_ERR_OPERATION;
                }
            };
            write_guest_output(&memory, &mut caller, out_ptr, out_cap, out_len_ptr, &json)
        }
        Err(HostOAuthError::NotConfigured) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "error",
                Some("not_configured"),
            );
            ABI_ERR_NOT_FOUND
        }
        Err(HostOAuthError::ModeDenied) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "denied",
                Some("oauth_mode=none"),
            );
            ABI_ERR_PERMISSION
        }
        Err(HostOAuthError::AccountMissing) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "error",
                Some("account_missing"),
            );
            ABI_ERR_NOT_FOUND
        }
        Err(HostOAuthError::RefreshFailed(reason)) => {
            let msg = format!("refresh_failed: {}", reason);
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "error",
                Some(&msg),
            );
            ABI_ERR_OPERATION
        }
        Err(HostOAuthError::ExpiredNoRefresh) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "error",
                Some("expired_no_refresh_token"),
            );
            ABI_ERR_OPERATION
        }
        Err(HostOAuthError::Internal(msg)) => {
            audit_log(
                caller.data(),
                "oauth.get_token",
                Some("oauth"),
                Some(&provider_id),
                "error",
                Some(&msg),
            );
            ABI_ERR_OPERATION
        }
    }
}

#[derive(Debug)]
pub(crate) enum HostOAuthError {
    NotConfigured,
    ModeDenied,
    AccountMissing,
    ExpiredNoRefresh,
    RefreshFailed(String),
    Internal(String),
}

/// Core resolve/refresh flow. Extracted for unit testing without WASM.
pub(crate) fn resolve_and_refresh(
    state: &AddonState,
    addon_id: &str,
    provider_id: &str,
    user_id: Option<i64>,
) -> Result<OAuthTokenOutPublic, HostOAuthError> {
    let cfg = repository::get_oauth_config(&state.db, addon_id, provider_id)
        .map_err(|e| HostOAuthError::Internal(format!("get_oauth_config: {}", e)))?
        .ok_or(HostOAuthError::NotConfigured)?;

    if !cfg.enabled {
        return Err(HostOAuthError::NotConfigured);
    }

    let target_user_id = match cfg.oauth_mode.as_str() {
        "global" => None,
        "individual" => match user_id {
            Some(_) => user_id,
            None => return Err(HostOAuthError::AccountMissing),
        },
        "none" | _ => return Err(HostOAuthError::ModeDenied),
    };

    let account = find_account(&state.db, target_user_id, addon_id, provider_id)
        .map_err(|e| HostOAuthError::Internal(format!("find_account: {}", e)))?
        .ok_or(HostOAuthError::AccountMissing)?;

    if account.revoked {
        return Err(HostOAuthError::AccountMissing);
    }

    let master_key = oauth_crypto::ensure_master_key(&state.db)
        .map_err(|e| HostOAuthError::Internal(format!("master_key: {}", e)))?;

    let access_blob = account
        .access_token_encrypted
        .as_deref()
        .ok_or_else(|| HostOAuthError::Internal("missing access_token blob".into()))?;
    let access_token = oauth_crypto::decrypt(&master_key, access_blob)
        .map_err(|e| HostOAuthError::Internal(format!("decrypt access: {}", e)))
        .and_then(|b| {
            String::from_utf8(b)
                .map_err(|e| HostOAuthError::Internal(format!("access utf8: {}", e)))
        })?;

    let now = Utc::now().timestamp();
    let expires_at_epoch = account.expires_at.as_deref().and_then(parse_expires_at);
    let needs_refresh = match expires_at_epoch {
        Some(e) => e - now < REFRESH_SKEW_SECS,
        None => false,
    };

    if !needs_refresh {
        let _ = repository::touch_oauth_last_used(&state.db, account.id);
        return Ok(OAuthTokenOutPublic {
            access_token,
            token_type: account.token_type,
            expires_at: expires_at_epoch.map(|e| e as u64),
            scopes: parse_scopes(&account.scopes),
        });
    }

    let refresh_blob = account.refresh_token_encrypted.as_deref();
    let refresh_token = match refresh_blob {
        Some(b) => oauth_crypto::decrypt(&master_key, b)
            .map_err(|e| HostOAuthError::Internal(format!("decrypt refresh: {}", e)))
            .and_then(|bs| {
                String::from_utf8(bs)
                    .map_err(|e| HostOAuthError::Internal(format!("refresh utf8: {}", e)))
            })?,
        None => return Err(HostOAuthError::ExpiredNoRefresh),
    };

    // Serialize concurrent refresh for this account so only one caller hits
    // the provider. The second caller will re-read the freshly updated row
    // and typically skip the HTTP call.
    let mutex = state.oauth_refresh_guard.mutex_for(account.id);
    let _guard = mutex.lock();

    // Re-read after acquiring the lock - another caller may have just refreshed.
    let fresh = find_account(&state.db, target_user_id, addon_id, provider_id)
        .map_err(|e| HostOAuthError::Internal(format!("refetch: {}", e)))?
        .ok_or(HostOAuthError::AccountMissing)?;
    if fresh.revoked {
        return Err(HostOAuthError::AccountMissing);
    }
    let fresh_exp = fresh.expires_at.as_deref().and_then(parse_expires_at);
    let still_stale = match fresh_exp {
        Some(e) => e - now < REFRESH_SKEW_SECS,
        None => true,
    };
    if !still_stale {
        if let Some(blob) = fresh.access_token_encrypted.as_deref() {
            if let Ok(plain) = oauth_crypto::decrypt(&master_key, blob) {
                if let Ok(tok) = String::from_utf8(plain) {
                    let _ = repository::touch_oauth_last_used(&state.db, fresh.id);
                    return Ok(OAuthTokenOutPublic {
                        access_token: tok,
                        token_type: fresh.token_type,
                        expires_at: fresh_exp.map(|e| e as u64),
                        scopes: parse_scopes(&fresh.scopes),
                    });
                }
            }
        }
    }

    // Provider decl + client_secret for refresh call.
    let decl = repository::list_oauth_providers_decl(&state.db, addon_id)
        .map_err(|e| HostOAuthError::Internal(format!("list_decl: {}", e)))?
        .into_iter()
        .find(|d| d.provider_id == provider_id)
        .ok_or_else(|| HostOAuthError::Internal("no provider decl".into()))?;

    let client_secret = cfg
        .client_secret_encrypted
        .as_deref()
        .and_then(|b| oauth_crypto::decrypt(&master_key, b).ok())
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();

    let tokens = match oauth::refresh_token_blocking(&cfg, &decl, &client_secret, &refresh_token) {
        Ok(t) => t,
        Err(e) => {
            let _ = repository::revoke_oauth_account(&state.db, account.id);
            return Err(HostOAuthError::RefreshFailed(e.to_string()));
        }
    };

    let new_access = oauth_crypto::encrypt(&master_key, tokens.access_token.as_bytes())
        .map_err(|e| HostOAuthError::Internal(format!("encrypt access: {}", e)))?;
    let new_refresh = match tokens.refresh_token.as_deref() {
        Some(rt) => Some(
            oauth_crypto::encrypt(&master_key, rt.as_bytes())
                .map_err(|e| HostOAuthError::Internal(format!("encrypt refresh: {}", e)))?,
        ),
        None => None,
    };

    let new_expires = tokens.expires_in_secs.map(format_expires_at);
    let new_scopes = tokens.scope.unwrap_or_else(|| account.scopes.clone());

    repository::upsert_user_oauth_account(
        &state.db,
        account.user_id,
        addon_id,
        provider_id,
        &account.external_account_id,
        &account.display_name,
        &new_access,
        new_refresh.as_deref(),
        &tokens.token_type,
        &new_scopes,
        new_expires.as_deref(),
    )
    .map_err(|e| HostOAuthError::Internal(format!("upsert: {}", e)))?;

    let _ = repository::touch_oauth_last_used(&state.db, account.id);

    Ok(OAuthTokenOutPublic {
        access_token: tokens.access_token,
        token_type: tokens.token_type,
        expires_at: new_expires
            .as_deref()
            .and_then(parse_expires_at)
            .map(|e| e as u64),
        scopes: parse_scopes(&new_scopes),
    })
}

/// Public-shaped token (same as serialized output).
#[derive(Debug)]
pub(crate) struct OAuthTokenOutPublic {
    pub access_token: String,
    pub token_type: String,
    pub expires_at: Option<u64>,
    pub scopes: Vec<String>,
}

impl Serialize for OAuthTokenOutPublic {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let wire = OAuthTokenOut {
            access_token: self.access_token.clone(),
            token_type: self.token_type.clone(),
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
        };
        wire.serialize(s)
    }
}

fn parse_scopes(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c == ' ')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Looks up a single user_oauth_account row by (user_id, addon_id, provider_id).
/// `user_id=None` matches rows where user_id IS NULL (global mode).
fn find_account(
    pool: &crate::db::DbPool,
    user_id: Option<i64>,
    addon_id: &str,
    provider_id: &str,
) -> anyhow::Result<Option<repository::DbUserOAuthAccount>> {
    match user_id {
        Some(uid) => Ok(repository::list_user_oauth_accounts_for_user(pool, uid)?
            .into_iter()
            .find(|a| a.addon_id == addon_id && a.provider_id == provider_id)),
        None => Ok(
            repository::list_user_oauth_accounts_for_addon(pool, addon_id)?
                .into_iter()
                .find(|a| a.user_id.is_none() && a.provider_id == provider_id),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::event_bus::EventBus;
    use crate::addon::permissions::PermissionChecker;
    use crate::db::DbPool;
    use std::sync::Arc;

    /// Build a minimal AddonState for resolve_and_refresh tests (no WASM runtime).
    fn mk_state(db: DbPool, addon_id: &str, user_id: Option<i64>) -> AddonState {
        use crate::addon::host_functions::network::NetworkConnectionManager;
        use crate::addon::AddonManifest;
        use parking_lot::Mutex;

        let permissions = vec!["oauth".to_string()];
        let perm_checker = Arc::new(PermissionChecker::new(db.clone()));
        AddonState {
            addon_id: addon_id.to_string(),
            instance_id: "inst-1".to_string(),
            user_id,
            db,
            permissions,
            event_bus: Arc::new(EventBus::new()),
            permission_checker: perm_checker,
            fuel_consumed: 0,
            is_system_call: false,
            rate_limiter: None,
            net_manager: Arc::new(Mutex::new(NetworkConnectionManager::new())),
            settings_cipher: Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32])),
            manifest: Arc::new(AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            router: None,
            oauth_refresh_guard: Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            #[cfg(any(target_os = "ios", target_os = "android"))]
            store_limits: wasmi::StoreLimitsBuilder::new()
                .memory_size(64 * 1024 * 1024)
                .build(),
        }
    }

    fn seed_config_and_decl(db: &DbPool, addon_id: &str, provider_id: &str, mode: &str) {
        repository::upsert_oauth_providers_decl(
            db,
            &repository::DbAddonOAuthProviderDecl {
                addon_id: addon_id.to_string(),
                provider_id: provider_id.to_string(),
                display_name: "X".into(),
                authorize_url: "https://x/authorize".into(),
                token_url: "https://x/token".into(),
                revoke_url: None,
                scopes: "scope1".into(),
                mode: mode.to_string(),
                pkce: true,
            },
        )
        .unwrap();

        // Config with enabled=1, no client_secret.
        repository::upsert_oauth_config(
            db,
            addon_id,
            provider_id,
            "client-id",
            None,
            "https://x/cb",
            true,
            None,
            mode,
        )
        .unwrap();
    }

    fn seed_account(
        db: &DbPool,
        user_id: Option<i64>,
        addon_id: &str,
        provider_id: &str,
        expires_at: Option<&str>,
        with_refresh: bool,
    ) -> i64 {
        let key = oauth_crypto::ensure_master_key(db).unwrap();
        let access = oauth_crypto::encrypt(&key, b"ACCESS-PLAIN").unwrap();
        let refresh = if with_refresh {
            Some(oauth_crypto::encrypt(&key, b"REFRESH-PLAIN").unwrap())
        } else {
            None
        };
        repository::upsert_user_oauth_account(
            db,
            user_id,
            addon_id,
            provider_id,
            "ext-1",
            "display",
            &access,
            refresh.as_deref(),
            "Bearer",
            "scope1",
            expires_at,
        )
        .unwrap()
    }

    fn future_expires() -> String {
        (Utc::now() + chrono::Duration::hours(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    fn past_expires() -> String {
        (Utc::now() - chrono::Duration::hours(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    #[test]
    fn test_oauth_get_token_returns_valid_access_token() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        // Ensure user exists for permission checks via FK.
        let user_id = 42i64;
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash) \
                 VALUES (?1, 'u' || ?1, 'x')",
                rusqlite::params![user_id],
            )
            .unwrap();
        }
        seed_config_and_decl(&db, "addon-a", "microsoft", "individual");
        seed_account(
            &db,
            Some(user_id),
            "addon-a",
            "microsoft",
            Some(&future_expires()),
            true,
        );

        let state = mk_state(db.clone(), "addon-a", Some(user_id));
        let out = resolve_and_refresh(&state, "addon-a", "microsoft", Some(user_id)).unwrap();
        assert_eq!(out.access_token, "ACCESS-PLAIN");
        assert_eq!(out.token_type, "Bearer");
        assert_eq!(out.scopes, vec!["scope1".to_string()]);
        assert!(out.expires_at.is_some());
    }

    #[test]
    fn test_oauth_get_token_returns_error_when_no_refresh_token_and_expired() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let user_id = 7i64;
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash) \
                 VALUES (?1, 'u' || ?1, 'x')",
                rusqlite::params![user_id],
            )
            .unwrap();
        }
        seed_config_and_decl(&db, "addon-b", "google", "individual");
        seed_account(
            &db,
            Some(user_id),
            "addon-b",
            "google",
            Some(&past_expires()),
            false,
        );

        let state = mk_state(db.clone(), "addon-b", Some(user_id));
        let err = resolve_and_refresh(&state, "addon-b", "google", Some(user_id)).unwrap_err();
        assert!(
            matches!(err, HostOAuthError::ExpiredNoRefresh),
            "got {:?}",
            err
        );
    }

    #[test]
    fn test_oauth_get_token_global_mode_uses_null_user() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        seed_config_and_decl(&db, "addon-c", "github", "global");
        // Global account with user_id=NULL.
        seed_account(
            &db,
            None,
            "addon-c",
            "github",
            Some(&future_expires()),
            true,
        );

        // Caller has user_id=99 but mode=global -> must read NULL row.
        let state = mk_state(db.clone(), "addon-c", Some(99));
        let out = resolve_and_refresh(&state, "addon-c", "github", Some(99)).unwrap();
        assert_eq!(out.access_token, "ACCESS-PLAIN");
    }

    #[test]
    fn test_oauth_get_token_individual_requires_user_account() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        seed_config_and_decl(&db, "addon-d", "microsoft", "individual");
        // No account seeded for this user.
        let user_id = 33i64;
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO user_accounts (id, username, password_hash) \
                 VALUES (?1, 'u' || ?1, 'x')",
                rusqlite::params![user_id],
            )
            .unwrap();
        }

        let state = mk_state(db.clone(), "addon-d", Some(user_id));
        let err = resolve_and_refresh(&state, "addon-d", "microsoft", Some(user_id)).unwrap_err();
        assert!(
            matches!(err, HostOAuthError::AccountMissing),
            "got {:?}",
            err
        );
    }

    #[test]
    fn test_oauth_get_token_mode_none_denied() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        seed_config_and_decl(&db, "addon-e", "github", "none");

        let state = mk_state(db.clone(), "addon-e", Some(1));
        let err = resolve_and_refresh(&state, "addon-e", "github", Some(1)).unwrap_err();
        assert!(matches!(err, HostOAuthError::ModeDenied), "got {:?}", err);
    }

    #[test]
    fn test_oauth_get_token_not_configured_when_missing() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let state = mk_state(db.clone(), "addon-f", Some(1));
        let err = resolve_and_refresh(&state, "addon-f", "nowhere", Some(1)).unwrap_err();
        assert!(
            matches!(err, HostOAuthError::NotConfigured),
            "got {:?}",
            err
        );
    }
}
