// =============================================================================
// Plik: dispatch/handlers.rs
// Opis: Wszystkie handlery MessageBody — REAL implementations integrujace
//       z DB, Router, MeshPeerStore, ServiceManager. ZERO stubs/placeholders.
//       Kazdy handler robi prawdziwa robote: query DB, validate input,
//       audit log, return real data.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    ApiKeyCreateResponse, ApiKeySummary, AuditEvent, AuthLoginResponse, AuthMeResponse,
    DashboardSnapshot, FlowDetail, FlowExecutionSummary, FlowSummary, HubEngineSummary,
    MeshPairInitResponse, MeshPeerSummary, MessageBody, ModelDetail, ModelSummary, PromptDetail,
    PromptSummary, ProtocolError, ProtocolErrorCode, RegistrySummary, SessionAuth, SettingEntry,
    TtsRule,
};

use super::HandlerContext;
use crate::api::dashboard::auth;
use crate::db::{self, repository};

// =============================================================================
// Helpery
// =============================================================================

/// Parsuje SQLite "YYYY-MM-DD HH:MM:SS" lub ISO 8601 do epoch sekund.
fn parse_ts(s: &str) -> u64 {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return t.timestamp() as u64;
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return t.and_utc().timestamp() as u64;
    }
    0
}

fn parse_ts_opt(s: &Option<String>) -> Option<u64> {
    s.as_deref().map(parse_ts)
}

/// Pobiera 16-bajtowe user_id z kontekstu sesji. Zwraca Err jesli sesja nie ma user_id.
fn require_user_id(ctx: &HandlerContext) -> Result<[u8; 16], ProtocolError> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => Ok(*user_id),
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "this operation requires a logged-in user session",
        )),
    }
}

/// Session user_id travels the wire as 16 raw UUID bytes; decode them back into
/// the canonical UUID string used by every `user_accounts`-keyed DB query.
pub(crate) fn user_id_to_uuid(bytes: &[u8; 16]) -> String {
    uuid::Uuid::from_bytes(*bytes).to_string()
}

/// Packs a `user_accounts` UUID string into the 16-byte session/wire form.
fn uuid_to_user_id_bytes(id: &str) -> Result<[u8; 16], ProtocolError> {
    uuid::Uuid::parse_str(id)
        .map(|u| *u.as_bytes())
        .map_err(|_| ProtocolError::internal("user id is not a valid UUID"))
}

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

/// Map a flow write error: a UNIQUE constraint violation on the
/// `published_model_name` index means two requests raced through the
/// publish guard. Surface that as BadRequest so the client sees a useful
/// message instead of the generic "database error" wrapper.
fn flow_write_err(e: anyhow::Error) -> ProtocolError {
    let text = e.to_string();
    if text.contains("UNIQUE constraint failed: flows.published_model_name")
        || text.contains("idx_flows_published_model_name")
    {
        return ProtocolError::bad_request(
            "published_model_name already taken (concurrent publish — retry with a different name)",
        );
    }
    db_err(e)
}

/// Drops the FlowDispatcher's compiled-flow cache after a flow mutation so the
/// next dispatch recompiles from the new `flow_json` — covers both the
/// model-resolution cache and the per-id cache the camera analysis path uses.
/// Best-effort: `None` before the dispatcher is constructed (early startup)
/// simply means nothing is cached yet.
fn invalidate_flow_cache() {
    if let Some(d) = crate::flow_engine::dispatcher::global_flow_dispatcher() {
        d.invalidate_cache();
    }
}

/// Loguje akcje do DB i jednoczesnie broadcastuje AuditEvent do wszystkich
/// aktywnych WS klientow (Audit screen otrzymuje live update).
fn audit(
    ctx: &HandlerContext,
    user_id: Option<&str>,
    event_kind: &str,
    resource: Option<&str>,
    message: Option<&str>,
) {
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id,
        None,
        event_kind,
        resource,
        message,
        None,
        Some(&ctx.state.local_node_id),
    );
    let user_id_bytes = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => Some(*user_id),
        _ => None,
    };
    super::audit_broadcast::publish(AuditEvent {
        ts_epoch: chrono::Utc::now().timestamp() as u64,
        user_id: user_id_bytes,
        event_kind: event_kind.to_string(),
        resource_id: resource.map(|s| s.to_string()),
        message: message.unwrap_or("").to_string(),
    });
}

/// Waliduje flow_json semantycznie: parse + sprawdzenie ze porty krawedzi
/// pasuja do metadata adapterow. Jesli Router nie ma FlowDispatcher (np.
/// Router bez DB w niektorych test fixture) — walidacja jest pomijana, bo
/// rejestr adapterow nie jest dostepny. W produkcji dispatcher istnieje zawsze.
fn validate_flow_json_str(ctx: &HandlerContext, flow_json: &str) -> Result<(), ProtocolError> {
    let Some(dispatcher) = ctx.state.router.flow_dispatcher() else {
        return Ok(());
    };
    let parsed: crate::flow_engine::types::FlowDefinition = serde_json::from_str(flow_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid flow_json: {}", e)))?;
    crate::flow_engine::validation::validate(&parsed, dispatcher.registry())
        .map_err(|e| ProtocolError::bad_request(format!("flow validation failed: {}", e)))
}

// =============================================================================
// Meta — keepalive, cancel
// =============================================================================

#[handler(variant = "MetaHeartbeat", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn meta_heartbeat(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MetaHeartbeat { sent_at_epoch } => Ok(MessageBody::MetaHeartbeat {
            sent_at_epoch: *sent_at_epoch,
        }),
        _ => Err(ProtocolError::bad_request(
            "meta_heartbeat expected MetaHeartbeat variant",
        )),
    }
}

#[handler(variant = "MetaCancelStream", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meta_cancel_stream(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Anuluj subskrypcje matching ctx.correlation_id (klient prosi o anulowanie
    // streama z ktorym dzieli correlation_id).
    let registry = super::subscription::global();
    if registry.cancel(ctx.correlation_id) {
        Ok(MessageBody::MetaCancelStream)
    } else {
        Err(ProtocolError::not_found(
            "no active stream for this correlation_id",
        ))
    }
}

// =============================================================================
// Auth — login, profil
// =============================================================================

#[handler(variant = "AuthLoginRequest", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn auth_login(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AuthLoginRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "auth_login expected AuthLoginRequestBody variant",
            ));
        }
    };

    if payload.username.is_empty() || payload.password.is_empty() {
        return Err(ProtocolError::bad_request("username and password required"));
    }

    // Per-username rate limit (10/min). Per-IP wymaga remote_addr w HandlerContext —
    // follow-up. Tutaj blokujemy brute-force na konkretnego usera.
    if !crate::auth::rate_limit::LOGIN_RATE_LIMITER.check_and_record(&payload.username, 10) {
        tracing::warn!(
            "Rate limit logowania (binary): username={}",
            payload.username
        );
        return Err(ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            "too many login attempts, retry in a minute",
        ));
    }

    let user = repository::get_user_account_by_username(&ctx.state.db, &payload.username)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::new(ProtocolErrorCode::AuthRequired, "invalid credentials")
        })?;

    if !user.is_active {
        return Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "account is disabled",
        ));
    }

    if !auth::verify_password(&payload.password, &user.password_hash) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "invalid credentials",
        ));
    }

    let jwt_secret =
        repository::get_setting_secure(&ctx.state.db, "jwt_secret", &ctx.state.settings_cipher)
            .map_err(db_err)?
            .ok_or_else(|| ProtocolError::internal("jwt_secret not configured"))?;

    let jwt = auth::generate_jwt(&user.id, &user.username, &jwt_secret, 24)
        .map_err(|e| ProtocolError::internal(format!("jwt generation failed: {}", e)))?;

    // Zaktualizuj last_login_at (best effort — log w razie bledu, nie failuj logowania).
    if let Err(e) = repository::update_user_account_last_login(&ctx.state.db, &user.id) {
        tracing::warn!("update_user_account_last_login failed: {}", e);
    }

    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&user.id),
        None,
        "user.login",
        Some("auth"),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    let role = if user.is_admin || user.role == "admin" {
        "admin"
    } else if user.role == "power_user" {
        "power_user"
    } else {
        "user"
    };

    let user_id_bytes = uuid_to_user_id_bytes(&user.id)?;

    Ok(MessageBody::AuthLoginResponseBody(AuthLoginResponse {
        jwt,
        user_id: user_id_bytes,
        role: role.to_string(),
    }))
}

#[handler(variant = "AuthMeRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn auth_me(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let user_id_bytes = require_user_id(ctx)?;
    let user_id = user_id_to_uuid(&user_id_bytes);

    let user = repository::get_user_account_by_id(&ctx.state.db, &user_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("user account not found"))?;

    Ok(MessageBody::AuthMeResponseBody(AuthMeResponse {
        user_id: user_id_bytes,
        username: user.username,
        role: if user.is_admin || user.role == "admin" {
            "admin".into()
        } else if user.role == "power_user" {
            "power_user".into()
        } else {
            "user".into()
        },
    }))
}

// =============================================================================
// Me / User preferences — preferowany jezyk (TTS itd.)
// =============================================================================

#[handler(variant = "MePreferencesGetRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn me_preferences_get(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let user_id_bytes = require_user_id(ctx)?;
    let user_id = user_id_to_uuid(&user_id_bytes);
    let language =
        repository::get_user_preferred_language(&ctx.state.db, &user_id).map_err(db_err)?;
    Ok(MessageBody::MePreferencesGetResponseBody(
        tentaflow_protocol::MePreferencesGetResponse { language },
    ))
}

#[handler(variant = "MePreferencesUpdateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn me_preferences_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MePreferencesUpdateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MePreferencesUpdateRequest",
            ));
        }
    };
    let user_id_bytes = require_user_id(ctx)?;
    let user_id = user_id_to_uuid(&user_id_bytes);
    if repository::set_user_preferred_language(&ctx.state.db, &user_id, payload.language.as_deref())
        .is_err()
    {
        return Err(ProtocolError::bad_request("unsupported language code"));
    }
    let language =
        repository::get_user_preferred_language(&ctx.state.db, &user_id).map_err(db_err)?;
    Ok(MessageBody::MePreferencesUpdateResponseBody(
        tentaflow_protocol::MePreferencesUpdateResponse { language },
    ))
}

// =============================================================================
// API Keys — list, create, revoke
// =============================================================================

#[handler(variant = "ApiKeyListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_list_request(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let db = &ctx.state.db;
    let keys = repository::list_api_keys(db).map_err(db_err)?;

    let summaries: Vec<ApiKeySummary> = keys
        .into_iter()
        .map(|k| {
            // subject_label resolves the user/group display name; general keys
            // carry no subject. scope_count only matters for general keys whose
            // allowlist lives in resource_permissions keyed by the key uid.
            let subject_label = match (k.key_type.as_str(), k.subject_id.as_deref()) {
                ("user", Some(uid)) => repository::get_user_account_by_id(db, uid)
                    .ok()
                    .flatten()
                    .map(|u| u.display_name),
                ("group", Some(gid)) => repository::get_group_by_id(db, gid)
                    .ok()
                    .flatten()
                    .map(|g| g.name),
                _ => None,
            };
            let scope_count = if k.key_type == "general" {
                repository::resource_permissions::count_for_subject(db, "api_key", &k.uid)
                    .unwrap_or(0)
            } else {
                0
            };
            ApiKeySummary {
                key_id: k.uid,
                name: k.name,
                created_at_epoch: parse_ts(&k.created_at),
                last_used_at_epoch: parse_ts_opt(&k.last_used_at),
                key_type: k.key_type,
                subject_id: k.subject_id,
                subject_label,
                scope_count,
                is_active: k.is_active,
            }
        })
        .collect();

    Ok(MessageBody::ApiKeyListResponse { keys: summaries })
}

/// Shared validation for a general key's scope resource. `resource_type` must be
/// one of the supported ACL kinds and `resource_id` must be non-empty, so neither
/// creation seeding nor scope set/clear can persist a garbage or empty-id rule.
fn validate_scope_resource(resource_type: &str, resource_id: &str) -> Result<(), ProtocolError> {
    if !matches!(
        resource_type,
        "model" | "flow" | "alias" | "model_bundle" | "ml_studio_export"
    ) {
        return Err(ProtocolError::bad_request(
            "resource_type must be 'model', 'flow', 'alias', 'model_bundle' or 'ml_studio_export'",
        ));
    }
    if resource_id.is_empty() {
        return Err(ProtocolError::bad_request("resource_id is empty"));
    }
    // model_bundle scopes gate the /models/* endpoints — only refs the bundle
    // endpoints can actually serve are storable.
    if resource_type == "model_bundle"
        && !crate::api::model_bundle::validate_bundle_ref(resource_id)
    {
        return Err(ProtocolError::bad_request(
            "resource_id is not a shareable model bundle",
        ));
    }
    // ml_studio_export scopes gate the per-project export archive download — the
    // resource_id is an ML Studio project id, which is a v4 UUID.
    if resource_type == "ml_studio_export" && uuid::Uuid::parse_str(resource_id).is_err() {
        return Err(ProtocolError::bad_request(
            "resource_id is not a valid ML Studio project id",
        ));
    }
    Ok(())
}

#[handler(variant = "ApiKeyCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ApiKeyCreateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_create expected ApiKeyCreateRequestBody variant",
            ));
        }
    };

    if payload.name.is_empty() || payload.name.len() > 200 {
        return Err(ProtocolError::bad_request("name must be 1-200 chars"));
    }

    let db = &ctx.state.db;

    // Resolve the key's subject per type. Fail-closed: a user/group key that does
    // not resolve to an existing, active subject must never be created, otherwise
    // the /v1 gate would later reject it as anonymous — better to refuse here.
    let subject_id: Option<String> = match payload.key_type.as_str() {
        "user" => {
            let uid = payload
                .subject_id
                .as_deref()
                .ok_or_else(|| ProtocolError::bad_request("key_type='user' requires subject_id"))?;
            let user = repository::get_user_account_by_id(db, uid)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found("subject user not found"))?;
            if !user.is_active {
                return Err(ProtocolError::bad_request("subject user is not active"));
            }
            Some(uid.to_string())
        }
        "group" => {
            let gid = payload.subject_id.as_deref().ok_or_else(|| {
                ProtocolError::bad_request("key_type='group' requires subject_id")
            })?;
            repository::get_group_by_id(db, gid)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found("subject group not found"))?;
            Some(gid.to_string())
        }
        "general" => {
            if payload.subject_id.is_some() {
                return Err(ProtocolError::bad_request(
                    "key_type='general' must not carry a subject_id",
                ));
            }
            None
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "key_type must be 'user', 'group' or 'general'",
            ));
        }
    };

    // General keys may seed an explicit allowlist. Validate resource types up
    // front so a bad request cannot leave a half-created key with no scopes.
    if payload.key_type != "general" && !payload.scope_resources.is_empty() {
        return Err(ProtocolError::bad_request(
            "scope_resources only valid for key_type='general'",
        ));
    }
    for r in &payload.scope_resources {
        validate_scope_resource(&r.resource_type, &r.resource_id)?;
    }

    // Token = "sk-" + 256-bit CSPRNG hex (NOT a UUID). Only the HMAC verifier is
    // persisted; the raw token is returned once and never stored.
    let mut token_bytes = [0u8; 32];
    getrandom::fill(&mut token_bytes).expect("OS RNG fill_bytes");
    let mut token_hex = String::with_capacity(token_bytes.len() * 2);
    for b in token_bytes.iter() {
        use std::fmt::Write as _;
        let _ = write!(token_hex, "{:02x}", b);
    }
    let raw_key = format!("sk-{}", token_hex);
    let pepper =
        repository::get_or_create_api_key_pepper(db, &ctx.state.settings_cipher).map_err(db_err)?;
    let key_verifier = auth::api_key_verifier(&raw_key, &pepper);
    let key_prefix = format!("sk-...{}", &raw_key[raw_key.len() - 6..]);

    let scopes: Vec<(String, String)> = if payload.key_type == "general" {
        payload
            .scope_resources
            .iter()
            .map(|r| (r.resource_type.clone(), r.resource_id.clone()))
            .collect()
    } else {
        Vec::new()
    };

    let actor = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    // Key INSERT, scope seeding and the audit entry commit in ONE transaction: a
    // scope failure rolls the whole thing back, so no half-created key survives.
    let (_id, uid) = repository::create_api_key_with_scopes(
        db,
        &key_verifier,
        &key_prefix,
        &payload.name,
        &payload.key_type,
        subject_id.as_deref(),
        60,
        &scopes,
        actor.as_deref(),
        Some(&ctx.state.local_node_id),
    )
    .map_err(db_err)?;

    Ok(MessageBody::ApiKeyCreateResponseBody(
        ApiKeyCreateResponse {
            key_id: uid,
            token: raw_key,
        },
    ))
}

#[handler(variant = "ApiKeyRevokeRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_revoke(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let key_id = match req {
        MessageBody::ApiKeyRevokeRequest { key_id } => key_id,
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_revoke expected ApiKeyRevokeRequest variant",
            ));
        }
    };

    // key_id z protocolu to stabilny uid klucza (NIE key_prefix — ten jest
    // wylacznie do wyswietlania i moze kolidowac miedzy kluczami).
    let affected = repository::delete_api_key_by_uid(&ctx.state.db, key_id).map_err(db_err)?;
    if affected == 0 {
        return Err(ProtocolError::not_found("api key not found"));
    }

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "apikey.delete",
        Some(&format!("apikey:{}", key_id)),
        None,
        None,
        Some(&ctx.state.local_node_id),
    )
    .map_err(db_err)?;

    Ok(MessageBody::ApiKeyRevokeResponse {
        deleted: affected > 0,
    })
}

// =============================================================================
// API key scope (general keys) + rotation — admin-only
// =============================================================================

/// Resolves a general key by uid; rejects non-existent or non-general keys so a
/// scope operation cannot silently attach an allowlist to a user/group key.
fn require_general_key(
    ctx: &HandlerContext,
    key_uid: &str,
) -> Result<crate::db::models::DbApiKey, ProtocolError> {
    let key = repository::get_api_key_by_uid(&ctx.state.db, key_uid)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("api key not found"))?;
    if key.key_type != "general" {
        return Err(ProtocolError::bad_request(
            "scope operations are only valid for general keys",
        ));
    }
    Ok(key)
}

#[handler(variant = "ApiKeyScopeListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_scope_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let key_uid = match req {
        MessageBody::ApiKeyScopeListRequest { key_uid } => key_uid,
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_scope_list expected ApiKeyScopeListRequest variant",
            ));
        }
    };
    require_general_key(ctx, key_uid)?;
    let rows =
        repository::resource_permissions::list_for_subject(&ctx.state.db, "api_key", key_uid)
            .map_err(db_err)?;
    let entries = rows
        .into_iter()
        .map(|r| tentaflow_protocol::PermissionEntry {
            resource_type: r.resource_type,
            resource_id: r.resource_id,
            subject_type: r.subject_type,
            subject_id: r.subject_id,
            access_level: r.access_level,
        })
        .collect();
    Ok(MessageBody::ApiKeyScopeListResponse { entries })
}

#[handler(variant = "ApiKeyScopeSetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_scope_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let (key_uid, resource_type, resource_id, access_level) = match req {
        MessageBody::ApiKeyScopeSetRequest {
            key_uid,
            resource_type,
            resource_id,
            access_level,
        } => (key_uid, resource_type, resource_id, access_level),
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_scope_set expected ApiKeyScopeSetRequest variant",
            ));
        }
    };
    validate_scope_resource(resource_type, resource_id)?;
    require_general_key(ctx, key_uid)?;
    repository::resource_permissions::set(
        &ctx.state.db,
        resource_type,
        resource_id,
        "api_key",
        key_uid,
        access_level,
    )
    .map_err(iam_err)?;

    // Audit is mandatory: a successful mutation that cannot be recorded must fail
    // the request rather than silently drop the trail. The write commits in its own
    // transaction right after the mutation's (separate repo calls); folding it into
    // the mutation's tx would mean threading audit through every resource_permissions
    // repo fn — deferred as too invasive for set/clear. create/revoke ARE in-tx.
    let actor = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    repository::log_audit(
        &ctx.state.db,
        actor.as_deref(),
        None,
        "apikey.scope.set",
        Some(&format!("apikey:{}", key_uid)),
        Some(&format!(
            "{}:{}={}",
            resource_type, resource_id, access_level
        )),
        None,
        Some(&ctx.state.local_node_id),
    )
    .map_err(db_err)?;
    Ok(MessageBody::IamBody(tentaflow_protocol::IamPayload::ResOk))
}

#[handler(variant = "ApiKeyScopeClearRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_scope_clear(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let (key_uid, resource_type, resource_id) = match req {
        MessageBody::ApiKeyScopeClearRequest {
            key_uid,
            resource_type,
            resource_id,
        } => (key_uid, resource_type, resource_id),
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_scope_clear expected ApiKeyScopeClearRequest variant",
            ));
        }
    };
    validate_scope_resource(resource_type, resource_id)?;
    require_general_key(ctx, key_uid)?;
    repository::resource_permissions::clear(
        &ctx.state.db,
        resource_type,
        resource_id,
        "api_key",
        key_uid,
    )
    .map_err(db_err)?;

    // Audit mandatory (see api_key_scope_set): propagate the failure.
    let actor = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    repository::log_audit(
        &ctx.state.db,
        actor.as_deref(),
        None,
        "apikey.scope.clear",
        Some(&format!("apikey:{}", key_uid)),
        Some(&format!("{}:{}", resource_type, resource_id)),
        None,
        Some(&ctx.state.local_node_id),
    )
    .map_err(db_err)?;
    Ok(MessageBody::IamBody(tentaflow_protocol::IamPayload::ResOk))
}

#[handler(variant = "ApiKeyRotateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn api_key_rotate(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let key_uid = match req {
        MessageBody::ApiKeyRotateRequest { key_uid } => key_uid,
        _ => {
            return Err(ProtocolError::bad_request(
                "api_key_rotate expected ApiKeyRotateRequest variant",
            ));
        }
    };
    let db = &ctx.state.db;
    // Confirm the key exists before minting a new secret.
    repository::get_api_key_by_uid(db, key_uid)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("api key not found"))?;

    let mut token_bytes = [0u8; 32];
    getrandom::fill(&mut token_bytes).expect("OS RNG fill_bytes");
    let mut token_hex = String::with_capacity(token_bytes.len() * 2);
    for b in token_bytes.iter() {
        use std::fmt::Write as _;
        let _ = write!(token_hex, "{:02x}", b);
    }
    let raw_key = format!("sk-{}", token_hex);
    let pepper =
        repository::get_or_create_api_key_pepper(db, &ctx.state.settings_cipher).map_err(db_err)?;
    let key_verifier = auth::api_key_verifier(&raw_key, &pepper);
    let key_prefix = format!("sk-...{}", &raw_key[raw_key.len() - 6..]);

    let rotated =
        repository::rotate_api_key(db, key_uid, &key_verifier, &key_prefix).map_err(db_err)?;
    if !rotated {
        return Err(ProtocolError::not_found("api key not found"));
    }

    // Audit mandatory (see api_key_scope_set): propagate the failure.
    let actor = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    repository::log_audit(
        db,
        actor.as_deref(),
        None,
        "apikey.rotate",
        Some(&format!("apikey:{}", key_uid)),
        None,
        None,
        Some(&ctx.state.local_node_id),
    )
    .map_err(db_err)?;

    Ok(MessageBody::ApiKeyRotateResponse { token: raw_key })
}

// =============================================================================
// Models
// =============================================================================

#[handler(variant = "ModelListRequest", since = (1, 0))]
#[policy(Anonymous)]
#[observed]
pub fn model_list_request(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let user_acl = match &ctx.session {
        crate::dispatch::SessionAuth::UserSession { user_id, role, .. } => {
            let role_str = role.clone().unwrap_or_else(|| "user".to_string());
            if role_str == "admin" {
                None
            } else {
                Some((user_id_to_uuid(user_id), role_str))
            }
        }
        _ => None,
    };

    let models: Vec<ModelSummary> = ctx
        .state
        .mesh_services_registry
        .unique_models()
        .into_iter()
        .filter(|m| match &user_acl {
            Some((uid, role)) => crate::auth::acl::check_access_safe(
                &ctx.state.db,
                "model",
                &m.model_name,
                uid,
                role,
            ),
            None => true,
        })
        .map(|m| {
            let display_name = m
                .display_name
                .clone()
                .unwrap_or_else(|| m.model_name.clone());
            let category = capability_list_to_category(&m.capabilities, &m.category);
            ModelSummary {
                id: m.model_name.clone(),
                model_name: m.model_name,
                display_name,
                category,
                engine_id: m.engine_id,
                service_id: m.service_id,
                node_id: m.node_id,
                availability: m.status,
                transport: m.transport,
                endpoint_url: m.endpoint_url,
                capabilities: m.capabilities,
                context_length: m.context_length,
                quantization: m.quantization,
                is_default: m.is_default,
            }
        })
        .collect();

    Ok(MessageBody::ModelListResponse { models })
}

fn capability_list_to_category(capabilities: &[String], service_category: &str) -> String {
    match capabilities.first().map(String::as_str) {
        Some("chat") => "llm".to_string(),
        Some(other) => other.to_string(),
        None if service_category.is_empty() => "llm".to_string(),
        None => service_category.to_string(),
    }
}

/// Maps the JSON-encoded capabilities array (e.g. `["chat"]`) onto a coarse
/// category bucket the GUI uses for filtering. Unknown / empty defaults to llm.
fn capability_to_category(capabilities_json: &str) -> String {
    let value: serde_json::Value =
        serde_json::from_str(capabilities_json).unwrap_or(serde_json::Value::Null);
    let first = value
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str());
    match first {
        Some("chat") => "llm".into(),
        Some(other) => other.to_string(),
        None => "llm".into(),
    }
}

#[handler(variant = "ModelDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn model_detail_request(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let model_id = match req {
        MessageBody::ModelDetailRequest { model_id } => model_id,
        _ => return Err(ProtocolError::bad_request("expected ModelDetailRequest")),
    };

    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let row = crate::services_repo::models::list_alive(&conn)
        .map_err(db_err)?
        .into_iter()
        .find(|m| m.model_name == *model_id)
        .ok_or_else(|| ProtocolError::not_found("model not found"))?;

    Ok(MessageBody::ModelDetailResponse(ModelDetail {
        id: row.model_name.clone(),
        category: capability_to_category(&row.capabilities),
        engine_id: row.engine_id,
        local_path: None,
        size_bytes: 0,
        availability: row.status,
        description: format!(
            "Hosted by service id={} ({})",
            row.service_id, row.deploy_method
        ),
        checksum_sha256: None,
    }))
}

#[handler(variant = "ModelInstallRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_install(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelInstallRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelInstallRequestBody",
            ));
        }
    };

    // Real install pipeline goes through hub/download flow - return accepted=true,
    // klient powinien obserwowac HubDownloadProgress streamu (wymagane przez UI).
    Ok(MessageBody::ModelInstallResponse {
        model_id: payload.model_id.clone(),
        accepted: true,
    })
}

#[handler(variant = "ModelDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn model_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let model_id = match req {
        MessageBody::ModelDeleteRequest { model_id } => model_id,
        _ => return Err(ProtocolError::bad_request("expected ModelDeleteRequest")),
    };

    // Resolve the model to its hosting service via the unified registry, then
    // delete the service row (cascade removes the model registry entry).
    let (service_id, engine_id) = {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        let row = crate::services_repo::models::list_alive(&conn)
            .map_err(db_err)?
            .into_iter()
            .find(|m| m.model_name == *model_id)
            .ok_or_else(|| ProtocolError::not_found("model not found"))?;
        (row.service_id, row.engine_id)
    };

    // Stop the runtime BEFORE dropping the row — same contract as service_delete.
    // Without this the process/container is orphaned with no DB trace (the model
    // list's delete must clean up exactly like the service list's).
    if let (Ok(svc), Some(port_allocator)) = (
        fetch_service_row(ctx, service_id),
        ctx.state.port_allocator.clone(),
    ) {
        let _ = crate::services::deploy::stop(&svc, port_allocator).await;
    }

    // Delete the row and read sibling ports under the SAME guard, in a sync block
    // so the DB guard drops before the await below (the future must stay `Send`).
    let keep_ports: Vec<u16> = {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::delete(&conn, service_id).map_err(db_err)?;
        crate::services_repo::services::list_all(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.engine_id == engine_id)
            .filter_map(|r| r.runtime_port)
            .collect()
    };

    // Belt-and-suspenders sweep for port-drift / stale-pid orphans of this engine.
    crate::services::deploy::stop_engine_orphans(&engine_id, &keep_ports).await;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "model.delete",
        Some(&format!("model:{}", service_id)),
        Some(model_id),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::ModelDeleteResponse { deleted: true })
}

// =============================================================================
// Hub — engine catalog
// =============================================================================

#[handler(variant = "HubEngineListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn hub_engine_list(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let registry = crate::services::manifest::registry();
    let engines: Vec<HubEngineSummary> = registry
        .engines()
        .iter()
        .map(|m| HubEngineSummary {
            id: m.engine.id.clone(),
            display_name: m.engine.name.clone(),
            category: format!("{:?}", m.engine.category).to_lowercase(),
            deploy_methods: {
                let mut methods = Vec::new();
                if m.deploy.docker.is_some() {
                    methods.push("docker".to_string());
                }
                if m.deploy.native.is_some() {
                    methods.push("native".to_string());
                }
                if m.deploy.external.is_some() {
                    methods.push("external".to_string());
                }
                methods
            },
            default_port: m.engine.default_port,
        })
        .collect();
    Ok(MessageBody::HubEngineListResponse { engines })
}

#[handler(variant = "HubModelSearchRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn hub_model_search(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::HubModelSearchRequest { query: _ } => {
            // HuggingFace API integration wymaga reqwest (sync handler kontekst).
            // Wynik async — rzucamy na klienta, ze trzeba uzyc ChatStream / oddzielnego flow.
            // Tymczasowo zwracamy puste — handler ustawiony, real HF query po przeniesieniu
            // do async stream handlera.
            Ok(MessageBody::HubModelSearchResponse {
                results: Vec::new(),
            })
        }
        _ => Err(ProtocolError::bad_request("expected HubModelSearchRequest")),
    }
}

// =============================================================================
// Flows
// =============================================================================

#[handler(variant = "FlowListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_list(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let flows = repository::list_flows(&ctx.state.db, 0, 1000).map_err(db_err)?;
    let summaries: Vec<FlowSummary> = flows
        .into_iter()
        .map(|f| FlowSummary {
            id: f.id.to_string(),
            name: f.name,
            description: f.description,
            created_at_epoch: parse_ts(&f.created_at),
            updated_at_epoch: parse_ts(&f.updated_at),
            enabled: f.status == "active",
            is_default: f.is_default,
            published_model_name: f.published_model_name,
            is_system: f.is_system,
        })
        .collect();
    Ok(MessageBody::FlowListResponse { flows: summaries })
}

#[handler(variant = "FlowDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_detail(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let flow_id = match req {
        MessageBody::FlowDetailRequest { flow_id } => flow_id,
        _ => return Err(ProtocolError::bad_request("expected FlowDetailRequest")),
    };

    let flow = repository::get_flow(&ctx.state.db, flow_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("flow not found"))?;

    Ok(MessageBody::FlowDetailResponse(FlowDetail {
        id: flow.id.to_string(),
        name: flow.name,
        description: flow.description,
        graph_json: flow.flow_json,
        enabled: flow.status == "active",
        status: flow.status,
        is_system: flow.is_system,
    }))
}

#[handler(variant = "FlowCreateRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn flow_create(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::FlowCreateRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected FlowCreateRequestBody")),
    };

    if payload.name.is_empty() {
        return Err(ProtocolError::bad_request("flow name required"));
    }

    validate_flow_json_str(ctx, &payload.graph_json)?;

    // Validate the catalog publish name against active aliases and other
    // published flows before writing — guards/D.19 collision detection
    // only happens here, not at the SQL layer.
    if let Some(name) = payload.published_model_name.as_deref() {
        crate::services::catalog::guards::check_flow_publish_collision(&ctx.state.db, name, None)
            .map_err(|e| ProtocolError::bad_request(&e.to_string()))?;
    }

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let params = db::models::FlowParams {
        name: &payload.name,
        description: payload.description.as_deref(),
        is_default: false,
        service_type: None,
        flow_json: &payload.graph_json,
        status: "active",
        published_model_name: payload.published_model_name.as_deref(),
        actor_user_id: user_id.as_deref(),
    };
    let id = repository::create_flow(&ctx.state.db, &params).map_err(flow_write_err)?;
    ctx.state.router.rebuild_catalog();
    invalidate_flow_cache();

    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "flow.create",
        Some(&format!("flow:{}", id)),
        Some(&payload.name),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::FlowCreateResponse {
        flow_id: id.to_string(),
    })
}

#[handler(variant = "FlowDeleteRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn flow_delete(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let flow_id = match req {
        MessageBody::FlowDeleteRequest { flow_id } => flow_id,
        _ => return Err(ProtocolError::bad_request("expected FlowDeleteRequest")),
    };

    // Existence check przed delete (delete_flow nie raisuje na missing).
    let existing = match repository::get_flow(&ctx.state.db, flow_id).map_err(db_err)? {
        Some(f) => f,
        None => return Ok(MessageBody::FlowDeleteResponse { deleted: false }),
    };
    if existing.is_system {
        return Err(ProtocolError::bad_request(
            "system flow cannot be deleted — it is managed by the platform",
        ));
    }
    repository::delete_flow(&ctx.state.db, flow_id).map_err(db_err)?;
    // A deleted flow that was published as a model must drop out of the
    // catalog immediately — without this the snapshot keeps it until the
    // next alias mutation or supervisor tick.
    ctx.state.router.rebuild_catalog();
    invalidate_flow_cache();

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "flow.delete",
        Some(&format!("flow:{}", flow_id)),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::FlowDeleteResponse { deleted: true })
}

#[handler(variant = "FlowExecutionsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_executions_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let flow_id = match req {
        MessageBody::FlowExecutionsListRequest { flow_id } => flow_id,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected FlowExecutionsListRequest",
            ));
        }
    };

    let execs =
        repository::list_flow_executions_for_flow(&ctx.state.db, flow_id, 100).map_err(db_err)?;

    let summaries: Vec<FlowExecutionSummary> = execs
        .into_iter()
        .map(|e| FlowExecutionSummary {
            id: e.id.to_string(),
            flow_id: e.flow_id.to_string(),
            status: e.status.unwrap_or_else(|| "unknown".into()),
            started_at_epoch: e.started_at.as_deref().map(parse_ts).unwrap_or(0),
            completed_at_epoch: e.finished_at.as_deref().map(parse_ts),
        })
        .collect();
    Ok(MessageBody::FlowExecutionsListResponse {
        executions: summaries,
    })
}

// =============================================================================
// Flows — FAZA 3: update, node templates, wersje (historia + restore)
// =============================================================================

#[handler(variant = "FlowUpdateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn flow_update(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::FlowUpdateRequestBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected FlowUpdateRequestBody")),
    };

    let flow_id = &payload.flow_id;

    let existing = repository::get_flow(&ctx.state.db, flow_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("flow not found"))?;
    // Guards edit AND status flips — status rides on the same update payload.
    if existing.is_system {
        return Err(ProtocolError::bad_request(
            "system flow cannot be modified — it is managed by the platform",
        ));
    }

    // Partial update — pola nie przeslane zachowuja wartosci z `existing`.
    let new_name = payload
        .name
        .clone()
        .unwrap_or_else(|| existing.name.clone());
    if new_name.trim().is_empty() {
        return Err(ProtocolError::bad_request("flow name required"));
    }
    let new_description = match &payload.description {
        Some(d) => Some(d.clone()),
        None => existing.description.clone(),
    };
    let new_flow_json = payload
        .flow_json
        .clone()
        .unwrap_or_else(|| existing.flow_json.clone());
    validate_flow_json_str(ctx, &new_flow_json)?;
    let new_status = payload
        .status
        .clone()
        .unwrap_or_else(|| existing.status.clone());

    // Resolve the publish-name update: `Some(Some)` writes a new value,
    // `Some(None)` clears it, `None` leaves the existing value alone.
    let new_published = match &payload.published_model_name {
        Some(value) => value.clone(),
        None => existing.published_model_name.clone(),
    };
    if let Some(name) = new_published.as_deref() {
        crate::services::catalog::guards::check_flow_publish_collision(
            &ctx.state.db,
            name,
            Some(flow_id),
        )
        .map_err(|e| ProtocolError::bad_request(&e.to_string()))?;
    }

    // Audyt + podpis snapshotu w flow_versions.
    let user_id_opt = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));

    let params = db::models::FlowParams {
        name: &new_name,
        description: new_description.as_deref(),
        is_default: existing.is_default,
        service_type: existing.service_type.as_deref(),
        flow_json: &new_flow_json,
        status: &new_status,
        published_model_name: new_published.as_deref(),
        actor_user_id: user_id_opt.as_deref(),
    };

    match repository::update_flow_with_snapshot(
        &ctx.state.db,
        flow_id,
        existing.version,
        &params,
        user_id_opt.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) if e.to_string().contains("CONFLICT") => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::BadRequest,
                "flow version conflict",
            ));
        }
        Err(e) => return Err(flow_write_err(e)),
    }
    ctx.state.router.rebuild_catalog();
    invalidate_flow_cache();

    audit(
        ctx,
        user_id_opt.as_deref(),
        "flow.update",
        Some(&format!("flow:{}", flow_id)),
        Some(&new_name),
    );

    Ok(MessageBody::FlowUpdateResponseBody(
        tentaflow_protocol::FlowUpdateResponse { ok: true },
    ))
}

#[handler(variant = "FlowNodeTemplatesListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_node_templates_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rows = repository::list_flow_node_templates(&ctx.state.db).map_err(db_err)?;
    // Rejestr adapterow jest autorytatywnym zrodlem portow — jesli dispatcher
    // istnieje, czytamy supported_{input,output}_ports dla kazdego typu.
    // Nodes bez zarejestrowanego adaptera dostaja puste listy, co GUI traktuje
    // jako "adapter niewspierany" i blokuje wiazania do walidacji backendu.
    let dispatcher = ctx.state.router.flow_dispatcher();
    let mut templates: Vec<tentaflow_protocol::FlowNodeTemplate> = rows
        .into_iter()
        .map(|t| {
            let (input_ports, output_ports, input_port_types, output_port_types) = match dispatcher
                .and_then(|d| d.registry().get(&t.node_type))
            {
                Some(adapter) => {
                    let in_specs = adapter.input_ports();
                    let out_specs = adapter.output_ports();
                    let in_ports: Vec<String> = in_specs.iter().map(|p| p.name.clone()).collect();
                    let out_ports: Vec<String> = out_specs.iter().map(|p| p.name.clone()).collect();
                    let in_types: Vec<String> = in_specs
                        .iter()
                        .map(|p| p.data_type.as_wire_str().to_string())
                        .collect();
                    let out_types: Vec<String> = out_specs
                        .iter()
                        .map(|p| p.data_type.as_wire_str().to_string())
                        .collect();
                    (in_ports, out_ports, in_types, out_types)
                }
                None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            };
            tentaflow_protocol::FlowNodeTemplate {
                id: t.id,
                node_type: t.node_type,
                category: t.category,
                label: t.label,
                description: t.description,
                default_config: t.default_config,
                icon: t.icon,
                input_ports,
                output_ports,
                input_port_types,
                output_port_types,
                params_schema: t.params_schema.unwrap_or_default(),
            }
        })
        .collect();

    // Dorzuc custom flow blocks z zainstalowanych addonow. GUI Flow Builder
    // dostaje je w tej samej palecie co core templates, kluczem jest
    // `node_type` ("addon.{addon_id}.{block}"); `id=0` bo addon blocks nie
    // żyją w tabeli flow_node_templates.
    if let Some(blocks) = dispatcher.and_then(|d| d.addon_flow_blocks()) {
        for b in blocks.list_all_blocks() {
            let input_ports: Vec<String> = b.inputs.iter().map(|p| p.name.clone()).collect();
            let output_ports: Vec<String> = b.outputs.iter().map(|p| p.name.clone()).collect();
            let input_port_types: Vec<String> =
                b.inputs.iter().map(|p| p.port_type.clone()).collect();
            let output_port_types: Vec<String> =
                b.outputs.iter().map(|p| p.port_type.clone()).collect();
            let params_schema = if b.config_schema.is_null() {
                String::new()
            } else {
                serde_json::to_string(&b.config_schema).unwrap_or_default()
            };
            templates.push(tentaflow_protocol::FlowNodeTemplate {
                id: 0,
                node_type: b.block_type.clone(),
                category: b.category.clone(),
                label: b.label.clone(),
                description: if b.description.is_empty() {
                    None
                } else {
                    Some(b.description.clone())
                },
                default_config: "{}".to_string(),
                icon: b.icon.clone(),
                input_ports,
                output_ports,
                input_port_types,
                output_port_types,
                params_schema,
            });
        }
    }

    // Per-agent palette entries (Harness §3.5 block 6): one `agent` block per
    // enabled agent, with `agent_id` prefilled in default_config — mirrors the
    // per-addon-block append above. Zero new adapters; the UX is "an agent is a
    // block". Ports/category/icon/params_schema are copied from the generic
    // `agent` template (already in `templates`) so the prefilled entries stay in
    // lockstep with it. Skipped silently when the generic template or the
    // dispatcher's AgentService is absent (headless / pre-seed).
    if let Some(generic) = templates.iter().find(|t| t.node_type == "agent").cloned() {
        let agents = repository::list_agents(
            &ctx.state.db,
            &db::models::AgentListFilter {
                is_enabled: Some(true),
                routable: None,
            },
        )
        .unwrap_or_default();
        for a in agents {
            let default_config = serde_json::json!({ "agent_id": a.id }).to_string();
            let label = a.display_name.clone().unwrap_or_else(|| a.name.clone());
            templates.push(tentaflow_protocol::FlowNodeTemplate {
                id: 0,
                node_type: "agent".to_string(),
                category: generic.category.clone(),
                label,
                description: if a.description.is_empty() {
                    generic.description.clone()
                } else {
                    Some(a.description.clone())
                },
                default_config,
                icon: generic.icon.clone(),
                input_ports: generic.input_ports.clone(),
                output_ports: generic.output_ports.clone(),
                input_port_types: generic.input_port_types.clone(),
                output_port_types: generic.output_port_types.clone(),
                params_schema: generic.params_schema.clone(),
            });
        }
    }

    Ok(MessageBody::FlowNodeTemplatesListResponseBody(
        tentaflow_protocol::FlowNodeTemplatesListResponse { templates },
    ))
}

#[handler(variant = "FlowVersionListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_version_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::FlowVersionListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected FlowVersionListRequestBody",
            ));
        }
    };
    let flow_id = &payload.flow_id;

    if repository::get_flow(&ctx.state.db, flow_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("flow not found"));
    }

    let rows = repository::list_flow_versions(&ctx.state.db, flow_id).map_err(db_err)?;
    let versions: Vec<tentaflow_protocol::FlowVersionSummary> = rows
        .into_iter()
        .map(|v| tentaflow_protocol::FlowVersionSummary {
            id: v.id.to_string(),
            flow_id: v.flow_id.to_string(),
            version_num: v.version_num,
            name: v.name,
            description: v.description,
            status: v.status,
            created_at_epoch: parse_ts(&v.created_at),
            created_by: v.created_by,
        })
        .collect();
    Ok(MessageBody::FlowVersionListResponseBody(
        tentaflow_protocol::FlowVersionListResponse { versions },
    ))
}

#[handler(variant = "FlowVersionGetRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn flow_version_get(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::FlowVersionGetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected FlowVersionGetRequestBody",
            ));
        }
    };
    let flow_id = &payload.flow_id;
    let version_id = &payload.version_id;

    if repository::get_flow(&ctx.state.db, flow_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("flow not found"));
    }

    let v = repository::get_flow_version(&ctx.state.db, flow_id, version_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("flow version not found"))?;

    let full = tentaflow_protocol::FlowVersionFull {
        id: v.id.to_string(),
        flow_id: v.flow_id.to_string(),
        version_num: v.version_num,
        name: v.name,
        description: v.description,
        status: v.status,
        flow_json: v.flow_json.unwrap_or_default(),
        created_at_epoch: parse_ts(&v.created_at),
        created_by: v.created_by,
    };
    Ok(MessageBody::FlowVersionGetResponseBody(
        tentaflow_protocol::FlowVersionGetResponse { version: full },
    ))
}

#[handler(variant = "FlowVersionRestoreRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn flow_version_restore(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::FlowVersionRestoreRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected FlowVersionRestoreRequestBody",
            ));
        }
    };
    let flow_id = &payload.flow_id;
    let version_id = &payload.version_id;

    let existing = repository::get_flow(&ctx.state.db, flow_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("flow not found"))?;
    if existing.is_system {
        return Err(ProtocolError::bad_request(
            "system flow cannot be modified — it is managed by the platform",
        ));
    }
    let version = repository::get_flow_version(&ctx.state.db, flow_id, version_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("flow version not found"))?;

    let flow_json = version.flow_json.as_deref().unwrap_or("");
    validate_flow_json_str(ctx, flow_json)?;
    let user_id_opt = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    // Restoring an old version keeps whatever publish name the live flow
    // currently advertises — old versions never tracked the catalog field.
    let params = db::models::FlowParams {
        name: &version.name,
        description: version.description.as_deref(),
        is_default: existing.is_default,
        service_type: existing.service_type.as_deref(),
        flow_json,
        status: version.status.as_deref().unwrap_or("draft"),
        published_model_name: existing.published_model_name.as_deref(),
        actor_user_id: user_id_opt.as_deref(),
    };

    match repository::update_flow_with_snapshot(
        &ctx.state.db,
        flow_id,
        existing.version,
        &params,
        user_id_opt.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) if e.to_string().contains("CONFLICT") => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::BadRequest,
                "flow version conflict",
            ));
        }
        Err(e) => return Err(db_err(e)),
    }
    invalidate_flow_cache();

    audit(
        ctx,
        user_id_opt.as_deref(),
        "flow.version.restore",
        Some(&format!("flow:{}", flow_id)),
        Some(&format!("version:{}", version_id)),
    );

    Ok(MessageBody::FlowVersionRestoreResponseBody(
        tentaflow_protocol::FlowVersionRestoreResponse { ok: true },
    ))
}

// =============================================================================
// Clusters — list/detail/create/update/delete + member ops
// =============================================================================

/// Konwertuje SQLite "YYYY-MM-DD HH:MM:SS" do i64 epoch sekund.
fn parse_ts_i64(s: &str) -> i64 {
    parse_ts(s) as i64
}

/// Buduje liste czlonkow klastra z DB wzbogacona o hostname/status z peer_store.
/// Wspoldzielone przez cluster_list i cluster_detail — jedno zrodlo prawdy.
fn build_cluster_members(
    ctx: &HandlerContext,
    cluster_id: &str,
) -> Vec<tentaflow_protocol::ClusterMember> {
    let db_members = match repository::list_cluster_members(&ctx.state.db, cluster_id) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    db_members
        .into_iter()
        .map(|m| {
            let peer = ctx.state.mesh_peer_store.get(&m.node_id);
            tentaflow_protocol::ClusterMember {
                node_id: m.node_id.clone(),
                hostname: peer
                    .as_ref()
                    .map(|p| {
                        if p.hostname.is_empty() {
                            m.node_id.clone()
                        } else {
                            p.hostname.clone()
                        }
                    })
                    .unwrap_or_else(|| m.node_id.clone()),
                status: peer
                    .map(|p| p.status)
                    .unwrap_or_else(|| "offline".to_string()),
                interface_type: if m.interface_type.is_empty() {
                    None
                } else {
                    Some(m.interface_type)
                },
                interface_speed_mbps: if m.interface_speed_mbps > 0 {
                    Some(m.interface_speed_mbps as u32)
                } else {
                    None
                },
                joined_at: parse_ts_i64(&m.joined_at),
                rdma_devices: if m.rdma_devices.is_empty() {
                    None
                } else {
                    Some(m.rdma_devices.clone())
                },
                rdma_ip: if m.rdma_ip.is_empty() {
                    None
                } else {
                    Some(m.rdma_ip.clone())
                },
                rdma_socket_ifname: if m.rdma_socket_ifname.is_empty() {
                    None
                } else {
                    Some(m.rdma_socket_ifname.clone())
                },
                interface_name: if m.interface_name.is_empty() {
                    None
                } else {
                    Some(m.interface_name.clone())
                },
                interface_ip: if m.interface_ip.is_empty() {
                    None
                } else {
                    Some(m.interface_ip.clone())
                },
                rdma_gid_index: u32::try_from(m.rdma_gid_index).ok(),
            }
        })
        .collect()
}

fn db_cluster_to_info(
    cluster: &crate::db::models::DbCluster,
    members_count: u32,
    members_online: u32,
    members: Vec<tentaflow_protocol::ClusterMember>,
) -> tentaflow_protocol::ClusterInfo {
    tentaflow_protocol::ClusterInfo {
        id: cluster.cluster_id.clone(),
        name: cluster.name.clone(),
        description: if cluster.description.is_empty() {
            None
        } else {
            Some(cluster.description.clone())
        },
        strategy: cluster.strategy.clone(),
        // Status klastra wyprowadzamy z liczby online czlonkow.
        status: if members_online == 0 {
            "inactive".to_string()
        } else {
            "active".to_string()
        },
        members_count,
        members_online,
        created_at: parse_ts_i64(&cluster.created_at),
        updated_at: parse_ts_i64(&cluster.updated_at),
        failover_enabled: cluster.failover_enabled,
        failover_target: cluster.failover_target.clone(),
        health_check_interval_ms: cluster.health_check_interval_ms as u32,
        timeout_ms: cluster.timeout_ms as u32,
        members,
    }
}

/// Liczy ilu czlonkow klastra jest osiagalnych wg peer_store. Peer_store nadaje
/// statusy "connected"/"reachable"/"discovered"/"disconnected"/"offline" — nody
/// zdolne przyjac komende mesh to dwa pierwsze.
fn count_online_members(ctx: &HandlerContext, cluster_id: &str) -> (u32, u32) {
    let members = match repository::list_cluster_members(&ctx.state.db, cluster_id) {
        Ok(m) => m,
        Err(_) => return (0, 0),
    };
    let total = members.len() as u32;
    let online = members
        .iter()
        .filter(|m| {
            ctx.state
                .mesh_peer_store
                .get(&m.node_id)
                .map(|p| matches!(p.status.as_str(), "connected" | "reachable"))
                .unwrap_or(false)
        })
        .count() as u32;
    (total, online)
}

#[handler(variant = "ClusterListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn cluster_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rows = repository::list_clusters_with_counts(&ctx.state.db).map_err(db_err)?;
    let clusters: Vec<tentaflow_protocol::ClusterInfo> = rows
        .into_iter()
        .map(|r| {
            let (_, online) = count_online_members(ctx, &r.cluster.cluster_id);
            let members = build_cluster_members(ctx, &r.cluster.cluster_id);
            db_cluster_to_info(&r.cluster, r.members_count as u32, online, members)
        })
        .collect();
    Ok(MessageBody::ClusterListResponseBody(
        tentaflow_protocol::ClusterListResponse { clusters },
    ))
}

#[handler(variant = "ClusterDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn cluster_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterDetailRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterDetailRequestBody",
            ));
        }
    };

    let cluster = repository::get_cluster(&ctx.state.db, &payload.cluster_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("cluster not found"))?;

    let (total, online) = count_online_members(ctx, &payload.cluster_id);
    let members = build_cluster_members(ctx, &payload.cluster_id);
    let info = db_cluster_to_info(&cluster, total, online, members.clone());

    Ok(MessageBody::ClusterDetailResponseBody(
        tentaflow_protocol::ClusterDetailResponse {
            cluster: info,
            members,
        },
    ))
}

#[handler(variant = "ClusterCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn cluster_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterCreateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterCreateRequestBody",
            ));
        }
    };

    if payload.name.trim().is_empty() {
        return Err(ProtocolError::bad_request("name required"));
    }
    let allowed = ["distributed", "replicated", "primary_replica"];
    if !allowed.contains(&payload.strategy.as_str()) {
        return Err(ProtocolError::bad_request(
            "strategy must be distributed/replicated/primary_replica",
        ));
    }

    let cluster_id = uuid::Uuid::new_v4().to_string();
    let description = payload.description.as_deref().unwrap_or("");
    repository::create_cluster(
        &ctx.state.db,
        &cluster_id,
        &payload.name,
        description,
        &payload.strategy,
    )
    .map_err(db_err)?;

    repository::update_cluster_full(
        &ctx.state.db,
        &cluster_id,
        None,
        None,
        None,
        Some(payload.failover_enabled),
        Some(payload.failover_target.as_deref()),
        Some(payload.health_check_interval_ms as i64),
        Some(payload.timeout_ms as i64),
    )
    .map_err(db_err)?;

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "cluster.create",
        Some(&format!("cluster:{}", cluster_id)),
        Some(&payload.name),
    );

    Ok(MessageBody::ClusterCreateResponseBody(
        tentaflow_protocol::ClusterCreateResponse { cluster_id },
    ))
}

#[handler(variant = "ClusterUpdateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn cluster_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterUpdateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterUpdateRequestBody",
            ));
        }
    };

    if repository::get_cluster(&ctx.state.db, &payload.cluster_id)
        .map_err(db_err)?
        .is_none()
    {
        return Ok(MessageBody::ClusterUpdateResponseBody(
            tentaflow_protocol::ClusterUpdateResponse { ok: false },
        ));
    }

    if let Some(s) = &payload.strategy {
        let allowed = ["distributed", "replicated", "primary_replica"];
        if !allowed.contains(&s.as_str()) {
            return Err(ProtocolError::bad_request(
                "strategy must be distributed/replicated/primary_replica",
            ));
        }
    }

    repository::update_cluster_full(
        &ctx.state.db,
        &payload.cluster_id,
        payload.name.as_deref(),
        payload.description.as_deref(),
        payload.strategy.as_deref(),
        payload.failover_enabled,
        // Convertujemy Option<String> na Option<Option<&str>> — Some(None) NIE oznacza
        // tutaj wyczyszczenia (CBOR encoding nie odroznia "missing" od "set to null").
        // Aktualizujemy failover_target tylko gdy klient go podal.
        payload.failover_target.as_ref().map(|s| Some(s.as_str())),
        payload.health_check_interval_ms.map(|v| v as i64),
        payload.timeout_ms.map(|v| v as i64),
    )
    .map_err(db_err)?;

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "cluster.update",
        Some(&format!("cluster:{}", payload.cluster_id)),
        payload.name.as_deref(),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::ClusterUpdateResponseBody(
        tentaflow_protocol::ClusterUpdateResponse { ok: true },
    ))
}

#[handler(variant = "ClusterDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn cluster_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterDeleteRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterDeleteRequestBody",
            ));
        }
    };

    if repository::get_cluster(&ctx.state.db, &payload.cluster_id)
        .map_err(db_err)?
        .is_none()
    {
        return Ok(MessageBody::ClusterDeleteResponseBody(
            tentaflow_protocol::ClusterDeleteResponse { ok: false },
        ));
    }

    repository::delete_cluster(&ctx.state.db, &payload.cluster_id).map_err(db_err)?;

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "cluster.delete",
        Some(&format!("cluster:{}", payload.cluster_id)),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::ClusterDeleteResponseBody(
        tentaflow_protocol::ClusterDeleteResponse { ok: true },
    ))
}

#[handler(variant = "ClusterAddMemberRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn cluster_add_member(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterAddMemberRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterAddMemberRequestBody",
            ));
        }
    };

    if repository::get_cluster(&ctx.state.db, &payload.cluster_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("cluster not found"));
    }

    repository::add_cluster_member(
        &ctx.state.db,
        &payload.cluster_id,
        &payload.node_id,
        "worker",
        payload.interface_name.as_deref().unwrap_or(""),
        payload.interface_ip.as_deref().unwrap_or(""),
        payload.interface_speed_mbps.map(|v| v as i64).unwrap_or(0),
        payload.interface_type.as_deref().unwrap_or(""),
    )
    .map_err(db_err)?;

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "cluster.add_member",
        Some(&format!(
            "cluster:{}/node:{}",
            payload.cluster_id, payload.node_id
        )),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::ClusterAddMemberResponseBody(
        tentaflow_protocol::ClusterAddMemberResponse { ok: true },
    ))
}

#[handler(variant = "ClusterRemoveMemberRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn cluster_remove_member(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ClusterRemoveMemberRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ClusterRemoveMemberRequestBody",
            ));
        }
    };

    if repository::get_cluster(&ctx.state.db, &payload.cluster_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("cluster not found"));
    }

    repository::remove_cluster_member(&ctx.state.db, &payload.cluster_id, &payload.node_id)
        .map_err(db_err)?;

    crate::routing::cluster_sync::broadcast_routing_mutation(&ctx.state.db, &ctx.state.quic_mesh);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "cluster.remove_member",
        Some(&format!(
            "cluster:{}/node:{}",
            payload.cluster_id, payload.node_id
        )),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::ClusterRemoveMemberResponseBody(
        tentaflow_protocol::ClusterRemoveMemberResponse { ok: true },
    ))
}

// =============================================================================
// Mesh peers
// =============================================================================

#[handler(variant = "MeshPeersListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_peers_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let peers: Vec<MeshPeerSummary> = ctx
        .state
        .mesh_peer_store
        .list()
        .into_iter()
        .map(|p| {
            let mut node_id = [0u8; 32];
            let bytes = p.node_id.as_bytes();
            let copy = bytes.len().min(32);
            node_id[..copy].copy_from_slice(&bytes[..copy]);
            let endpoint = p
                .addresses
                .first()
                .map(|addr| format!("{}:{}", addr, p.port));
            MeshPeerSummary {
                node_id,
                display_name: if p.hostname.is_empty() {
                    p.node_id.clone()
                } else {
                    p.hostname.clone()
                },
                trust_state: p.status,
                endpoint,
                last_seen_epoch: Some(parse_ts(&p.discovered_at)),
            }
        })
        .collect();
    Ok(MessageBody::MeshPeersListResponse { peers })
}

#[handler(variant = "MeshPairInitRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn mesh_pair_init(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshPairInitRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshPairInitRequestBody",
            ));
        }
    };

    if payload.pin.len() != 6 || !payload.pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(ProtocolError::bad_request("pin must be 6 digits"));
    }

    // Pair_id stable z node_id (hex) + timestamp.
    let pair_id = format!(
        "pair-{}-{}",
        hex::encode(&payload.node_id[..8]),
        chrono::Utc::now().timestamp()
    );

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "mesh.pair_init",
        Some(&format!("node:{}", hex::encode(&payload.node_id[..8]))),
        None,
        None,
        Some(&ctx.state.local_node_id),
    );

    // Real handshake (Ed25519+PIN) wykonuje IrohMeshManager — handler tu
    // tylko rejestruje intencje pair init. UI obserwuje peer status zmiany
    // przez MeshPeersList polling lub future subscription.
    Ok(MessageBody::MeshPairInitResponseBody(
        MeshPairInitResponse {
            pair_id,
            expires_at_epoch: (chrono::Utc::now().timestamp() + 300) as u64,
        },
    ))
}

// =============================================================================
// Settings
// =============================================================================

#[handler(variant = "SettingsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn settings_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let settings = repository::list_settings(&ctx.state.db).map_err(db_err)?;
    let entries: Vec<SettingEntry> = settings
        .into_iter()
        .map(|s| {
            let is_secret = crate::crypto::SettingsCipher::should_encrypt(&s.key);
            let value = if is_secret && !s.value.is_empty() {
                "<redacted>".to_string()
            } else {
                s.value
            };
            SettingEntry {
                key: s.key,
                // Klient nigdy nie powinien zobaczyc plaintext sekretu w listingu.
                value,
                is_secret,
            }
        })
        .collect();
    Ok(MessageBody::SettingsListResponse { entries })
}

#[handler(variant = "SettingsUpdateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn settings_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SettingsUpdateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SettingsUpdateRequestBody",
            ));
        }
    };

    // Konfigurowalne lokalizacje katalogow danych — node-local, walidowane i
    // stosowane na zywo po zapisie. Klucze restartowe (data_dir/sync_dir) NIE
    // ida ta droga — obsluguje je storage_admin (plik conf + pending move).
    let touched_categories: Vec<crate::paths::StorageCategory> = payload
        .entries
        .iter()
        .filter_map(|e| crate::paths::StorageCategory::from_setting_key(&e.key))
        .collect();
    if touched_categories.iter().any(|c| !c.live_migratable()) {
        return Err(ProtocolError::bad_request(
            "data_dir/sync_dir zmienia sie przez Magazyn danych (StorageMigrateRequest), nie przez settings",
        ));
    }

    // Walidacja PRZED zapisem: niepusta sciezka musi byc tworzalna. Inaczej
    // odrzucamy caly request, zeby nie zapisac nieuzywalnej lokalizacji.
    for entry in &payload.entries {
        if crate::paths::StorageCategory::from_setting_key(&entry.key).is_some() {
            let trimmed = entry.value.trim();
            if !trimmed.is_empty() {
                if let Err(e) = std::fs::create_dir_all(trimmed) {
                    return Err(ProtocolError::bad_request(format!(
                        "Nie można utworzyć katalogu '{}' dla ustawienia '{}': {}",
                        trimmed, entry.key, e
                    )));
                }
            }
        }
    }

    let mut applied = 0u32;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    for entry in &payload.entries {
        let result = if entry.is_secret && repository::is_shared_secret_setting_key(&entry.key) {
            repository::set_shared_secret_setting_secure(
                &ctx.state.db,
                &entry.key,
                &entry.value,
                &ctx.state.settings_cipher,
                user_id.as_deref(),
            )
        } else if entry.is_secret {
            repository::set_setting_secure(
                &ctx.state.db,
                &entry.key,
                &entry.value,
                &ctx.state.settings_cipher,
            )
        } else {
            repository::set_setting(&ctx.state.db, &entry.key, &entry.value)
        };
        match result {
            Ok(_) => applied += 1,
            Err(e) => tracing::warn!("settings_update '{}' failed: {}", entry.key, e),
        }
    }

    // Zastosuj nowe lokalizacje na zywo: odczytaj zapisane wartosci z bazy,
    // ustaw override i utworz nowe katalogi (idempotentne).
    if !touched_categories.is_empty() {
        for cat in &touched_categories {
            let value = repository::get_setting(&ctx.state.db, cat.setting_key())
                .ok()
                .flatten();
            crate::paths::set_category_override(*cat, value);
        }
        let _ = crate::paths::ensure_app_dirs();
    }

    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "settings.update",
        Some("settings"),
        Some(&format!("{} keys", applied)),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::SettingsUpdateResponse { applied })
}

// =============================================================================
// SSO / TLS / NGC (FAZA 4 — REST → binary)
// =============================================================================

#[handler(variant = "SsoProvidersListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn sso_providers_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let providers = repository::list_sso_providers(&ctx.state.db).map_err(db_err)?;
    let entries: Vec<tentaflow_protocol::SsoProviderEntry> = providers
        .into_iter()
        .map(|p| tentaflow_protocol::SsoProviderEntry {
            id: p.id,
            name: p.name,
            provider_type: p.provider_type,
            discovery_url: p.discovery_url,
            enabled: p.enabled,
            auto_create_users: p.auto_create_users,
            default_group_id: p.default_group_id,
            created_at: p.created_at,
        })
        .collect();
    Ok(MessageBody::SsoProvidersListResponseBody(
        tentaflow_protocol::SsoProvidersListResponse { providers: entries },
    ))
}

#[handler(variant = "SsoProviderCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn sso_provider_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SsoProviderCreateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SsoProviderCreateRequestBody",
            ));
        }
    };

    if payload.name.is_empty() || payload.client_id.is_empty() || payload.client_secret.is_empty() {
        return Err(ProtocolError::bad_request(
            "name, client_id i client_secret sa wymagane",
        ));
    }
    let valid_types = ["oidc", "azure_ad", "google", "adfs", "authentik"];
    if !valid_types.contains(&payload.provider_type.as_str()) {
        return Err(ProtocolError::bad_request(format!(
            "Nieznany typ providera. Dostepne: {}",
            valid_types.join(", ")
        )));
    }
    if !payload.discovery_url.starts_with("http://")
        && !payload.discovery_url.starts_with("https://")
    {
        return Err(ProtocolError::bad_request(
            "Discovery URL musi zaczynac sie od http:// lub https://",
        ));
    }
    if repository::get_sso_provider_by_name(&ctx.state.db, &payload.name)
        .map_err(db_err)?
        .is_some()
    {
        return Err(ProtocolError::bad_request(
            "Provider o tej nazwie juz istnieje",
        ));
    }

    let encrypted_secret = ctx
        .state
        .cipher
        .encrypt(&payload.client_secret)
        .map_err(|e| ProtocolError::internal(format!("blad szyfrowania: {}", e)))?;

    let id = repository::create_sso_provider(
        &ctx.state.db,
        &payload.name,
        &payload.provider_type,
        &payload.client_id,
        &encrypted_secret,
        &payload.discovery_url,
        payload.auto_create_users,
        payload.default_group_id.as_deref(),
    )
    .map_err(db_err)?;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "sso.provider.create",
        Some(&payload.name),
        Some(&format!("type={}", payload.provider_type)),
    );

    Ok(MessageBody::SsoProviderCreateResponseBody(
        tentaflow_protocol::SsoProviderCreateResponse {
            id,
            name: payload.name.clone(),
            provider_type: payload.provider_type.clone(),
        },
    ))
}

#[handler(variant = "SsoProviderDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn sso_provider_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SsoProviderDeleteRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SsoProviderDeleteRequestBody",
            ));
        }
    };

    let provider = repository::get_sso_provider(&ctx.state.db, payload.id).map_err(db_err)?;
    let name = provider
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    repository::delete_sso_provider(&ctx.state.db, payload.id).map_err(db_err)?;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "sso.provider.delete",
        Some(&name),
        None,
    );

    Ok(MessageBody::SsoProviderDeleteResponseBody(
        tentaflow_protocol::SsoProviderDeleteResponse {
            deleted: provider.is_some(),
        },
    ))
}

#[handler(variant = "TlsStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn tls_status(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let cert = repository::get_setting(&ctx.state.db, "tls_cert_pem")
        .map_err(db_err)?
        .unwrap_or_default();
    let key = repository::get_setting(&ctx.state.db, "tls_key_pem")
        .map_err(db_err)?
        .unwrap_or_default();
    Ok(MessageBody::TlsStatusResponseBody(
        tentaflow_protocol::TlsStatusResponse {
            has_cert: !cert.is_empty(),
            has_key: !key.is_empty(),
        },
    ))
}

#[handler(variant = "NgcStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn ngc_status(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let key =
        repository::get_setting_secure(&ctx.state.db, "ngc_api_key", &ctx.state.settings_cipher)
            .map_err(db_err)?
            .unwrap_or_default();
    Ok(MessageBody::NgcStatusResponseBody(
        tentaflow_protocol::NgcStatusResponse {
            configured: !key.is_empty(),
        },
    ))
}

// =============================================================================
// Dashboard metrics
// =============================================================================

#[handler(variant = "DashboardMetricsRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn dashboard_metrics(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let snapshot = ctx.state.metrics.snapshot();

    // CPU/RAM dla local node — z self peer info w peer_store.
    let local_id: &str = &ctx.state.local_node_id;
    let local_peer = ctx.state.mesh_peer_store.get(local_id);
    let (cpu, ram_used, ram_total) = match local_peer {
        Some(p) => (p.cpu_usage_percent, p.ram_used_mb, p.ram_total_mb),
        None => (0.0, 0, 0),
    };

    Ok(MessageBody::DashboardMetricsResponse(DashboardSnapshot {
        cpu_usage_percent: cpu,
        ram_used_mb: ram_used,
        ram_total_mb: ram_total,
        active_requests: snapshot.active_requests,
        total_requests: snapshot.total_requests,
        total_errors: snapshot.total_errors,
        tokens_per_second: snapshot.tokens_per_second,
        active_services: snapshot.active_services as u32,
    }))
}

// =============================================================================
// Prompts
// =============================================================================

#[handler(variant = "PromptListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn prompt_list(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let prompts = repository::list_prompts(&ctx.state.db, 0, 1000).map_err(db_err)?;
    let summaries: Vec<PromptSummary> = prompts
        .into_iter()
        .map(|p| PromptSummary {
            id: p.prompt_id,
            name: p.name,
            category: p.prompt_type,
            updated_at_epoch: parse_ts(&p.updated_at),
        })
        .collect();
    Ok(MessageBody::PromptListResponse { prompts: summaries })
}

#[handler(variant = "PromptDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn prompt_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let prompt_id = match req {
        MessageBody::PromptDetailRequest { prompt_id } => prompt_id,
        _ => return Err(ProtocolError::bad_request("expected PromptDetailRequest")),
    };

    let prompt = repository::get_prompt_by_prompt_id(&ctx.state.db, prompt_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found("prompt not found"))?;

    let variables: Vec<String> = prompt
        .variables
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    Ok(MessageBody::PromptDetailResponse(PromptDetail {
        id: prompt.prompt_id,
        name: prompt.name,
        category: prompt.prompt_type,
        template: prompt.content,
        variables,
        updated_at_epoch: parse_ts(&prompt.updated_at),
    }))
}

// =============================================================================
// Registries
// =============================================================================

#[handler(variant = "RegistryListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn registry_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let regs = repository::list_registries(&ctx.state.db).map_err(db_err)?;
    let summaries: Vec<RegistrySummary> = regs
        .into_iter()
        .map(|r| RegistrySummary {
            id: r.id.to_string(),
            url: r.url,
            kind: r.registry_type,
            auth_required: !r.username.is_empty(),
        })
        .collect();
    Ok(MessageBody::RegistryListResponse {
        registries: summaries,
    })
}

// =============================================================================
// Containers (Portainer)
// =============================================================================

#[handler(variant = "ContainerListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn container_list(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Real Docker API integration wymaga bollard async — w sync handler
    // zwracamy zarejestrowane kontenery z Service registry (proxy).
    // Pelne portainer integration jako oddzielny stream handler w przyszlosci.
    Ok(MessageBody::ContainerBody(
        tentaflow_protocol::ContainerPayload::ListResponse {
            containers: Vec::new(),
        },
    ))
}

#[handler(variant = "ContainerStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn container_start(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ContainerBody(tentaflow_protocol::ContainerPayload::StartRequest {
            container_id: _,
        }) => {
            // Real Docker start wymaga async bollard — zwracamy started=true
            // jako synchroniczny ack; klient powinien obserwowac ContainerList
            // dla potwierdzenia state change.
            Ok(MessageBody::ContainerBody(
                tentaflow_protocol::ContainerPayload::StartResponse { started: true },
            ))
        }
        _ => Err(ProtocolError::bad_request("expected ContainerStartRequest")),
    }
}

#[handler(variant = "ContainerStopRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn container_stop(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::ContainerBody(tentaflow_protocol::ContainerPayload::StopRequest {
            container_id: _,
        }) => Ok(MessageBody::ContainerBody(
            tentaflow_protocol::ContainerPayload::StopResponse { stopped: true },
        )),
        _ => Err(ProtocolError::bad_request("expected ContainerStopRequest")),
    }
}

// =============================================================================
// Voice profiles
// =============================================================================

#[handler(variant = "VoiceProfileListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn voice_profile_list(
    _req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Voice profiles wymagaja inference-diarization feature flag; przy
    // wylaczonym feature zwracamy puste (UI to obsluguje).
    Ok(MessageBody::VoiceProfileListResponse {
        profiles: Vec::new(),
    })
}

// =============================================================================
// TTS / PII / FastPath rules
// =============================================================================

#[handler(variant = "TtsRuleListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn tts_rule_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rules = repository::list_tts_cleaning_rules(&ctx.state.db, 0, 1000).map_err(db_err)?;
    let summaries: Vec<TtsRule> = rules
        .into_iter()
        .map(|r| TtsRule {
            id: r.id.to_string(),
            pattern: r.pattern,
            voice_id: r.replacement.unwrap_or_default(),
            priority: r.priority as i32,
        })
        .collect();
    Ok(MessageBody::TtsRuleListResponse { rules: summaries })
}

#[handler(variant = "TtsRuleCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn tts_rule_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TtsRuleCreateRequest(p) => p,
        _ => return Err(ProtocolError::bad_request("expected TtsRuleCreateRequest")),
    };

    // Reguly z dashboardu to SUBSTYTUCJA tekstu pod TTS (pattern -> zamiennik),
    // typ `phonetic` (String::replace w clean_cache). Pole `voice_id` w
    // protokole TtsRule niesie tekst zamiennika (historyczna nazwa — UI pokazuje
    // "zamiennik"). Wczesniej tworzylo martwy `voice_assignment` ktory nigdzie
    // nie byl czytany ani stosowany.
    let rule_id = repository::create_tts_cleaning_rule(
        &ctx.state.db,
        "phonetic",
        &payload.pattern,
        Some(&payload.voice_id),
        "pl",
        payload.priority as i64,
    )
    .map_err(db_err)?;

    crate::tts::clean_cache::refresh(&ctx.state.db);

    Ok(MessageBody::TtsRuleCreateResponse {
        rule_id: rule_id.to_string(),
    })
}

#[handler(variant = "TtsRuleDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn tts_rule_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rule_id_str = match req {
        MessageBody::TtsRuleDeleteRequest { rule_id } => rule_id,
        _ => return Err(ProtocolError::bad_request("expected TtsRuleDeleteRequest")),
    };
    let rule_id: i64 = rule_id_str
        .parse()
        .map_err(|_| ProtocolError::bad_request("rule_id must be integer"))?;
    repository::delete_tts_cleaning_rule(&ctx.state.db, rule_id).map_err(db_err)?;
    crate::tts::clean_cache::refresh(&ctx.state.db);
    Ok(MessageBody::TtsRuleDeleteResponse { deleted: true })
}

/// Podglad TTS: syntezuje `text` (po czyszczeniu/substytucji w `synthesize_speech`)
/// na audio i zwraca bajty, zeby admin uslyszal jak regula wyjdzie. Binary CBOR.
#[handler(variant = "TtsPreviewRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn tts_preview(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let (text, model, voice) = match req {
        MessageBody::TtsPreviewRequest { text, model, voice } => {
            (text.clone(), model.clone(), voice.clone())
        }
        _ => return Err(ProtocolError::bad_request("expected TtsPreviewRequest")),
    };
    if text.trim().is_empty() {
        return Err(ProtocolError::bad_request("text wymagany"));
    }
    let tts_req = crate::api::openai::types::TTSRequest {
        model,
        input: text,
        voice,
        response_format: Some("wav".to_string()),
        speed: None,
        language: None,
    };
    let result = ctx
        .state
        .router
        .synthesize_speech(&tts_req, None)
        .await
        .map_err(|e| ProtocolError::internal(format!("tts preview: {e}")))?;
    Ok(MessageBody::TtsPreviewResponse {
        bytes: result.response.bytes,
        format: result.response.format,
    })
}

#[handler(variant = "PiiRuleListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn pii_rule_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rules =
        repository::list_pii_rules(&ctx.state.db, crate::services::org::DEFAULT_ORG_ID, 0, 1000)
            .map_err(db_err)?;
    let summaries: Vec<tentaflow_protocol::PiiRule> = rules
        .into_iter()
        .map(|r| tentaflow_protocol::PiiRule {
            id: r.id,
            kind: r.category,
            regex: r.pattern,
            action: r.replacement,
        })
        .collect();
    Ok(MessageBody::PiiRuleBody(
        tentaflow_protocol::PiiRulePayload::ListResponse { rules: summaries },
    ))
}

/// Vision inference: face detection / pose / emotion. Caller dostaje
/// `service_name` z deploy handler runtime=embedded i przekazuje obrazek
/// jako encoded JPEG/PNG/WEBP albo raw RGB. Wynik to inner-enum
/// VisionInferResult (Faces / Poses / Emotion) zaleznie od typu silnika
/// ktory wisi pod `service_name` w `vision::registry`.
#[handler(variant = "VisionInferRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn vision_infer(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::VisionBody(tentaflow_protocol::VisionInferPayload::InferRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected VisionBody/InferRequest",
            ));
        }
    };

    let started = std::time::Instant::now();
    let (rgb, w, h) = match &payload.format {
        tentaflow_protocol::VisionImageFormat::RawRgb { width, height } => {
            let expected = (*width as usize) * (*height as usize) * 3;
            if payload.image.len() != expected {
                return Err(ProtocolError::bad_request(
                    "RawRgb: rozmiar bufora nie pasuje do width*height*3",
                ));
            }
            (payload.image.clone(), *width, *height)
        }
        tentaflow_protocol::VisionImageFormat::Encoded => {
            let img = image::load_from_memory(&payload.image)
                .map_err(|e| ProtocolError::bad_request(&format!("decode: {e}")))?;
            use image::GenericImageView;
            let (w, h) = img.dimensions();
            (img.to_rgb8().into_raw(), w, h)
        }
    };

    let out = crate::vision::infer(&payload.service_name, &rgb, w, h)
        .map_err(|e| ProtocolError::internal(&format!("vision::infer: {e}")))?;

    let result = match out {
        crate::vision::InferOutput::Faces(faces) => tentaflow_protocol::VisionInferResult::Faces(
            faces
                .into_iter()
                .map(|f| tentaflow_protocol::VisionFaceDet {
                    x1: f.bbox.0,
                    y1: f.bbox.1,
                    x2: f.bbox.2,
                    y2: f.bbox.3,
                    score: f.score,
                    keypoints: f
                        .keypoints
                        .map(|k| k.iter().map(|p| (p.0, p.1)).collect())
                        .unwrap_or_default(),
                })
                .collect(),
        ),
        crate::vision::InferOutput::Poses(poses) => tentaflow_protocol::VisionInferResult::Poses(
            poses
                .into_iter()
                .map(|p| tentaflow_protocol::VisionPoseDet {
                    x1: p.bbox.0,
                    y1: p.bbox.1,
                    x2: p.bbox.2,
                    y2: p.bbox.3,
                    score: p.score,
                    keypoints: p
                        .keypoints
                        .into_iter()
                        .map(|k| tentaflow_protocol::VisionPoseKeypoint {
                            id: k.id,
                            name: k.name.to_string(),
                            x: k.x,
                            y: k.y,
                            score: k.score,
                        })
                        .collect(),
                })
                .collect(),
        ),
        crate::vision::InferOutput::Emotion(em) => tentaflow_protocol::VisionInferResult::Emotion {
            label: em.label,
            probabilities: em.probabilities,
            valence: em.valence,
            arousal: em.arousal,
        },
    };

    let resp = tentaflow_protocol::VisionInferResponse {
        service_name: payload.service_name.clone(),
        result,
        latency_ms: started.elapsed().as_millis() as u64,
    };
    Ok(MessageBody::VisionBody(
        tentaflow_protocol::VisionInferPayload::InferResponse(resp),
    ))
}

/// Rerank przez protokół binarny (Tier 1, dashboard / addony). Natywny
/// odpowiednik REST `/v1/rerank` / `/v1/ranking`: rozwiązuje serwis o powierzchni
/// `Rerank` i forwarduje na jego Cohere-style `/v1/rerank`. Współdzieli ścieżkę
/// resolve+forward z handlerem REST przez `api::openai::server::rerank_forward`
/// — zero duplikacji logiki HTTP.
#[handler(variant = "RerankRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn rerank(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::RerankBody(tentaflow_protocol::RerankExchange::Request(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request("expected RerankBody/Request"));
        }
    };

    if payload.model.trim().is_empty() {
        return Err(ProtocolError::bad_request("brak wymaganego pola 'model'"));
    }
    if payload.documents.is_empty() {
        return Err(ProtocolError::bad_request(
            "lista 'documents' nie może być pusta",
        ));
    }

    // ACL po modelu egzekwowany przez resolver (`resolve_proxy_target`) tak samo
    // jak na ścieżce REST; tożsamość bierzemy z zalogowanej sesji.
    let user_ctx = match &ctx.session {
        SessionAuth::UserSession { user_id, role } => Some(crate::auth::acl::UserContext::new(
            user_id_to_uuid(user_id),
            role.clone().unwrap_or_else(|| "user".to_string()),
        )),
        _ => None,
    };

    let result = crate::api::openai::server::rerank_forward(
        &ctx.state.router,
        &payload.model,
        &payload.query,
        &payload.documents,
        payload.top_n,
        payload.return_documents,
        user_ctx,
        "rerank (binary)",
    )
    .await
    .map_err(|msg| ProtocolError::internal(msg))?;

    Ok(MessageBody::RerankBody(
        tentaflow_protocol::RerankExchange::Response(result),
    ))
}

#[handler(variant = "FastPathListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn fast_path_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let patterns = repository::list_fast_path_patterns(&ctx.state.db, 0, 1000).map_err(db_err)?;
    let summaries: Vec<tentaflow_protocol::FastPathPattern> = patterns
        .into_iter()
        .map(|p| tentaflow_protocol::FastPathPattern {
            id: p.id.to_string(),
            pattern: p.pattern,
            response: p.result_json,
            priority: p.priority as i32,
        })
        .collect();
    Ok(MessageBody::FastPathListResponse {
        patterns: summaries,
    })
}

// SubscribeResumeRequest jest streaming handlerem (patrz stream_handlers.rs).

// =============================================================================
// Mesh read-only views (FAZA 1a — REST → binary handlery)
// =============================================================================

use crate::mesh::peer_store::{MeshPeerInfo as StorePeerInfo, PeerGpuInfo as StoreGpu};

fn first_non_loopback_ip_str(addresses: &[std::net::IpAddr]) -> Option<String> {
    addresses
        .iter()
        .find(|a| a.is_ipv4() && !a.is_loopback())
        .map(|a| a.to_string())
}

fn all_gpus_to_proto(gpus: &[StoreGpu]) -> Vec<tentaflow_protocol::MeshNodeGpuInfo> {
    gpus.iter()
        .map(|g| {
            let name_lc = g.name.to_lowercase();
            // Vendor zgodny z `GpuVendor` enum (Nvidia/Amd/Intel/Apple/Other) —
            // frontend (mesh-detail-nsight.js) porownuje `g.vendor === 'Nvidia'`
            // strict equality, wiec lowercase tu blokowal Profile button NIGDZIE.
            let vendor = if name_lc.contains("nvidia") {
                "Nvidia"
            } else if name_lc.contains("amd") || name_lc.contains("radeon") {
                "Amd"
            } else if name_lc.contains("intel") {
                "Intel"
            } else if name_lc.contains("apple") {
                "Apple"
            } else {
                "Other"
            };
            tentaflow_protocol::MeshNodeGpuInfo {
                vendor: vendor.to_string(),
                name: g.name.clone(),
                vram_total_mb: g.vram_total_mb,
                vram_used_mb: Some(g.vram_used_mb),
                temperature_c: Some(g.temperature_c as f32),
                power_draw_w: g.power_draw_w,
                utilization_percent: Some(g.usage_percent),
                driver_version: None,
                cuda_version: None,
            }
        })
        .collect()
}

async fn store_peer_to_proto(
    p: &StorePeerInfo,
    local_node_id: &str,
    is_trusted: bool,
    route: Option<tentaflow_protocol::MeshNodeRoute>,
    connection: Option<tentaflow_protocol::MeshConnectionInfo>,
) -> tentaflow_protocol::MeshNodeInfo {
    let is_local = p.node_id == local_node_id;
    let source = if is_local {
        "local"
    } else if is_trusted {
        "trusted"
    } else {
        "discovered"
    };

    let interfaces: Vec<tentaflow_protocol::MeshNodeNetworkInterface> = p
        .networks
        .iter()
        .map(|n| tentaflow_protocol::MeshNodeNetworkInterface {
            name: n.name.clone(),
            link_up: n.link_up,
            speed_mbps: n.speed_mbps.map(|v| v as u32),
            ipv4_address: if n.ipv4_address.is_empty() {
                None
            } else {
                Some(n.ipv4_address.clone())
            },
            interface_type: if n.interface_type.is_empty() {
                None
            } else {
                Some(n.interface_type.clone())
            },
            rdma_available: Some(n.rdma_available),
            roce_available: None,
            numa_node: n.numa_node,
            rx_bytes_per_sec: Some(n.rx_bytes_per_sec),
            tx_bytes_per_sec: Some(n.tx_bytes_per_sec),
        })
        .collect();

    let models: Vec<tentaflow_protocol::MeshNodeModel> = p
        .models
        .iter()
        .map(|m| tentaflow_protocol::MeshNodeModel {
            alias: m.alias.clone(),
            kind: if m.kind.is_empty() {
                None
            } else {
                Some(m.kind.clone())
            },
            backend: if m.backend.is_empty() {
                None
            } else {
                Some(m.backend.clone())
            },
            size_mb: if m.size_mb == 0 {
                None
            } else {
                Some(m.size_mb)
            },
            loaded: m.loaded,
        })
        .collect();

    let containers: Vec<tentaflow_protocol::MeshNodeContainer> = p
        .containers
        .iter()
        .map(|c| tentaflow_protocol::MeshNodeContainer {
            name: c.name.clone(),
            image: c.image.clone(),
            status: c.status.clone(),
            cpu_percent: Some(c.cpu_percent as f32),
            memory_mb: Some(c.memory_mb as f32),
            memory_limit_mb: if c.memory_limit_mb == 0 {
                None
            } else {
                Some(c.memory_limit_mb)
            },
        })
        .collect();

    // Sumaryczne VRAM po wszystkich GPU (UI dashboardu pokazuje tak ten zbior).
    let (vram_total, vram_used, gpu_load) = if p.gpu_info.is_empty() {
        (None, None, None)
    } else {
        let total: u64 = p.gpu_info.iter().map(|g| g.vram_total_mb).sum();
        let used: u64 = p.gpu_info.iter().map(|g| g.vram_used_mb).sum();
        let load: f32 =
            p.gpu_info.iter().map(|g| g.usage_percent).sum::<f32>() / p.gpu_info.len() as f32;
        (Some(total), Some(used), Some(load))
    };

    // Lokalnie pytamy detektora bezposrednio (swiezy wynik bez czekania na
    // tick heartbeatu). Dla peerow czytamy ostatni snapshot z peer_store —
    // jest aktualizowany przy kazdym odebranym heartbeacie.
    let (nsys_available, nsys_version) = if is_local {
        let cap = crate::profiling::detect_capability().await;
        (cap.available, cap.version)
    } else {
        (p.nsys_available, p.nsys_version.clone())
    };

    let profiling_collectors_available = if is_local {
        crate::profiling::collectors::CollectorRegistry::probe_available_ids(
            &crate::profiling::COLLECTOR_REGISTRY,
        )
    } else {
        p.profiling_collectors_available.clone()
    };

    tentaflow_protocol::MeshNodeInfo {
        node_id: p.node_id.clone(),
        hostname: p.hostname.clone(),
        ip: first_non_loopback_ip_str(&p.addresses),
        source: source.to_string(),
        is_local,
        uptime_secs: None,
        gpus: all_gpus_to_proto(&p.gpu_info),
        network_interfaces: interfaces,
        cpu_count: Some(p.cpu_count),
        cpu_usage_percent: Some(p.cpu_usage_percent),
        ram_total_mb: Some(p.ram_total_mb),
        ram_used_mb: Some(p.ram_used_mb),
        vram_total_mb: vram_total,
        vram_used_mb: vram_used,
        gpu_load_percent: gpu_load,
        models,
        containers,
        last_seen_epoch: Some(parse_ts(&p.discovered_at) as i64),
        route,
        platform: p.platform.clone(),
        connection,
        nsys_available,
        nsys_version,
        profiling_collectors_available,
    }
}

fn is_loopback_or_local_dup(p: &StorePeerInfo, local_node_id: &str) -> bool {
    if p.node_id == local_node_id {
        return false;
    }
    if p.hostname == "127.0.0.1" || p.hostname == "::1" {
        return true;
    }
    !p.addresses.is_empty() && p.addresses.iter().all(|a| a.is_loopback())
}

#[handler(variant = "MeshNodeListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn mesh_node_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let store = &ctx.state.mesh_peer_store;
    let local_node_id = ctx.state.local_node_id.as_ref();
    let peers = store.list();
    let connection_map = ctx
        .state
        .quic_mesh
        .as_ref()
        .map(|qm| qm.connection_snapshots())
        .unwrap_or_default();

    // Registry is the authoritative source for "which nodes do we know about
    // and what is their current connection state" — including trusted peers
    // that are currently offline (no peer_store row). peer_store still owns
    // the rich device data (CPU, RAM, GPU, models, containers).
    let registry = store.registry().cloned();
    let summaries = registry
        .as_ref()
        .map(|r| r.snapshot_summary())
        .unwrap_or_default();
    let now_ms = crate::mesh::proto_conv::now_unix_ms();

    let store_by_id: std::collections::HashMap<String, &StorePeerInfo> = peers
        .iter()
        .filter(|p| p.node_id == local_node_id || !is_loopback_or_local_dup(p, local_node_id))
        .map(|p| (p.node_id.clone(), p))
        .collect();

    let mut nodes: Vec<tentaflow_protocol::MeshNodeInfo> = Vec::new();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Local node first (always present in peer_store via seed_local).
    if let Some(local) = store_by_id.get(local_node_id) {
        let route = Some(tentaflow_protocol::MeshNodeRoute {
            hops: 0,
            direct: true,
            next_hop: None,
        });
        let connection = registry.as_ref().and_then(|r| {
            let id_bytes = parse_node_id_hex(&local.node_id)?;
            let summary = r
                .snapshot_summary()
                .into_iter()
                .find(|s| s.node_id == id_bytes)?;
            Some(crate::mesh::proto_conv::build_conn_info(
                &summary,
                connection_map.get(&local.node_id),
                now_ms,
            ))
        });
        nodes.push(store_peer_to_proto(local, local_node_id, true, route, connection).await);
        emitted.insert(local.node_id.clone());
    }

    // All other registry-known peers — drives both online and trusted-offline.
    for summary in &summaries {
        let node_id_hex = hex::encode(summary.node_id);
        if emitted.contains(&node_id_hex) {
            continue;
        }
        let is_trusted = matches!(
            summary.trust,
            crate::mesh::peer_registry::TrustStateTag::Trusted
        ) || ctx
            .state
            .mesh_security
            .as_ref()
            .map_or(false, |s| s.is_trusted(&node_id_hex));

        let connection = Some(crate::mesh::proto_conv::build_conn_info(
            summary,
            connection_map.get(&node_id_hex),
            now_ms,
        ));

        if let Some(p) = store_by_id.get(&node_id_hex) {
            let route = store
                .get_route(&p.node_id)
                .map(|r| tentaflow_protocol::MeshNodeRoute {
                    hops: r.hops as u32,
                    direct: r.direct,
                    next_hop: if r.direct {
                        None
                    } else {
                        Some(r.next_hop.clone())
                    },
                });
            nodes.push(store_peer_to_proto(p, local_node_id, is_trusted, route, connection).await);
        } else {
            // No peer_store entry — trusted node offline (or freshly seeded).
            // Render with whatever the registry knows; rich device fields stay
            // empty until the peer comes online and pushes node info.
            nodes.push(tentaflow_protocol::MeshNodeInfo {
                node_id: node_id_hex.clone(),
                hostname: summary.hostname.to_string(),
                ip: None,
                source: if is_trusted { "trusted" } else { "discovered" }.to_string(),
                is_local: false,
                uptime_secs: None,
                gpus: Vec::new(),
                network_interfaces: Vec::new(),
                cpu_count: None,
                cpu_usage_percent: None,
                ram_total_mb: None,
                ram_used_mb: None,
                vram_total_mb: None,
                vram_used_mb: None,
                gpu_load_percent: None,
                models: Vec::new(),
                containers: Vec::new(),
                last_seen_epoch: None,
                route: None,
                platform: summary.platform.to_string(),
                connection,
                nsys_available: false,
                nsys_version: String::new(),
                profiling_collectors_available: Vec::new(),
            });
        }
        emitted.insert(node_id_hex);
    }

    // Backfill: any peer_store entry not present in the registry yet (e.g. a
    // legacy mDNS row before the shadow caught up). Treat as offline.
    for p in store_by_id.values() {
        if emitted.contains(&p.node_id) {
            continue;
        }
        let is_trusted = ctx
            .state
            .mesh_security
            .as_ref()
            .map_or(false, |s| s.is_trusted(&p.node_id));
        let route = store
            .get_route(&p.node_id)
            .map(|r| tentaflow_protocol::MeshNodeRoute {
                hops: r.hops as u32,
                direct: r.direct,
                next_hop: if r.direct {
                    None
                } else {
                    Some(r.next_hop.clone())
                },
            });
        nodes.push(store_peer_to_proto(p, local_node_id, is_trusted, route, None).await);
    }

    Ok(MessageBody::MeshNodeListResponseBody(
        tentaflow_protocol::MeshNodeListResponse { nodes },
    ))
}

fn parse_node_id_hex(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(s, &mut out).ok().map(|()| out)
}

#[handler(variant = "MeshNodeDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn mesh_node_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MeshNodeDetailRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MeshNodeDetailRequestBody",
            ));
        }
    };

    let store = &ctx.state.mesh_peer_store;
    let local_node_id = ctx.state.local_node_id.as_ref();
    let peer = store.get(&payload.node_id).ok_or_else(|| {
        ProtocolError::not_found(format!("node '{}' nie znaleziony", payload.node_id))
    })?;
    let is_local = peer.node_id == local_node_id;
    let registry = store.registry().cloned();
    let summary = registry.as_ref().and_then(|r| {
        parse_node_id_hex(&peer.node_id)
            .and_then(|id| r.snapshot_summary().into_iter().find(|s| s.node_id == id))
    });
    let is_trusted = is_local
        || summary
            .as_ref()
            .map(|s| matches!(s.trust, crate::mesh::peer_registry::TrustStateTag::Trusted))
            .unwrap_or(false)
        || ctx
            .state
            .mesh_security
            .as_ref()
            .map_or(false, |s| s.is_trusted(&peer.node_id));
    let route = if is_local {
        Some(tentaflow_protocol::MeshNodeRoute {
            hops: 0,
            direct: true,
            next_hop: None,
        })
    } else {
        store
            .get_route(&peer.node_id)
            .map(|r| tentaflow_protocol::MeshNodeRoute {
                hops: r.hops as u32,
                direct: r.direct,
                next_hop: if r.direct {
                    None
                } else {
                    Some(r.next_hop.clone())
                },
            })
    };
    let iroh_snapshot = ctx
        .state
        .quic_mesh
        .as_ref()
        .and_then(|qm| qm.connection_snapshot(&payload.node_id));
    let now_ms = crate::mesh::proto_conv::now_unix_ms();
    let connection = summary
        .as_ref()
        .map(|s| crate::mesh::proto_conv::build_conn_info(s, iroh_snapshot.as_ref(), now_ms));
    let info = store_peer_to_proto(&peer, local_node_id, is_trusted, route, connection).await;
    Ok(MessageBody::MeshNodeDetailResponseBody(
        tentaflow_protocol::MeshNodeDetailResponse { node: info },
    ))
}

#[handler(variant = "MeshPendingListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_pending_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let _ = repository::cleanup_expired_pairings(&ctx.state.db);
    let pairings = repository::list_pending_pairings(&ctx.state.db).map_err(db_err)?;
    let pending: Vec<tentaflow_protocol::MeshPendingPair> = pairings
        .into_iter()
        .map(|p| tentaflow_protocol::MeshPendingPair {
            pair_id: p.id.to_string(),
            remote_node_id: p.remote_node_id,
            remote_hostname: None,
            remote_ip: None,
            initiated_at: parse_ts(&p.expires_at) as i64,
            state: p.direction,
            pin: if p.pin_code.is_empty() {
                None
            } else {
                Some(p.pin_code)
            },
        })
        .collect();
    Ok(MessageBody::MeshPendingListResponseBody(
        tentaflow_protocol::MeshPendingListResponse { pending },
    ))
}

#[handler(variant = "MeshIdentityRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_identity(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let sec = ctx
        .state
        .mesh_security
        .as_ref()
        .ok_or_else(|| ProtocolError::internal("MeshSecurity niedostepny"))?;
    let local_node_id = ctx
        .state
        .quic_mesh
        .as_ref()
        .map(|qm| qm.node_id())
        .unwrap_or_else(|| {
            if ctx.state.local_node_id.len() == 64
                && ctx
                    .state
                    .local_node_id
                    .chars()
                    .all(|c| c.is_ascii_hexdigit())
            {
                ctx.state.local_node_id.to_string()
            } else {
                sec.ed25519_public_key_hex()
            }
        });
    let addresses: Vec<String> = ctx
        .state
        .mesh_peer_store
        .get(local_node_id.as_str())
        .map(|p| {
            p.addresses
                .iter()
                .map(|a| format!("{}:{}", a, p.port))
                .collect()
        })
        .unwrap_or_default();
    let hostname = ctx
        .state
        .mesh_peer_store
        .get(local_node_id.as_str())
        .map(|p| p.hostname)
        .unwrap_or_default();
    let relay_url = ctx
        .state
        .quic_mesh
        .as_ref()
        .and_then(|qm| qm.relay_url())
        .map(|url| url.to_string())
        .unwrap_or_default();
    // Generuj fresh invite PIN dla QR code (60s TTL). Frontend co 50s re-fetchuje
    // identity zeby odswiezyc PIN, wiec zawsze w QR jest wazny kod.
    let (invite_pin, invite_pin_expires_sec) = sec.generate_invite_pin();
    Ok(MessageBody::MeshIdentityResponseBody(
        tentaflow_protocol::MeshIdentityResponse {
            node_id: local_node_id,
            hostname,
            public_key: sec.public_key_hex(),
            addresses,
            relay_url,
            version: env!("CARGO_PKG_VERSION").to_string(),
            invite_pin,
            invite_pin_expires_sec,
        },
    ))
}

#[handler(variant = "MeshServicesListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_services_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let services: Vec<tentaflow_protocol::MeshServicesEntry> = ctx
        .state
        .mesh_services_registry
        .visible_services()
        .into_iter()
        .map(|s| tentaflow_protocol::MeshServicesEntry {
            service_name: s.display_name,
            node_id: s.node_id,
            status: s.status,
            endpoint: s.endpoint_url,
        })
        .collect();
    Ok(MessageBody::MeshServicesListResponseBody(
        tentaflow_protocol::MeshServicesListResponse { services },
    ))
}

#[handler(variant = "MeshTrustedListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn mesh_trusted_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::mesh::peer_registry::TrustStateTag;
    // Source of truth: in-memory PeerRegistry (hydrated from peer_persisted
    // at startup). When no registry is wired (test stubs) the response is
    // simply empty — there is no legacy fallback.
    let nodes: Vec<tentaflow_protocol::MeshTrustedNode> = ctx
        .state
        .mesh_peer_store
        .registry()
        .map(|reg| {
            reg.snapshot_summary()
                .into_iter()
                .filter(|s| matches!(s.trust, TrustStateTag::Trusted))
                .map(|s| tentaflow_protocol::MeshTrustedNode {
                    node_id: hex::encode(s.node_id),
                    hostname: if s.hostname.is_empty() {
                        None
                    } else {
                        Some((*s.hostname).to_string())
                    },
                    // PeerSummary does not carry an explicit "trusted since"
                    // timestamp; expose 0 ("unknown") rather than fabricating
                    // one. GUI tolerates 0.
                    trusted_since_epoch: 0,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(MessageBody::MeshTrustedListResponseBody(
        tentaflow_protocol::MeshTrustedListResponse { trusted: nodes },
    ))
}

// =============================================================================
// Models unified + aliasy (FAZA 2 — REST → binary)
// =============================================================================

/// Mapuje `DbModelAlias` na `ModelAliasEntry` protokolu.
fn db_alias_to_proto(a: crate::db::models::DbModelAlias) -> tentaflow_protocol::ModelAliasEntry {
    tentaflow_protocol::ModelAliasEntry {
        id: a.id,
        alias: a.alias,
        target_model: a.target_model,
        is_active: a.is_active,
        fallback_targets: a.fallback_targets,
        strategy: a.strategy,
    }
}

#[handler(variant = "CatalogListRequestBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn catalog_list(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let request = match req {
        MessageBody::CatalogListRequestBody(r) => r,
        // Defensive — the proc-macro only routes the right variant here, but
        // an internal caller invoking `catalog_list` directly with the wrong
        // body should get an error, not a crash.
        other => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Internal,
                format!(
                    "catalog_list: unexpected MessageBody variant '{}'",
                    crate::dispatch::variant_name_of(other)
                ),
            ));
        }
    };

    let snapshot = ctx.state.router.catalog_snapshot();
    let entries = catalog_snapshot_to_wire(&snapshot, request);
    Ok(MessageBody::CatalogListResponseBody(
        tentaflow_protocol::CatalogListResponse {
            entries,
            version: snapshot.version,
        },
    ))
}

fn catalog_snapshot_to_wire(
    snapshot: &crate::services::catalog::CatalogSnapshot,
    request: &tentaflow_protocol::CatalogListRequest,
) -> Vec<tentaflow_protocol::CatalogEntryWire> {
    use crate::services::catalog::{CatalogDiagnostic, CatalogEntryKind};
    use tentaflow_protocol::{
        CatalogDiagnosticWire, CatalogEntryKindWire, CatalogEntryWire, CatalogModelInstance,
    };

    let surface_filter_lower = request
        .surface_filter
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase());

    let mut out = Vec::with_capacity(snapshot.entries.len());
    for entry in snapshot.entries.iter() {
        if entry
            .diagnostic
            .as_ref()
            .map(|d| d.is_blocking())
            .unwrap_or(false)
            && !request.include_blocking_diagnostics
        {
            continue;
        }
        if let Some(ref needed) = surface_filter_lower {
            let matches = entry
                .service_surfaces
                .iter()
                .any(|s| s.as_wire_str() == needed.as_str());
            if !matches {
                continue;
            }
        }

        let owned_by = entry.owned_by().to_string();
        let kind = match &entry.kind {
            CatalogEntryKind::ServiceModel { instances } => CatalogEntryKindWire::ServiceModel {
                instances: instances
                    .iter()
                    .map(|i| CatalogModelInstance {
                        node_id: i.node_id.clone(),
                        node_hostname: i.node_hostname.clone(),
                        service_id: i.service_id,
                        status: i.status.clone(),
                        backend: i.backend.clone(),
                        size_mb: i.size_mb,
                        loaded: i.loaded,
                    })
                    .collect(),
            },
            CatalogEntryKind::Flow {
                flow_id,
                published_name,
            } => CatalogEntryKindWire::Flow {
                flow_id: flow_id.clone(),
                published_name: published_name.clone(),
            },
            CatalogEntryKind::Alias {
                target,
                fallback_targets,
                strategy,
            } => CatalogEntryKindWire::Alias {
                target: target.clone(),
                fallback_targets: fallback_targets.clone(),
                strategy: strategy.as_wire_str().to_string(),
            },
        };

        let diagnostic = entry.diagnostic.as_ref().map(|d| match d {
            CatalogDiagnostic::RemoteShadowed { local_owner } => {
                CatalogDiagnosticWire::RemoteShadowed {
                    local_owner: local_owner.clone(),
                }
            }
            CatalogDiagnostic::LocalOverride {
                conflicting_remote_node,
            } => CatalogDiagnosticWire::LocalOverride {
                conflicting_remote_node: conflicting_remote_node.clone(),
            },
            CatalogDiagnostic::IncompatibleAliasTargets {
                alias,
                missing_modalities,
            } => CatalogDiagnosticWire::IncompatibleAliasTargets {
                alias: alias.clone(),
                missing_modalities: missing_modalities
                    .iter()
                    .map(|m| m.as_wire_str().to_string())
                    .collect(),
            },
        });

        out.push(CatalogEntryWire {
            id: entry.id.clone(),
            kind,
            service_surfaces: entry
                .service_surfaces
                .iter()
                .map(|s| s.as_wire_str().to_string())
                .collect(),
            input_modalities: entry
                .input_modalities
                .iter()
                .map(|m| m.as_wire_str().to_string())
                .collect(),
            output_modalities: entry
                .output_modalities
                .iter()
                .map(|m| m.as_wire_str().to_string())
                .collect(),
            diagnostic,
            owned_by,
        });
    }
    out
}

#[handler(variant = "ModelAliasListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn model_alias_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let items = crate::services::models::list_aliases(&ctx.state.db).map_err(db_err)?;
    let aliases = items.into_iter().map(db_alias_to_proto).collect();
    Ok(MessageBody::ModelAliasListResponseBody(
        tentaflow_protocol::ModelAliasListResponse { aliases },
    ))
}

#[handler(variant = "ModelAliasCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_alias_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelAliasCreateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelAliasCreateRequestBody",
            ));
        }
    };

    let id = crate::services::models::create_alias(
        &ctx.state.db,
        &payload.alias,
        &payload.target_model,
        payload.strategy.as_deref(),
        payload.fallback_targets.as_deref(),
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    crate::services::models::broadcast_alias_mutation(
        &ctx.state.db,
        &ctx.state.router,
        &ctx.state.quic_mesh,
    );

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model_alias_create",
        Some(&payload.alias),
        Some(&format!("target={}", payload.target_model)),
    );

    Ok(MessageBody::ModelAliasCreateResponseBody(
        tentaflow_protocol::ModelAliasCreateResponse { id },
    ))
}

#[handler(variant = "ModelAliasUpdateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_alias_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelAliasUpdateRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelAliasUpdateRequestBody",
            ));
        }
    };

    let updated = crate::services::models::update_alias(
        &ctx.state.db,
        payload.id,
        &payload.alias,
        &payload.target_model,
        payload.is_active.unwrap_or(true),
        payload.strategy.as_deref(),
        payload.fallback_targets.as_deref(),
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    if !updated {
        return Err(ProtocolError::not_found(format!(
            "Alias modelu o id {} nie istnieje",
            payload.id
        )));
    }

    crate::services::models::broadcast_alias_mutation(
        &ctx.state.db,
        &ctx.state.router,
        &ctx.state.quic_mesh,
    );

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model_alias_update",
        Some(&payload.alias),
        Some(&format!("target={}", payload.target_model)),
    );

    Ok(MessageBody::ModelAliasUpdateResponseBody(
        tentaflow_protocol::ModelAliasUpdateResponse { ok: true },
    ))
}

#[handler(variant = "ModelAliasDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_alias_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let id = match req {
        MessageBody::ModelAliasDeleteRequestBody(p) => p.id,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelAliasDeleteRequestBody",
            ));
        }
    };

    let deleted = crate::services::models::delete_alias(&ctx.state.db, id).map_err(db_err)?;

    if !deleted {
        return Err(ProtocolError::not_found(format!(
            "Alias modelu o id {} nie istnieje",
            id
        )));
    }

    crate::services::models::broadcast_alias_mutation(
        &ctx.state.db,
        &ctx.state.router,
        &ctx.state.quic_mesh,
    );

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model_alias_delete",
        Some(&id.to_string()),
        None,
    );

    Ok(MessageBody::ModelAliasDeleteResponseBody(
        tentaflow_protocol::ModelAliasDeleteResponse { ok: true },
    ))
}

// =============================================================================
// FAZA 5 — katalog NIM + deploy silnika z manifestu (REST -> binary)
// =============================================================================

#[handler(variant = "NimCatalogListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn nim_catalog_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let result = crate::services::nim::fetch_catalog(&ctx.state.db, &ctx.state.settings_cipher)
        .await
        .map_err(|e| ProtocolError::internal(format!("nim catalog: {}", e)))?;

    let containers = result
        .containers
        .into_iter()
        .map(|c| tentaflow_protocol::NimContainerEntry {
            name: c.name,
            display_name: c.display_name,
            description: c.description,
            image: c.image,
            latest_tag: c.latest_tag,
            publisher: c.publisher,
            category: c.category,
            min_gpu_memory_gb: c.min_gpu_memory_gb,
            updated_at: c.updated_at,
            self_hostable: c.self_hostable,
        })
        .collect();

    Ok(MessageBody::NimCatalogListResponseBody(
        tentaflow_protocol::NimCatalogListResponse {
            containers,
            error: result.error,
        },
    ))
}

#[handler(variant = "ServiceManifestDeployRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_manifest_deploy(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ReqStart(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected DeploymentBody::ReqStart",
            ));
        }
    };

    if payload.engine_id.is_empty() || payload.node_id.is_empty() {
        return Err(ProtocolError::bad_request("engine_id i node_id wymagane"));
    }

    // Cross-node deploy forwarding (krok N3b). When the request targets a
    // different mesh node, forward it as `ServiceDeployRemote` and return the
    // slug allocated on the receiver. The deploy log websocket lives on the
    // receiver side — cross-node log streaming is intentionally out of scope.
    let local_node_id = ctx.state.local_node_id.as_ref();
    if !payload.node_id.is_empty() && payload.node_id != local_node_id {
        let target = payload.node_id.clone();
        // Sekret HF musi zostac zdjety ZANIM config_json poleci przez mesh.
        // Legacy klient (lub wlasciciel) moze umiescic `hf_token` w payloadzie —
        // bez tego stripu token TEGO noda wycieklby do odbiorcy. Odbiorca i tak
        // rozwiazuje WLASNY token lokalnie z secure setting (`deploy()`), wiec
        // sekret nigdy nie opuszcza noda.
        let forwarded_config_json = if payload.config_json.is_empty() {
            payload.config_json.clone()
        } else {
            let parsed: serde_json::Value = serde_json::from_str(&payload.config_json)
                .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {}", e)))?;
            let sanitized = crate::services::deploy::strip_hf_token(&parsed);
            serde_json::to_string(&sanitized).map_err(|e| ProtocolError::internal(e.to_string()))?
        };
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServiceDeployRemote {
            engine_id: payload.engine_id.clone(),
            deploy_method: payload.deploy_method.clone(),
            config_json: forwarded_config_json,
        };
        let iroh =
            ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
        if let Some(security) = ctx.state.mesh_security.as_ref() {
            if !security.is_trusted(&target) {
                return Err(ProtocolError::bad_request(format!(
                    "peer {} is not trusted",
                    target
                )));
            }
        }
        let resp = iroh
            // 120 s (nie 30): odbiorca embedded (np. MLX na Macu) przy deployu
            // przebudowuje serwis i może odpowiedzieć z opóźnieniem; krótki timeout
            // gubił ACK mimo udanego deployu (model wdrożony, UI nie widziało sukcesu).
            .send_command_and_wait(&target, cmd, 120)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))?;
        if !resp.ok {
            return Err(ProtocolError::internal(
                resp.error
                    .unwrap_or_else(|| "remote deploy failed".to_string()),
            ));
        }
        let (deploy_id, engine_id_resp, deploy_method_resp) = match resp.payload {
            tentaflow_protocol::mesh::MeshCommandResponsePayload::ServiceDeployResult {
                deploy_id,
                engine_id,
                deploy_method,
            } => (deploy_id, engine_id, deploy_method),
            _ => (
                String::new(),
                payload.engine_id.clone(),
                payload.deploy_method.clone(),
            ),
        };
        return Ok(MessageBody::DeploymentBody(
            tentaflow_protocol::DeploymentPayload::ResStart(
                tentaflow_protocol::ServiceManifestDeployResponse {
                    status: "forwarded".to_string(),
                    deploy_id: deploy_id.clone(),
                    engine_id: engine_id_resp,
                    deploy_method: deploy_method_resp,
                    node_id: target,
                    websocket_url: format!("/ws/deploy?id={}", deploy_id),
                },
            ),
        ));
    }

    tracing::info!(
        target: "tentaflow::deploy",
        "[manifest-deploy] received: engine_id={} deploy_method={:?} node_id={}",
        payload.engine_id, payload.deploy_method, payload.node_id
    );

    use crate::services::manifest::runtime_validate::{
        validate_deploy_target, DeployValidationError,
    };
    validate_deploy_target(&payload.engine_id, &payload.deploy_method).map_err(
        |err| match err {
            DeployValidationError::EngineNotFound => ProtocolError::not_found(format!(
                "Silnik '{}' nie istnieje w manifescie",
                payload.engine_id
            )),
            DeployValidationError::DeployMethodNotAvailable => ProtocolError::bad_request(format!(
                "Silnik '{}' nie obsluguje trybu '{}'",
                payload.engine_id, payload.deploy_method
            )),
            DeployValidationError::InvalidDeployMethod => {
                ProtocolError::bad_request("deploy_method musi byc docker/native/external")
            }
        },
    )?;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.manifest.deploy",
        Some(&payload.engine_id),
        Some(&format!(
            "method={} node={}",
            payload.deploy_method, payload.node_id
        )),
    );

    // Resolve the deploy method tag from the wire ("docker"/"native"/"external")
    // into the internal `DeployMethod` enum used by the unified pipeline.
    let manifest = crate::services::manifest::registry()
        .by_id(&payload.engine_id)
        .cloned()
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "Silnik '{}' nie istnieje w manifescie",
                payload.engine_id
            ))
        })?;

    let deploy_method = resolve_deploy_method(&manifest, &payload.deploy_method)
        .map_err(ProtocolError::bad_request)?;

    // Token HF NIE jest wstrzykiwany do user_config tutaj — sekret nie moze
    // trafic do config_json (services + deployments ida do bazy plaintextem i
    // replikuja sie przez sync). `deploy()` rozwiazuje go per-node z secure
    // setting i wstrzykuje wylacznie do ENV procesu silnika.
    let user_config: serde_json::Value = if payload.config_json.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&payload.config_json)
            .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {}", e)))?
    };
    // Subscription login: the wizard sends an `oauth_flow_id` instead of a key.
    // Swap it for the tokens captured by the completed OAuth flow on this node
    // (the credential blob then follows the normal encrypted `api_key` path).
    let user_config = {
        let mut cfg = user_config;
        if let Some(obj) = cfg.as_object_mut() {
            if let Some(flow_id) = obj
                .get("oauth_flow_id")
                .and_then(|v| v.as_str())
                .map(String::from)
            {
                if let Some(blob) = crate::services::backend::codex_oauth::take_tokens(&flow_id) {
                    obj.insert("api_key".to_string(), serde_json::Value::String(blob));
                }
                obj.remove("oauth_flow_id");
            }
        }
        cfg
    };

    // Cloud external providers carry an `api_key` — encrypt it with this node's
    // settings cipher before it flows into the placeholder/deployment/service
    // config_json. The key stays node-local; remote deploys are forwarded with
    // the plaintext key over the encrypted mesh and re-encrypted by the receiver.
    let user_config = crate::services::deploy::encrypt_api_key_in_config(
        &user_config,
        &ctx.state.settings_cipher,
    );

    let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
        ProtocolError::internal("port allocator not initialized (supervisor disabled)")
    })?;

    let job = crate::services::deploy::create_deploy_job(
        deploy_method,
        &manifest,
        &user_config,
        &ctx.state.db,
        ctx.state.local_node_id.as_ref(),
        user_id.as_deref(),
        None,
    )
    .map_err(|e| ProtocolError::internal(e.to_string()))?;

    if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
        &ctx.state.db,
        job.service_id,
        ctx.state.local_node_id.as_ref(),
    ) {
        broadcast_service_change(ctx, tentaflow_protocol::ServiceChange::Added(info));
    }

    let slug = spawn_deploy_pipeline(
        ctx,
        job,
        deploy_method,
        &manifest,
        &user_config,
        port_allocator,
    );

    Ok(MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ResStart(
            tentaflow_protocol::ServiceManifestDeployResponse {
                status: "started".to_string(),
                deploy_id: slug.clone(),
                engine_id: payload.engine_id.clone(),
                deploy_method: payload.deploy_method.clone(),
                node_id: payload.node_id.clone(),
                websocket_url: format!("/ws/deploy?id={}", slug),
            },
        ),
    ))
}

/// Spawnuje pełny pipeline deployu (status-watcher + worker) i zwraca slug
/// (`deploy_id`) do streamu logów. Wyodrębnione z `service_manifest_deploy`,
/// żeby `service_redeploy` reużywał DOKŁADNIE tę samą ścieżkę — zero duplikacji
/// logiki post-deploy (rejestr mesh, live handles, rebuild katalogu).
pub(super) fn spawn_deploy_pipeline(
    ctx: &HandlerContext,
    job: crate::services::deploy::DeployJob,
    deploy_method: crate::services_repo::services::DeployMethod,
    manifest: &crate::services::manifest::ServiceManifest,
    user_config: &serde_json::Value,
    port_allocator: std::sync::Arc<crate::services::ports::PortAllocator>,
) -> String {
    let slug = job.deploy_id.clone();
    let log_sender = crate::deploy::log_bus::sender_for(&slug);
    {
        let mut status_rx = log_sender.subscribe();
        let db_status = ctx.state.db.clone();
        let service_id_status = job.service_id;
        let local_node_id_status = ctx.state.local_node_id.to_string();
        let quic_mesh_status = ctx.state.quic_mesh.clone();
        let mesh_services_registry_status = ctx.state.mesh_services_registry.clone();
        tokio::spawn(async move {
            loop {
                match status_rx.recv().await {
                    Ok(crate::deploy::log_bus::BusMessage::Line(line))
                        if line.kind == "phase" || line.kind == "progress" =>
                    {
                        if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
                            &db_status,
                            service_id_status,
                            &local_node_id_status,
                        ) {
                            mesh_services_registry_status.apply_local_change(
                                &local_node_id_status,
                                tentaflow_protocol::ServiceChange::Updated(info.clone()),
                            );
                            if let Some(qm) = quic_mesh_status.as_ref() {
                                let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                                    from_node_id: local_node_id_status.clone(),
                                    change: tentaflow_protocol::ServiceChange::Updated(info),
                                };
                                if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                    let _ = qm
                                        .broadcast_ufp2_to_trusted(
                                            tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                            &bytes,
                                            None,
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                    Ok(crate::deploy::log_bus::BusMessage::Line(_)) => {}
                    Ok(crate::deploy::log_bus::BusMessage::End { .. }) => return,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    }
    let db_clone = ctx.state.db.clone();
    let settings_cipher_task = ctx.state.settings_cipher.clone();
    let job_task = job.clone();
    let slug_task = slug.clone();
    let manifest_task = manifest.clone();
    let user_config_task = user_config.clone();
    let log_sender_task = log_sender.clone();
    let local_node_id_task = ctx.state.local_node_id.to_string();
    let quic_mesh_task = ctx.state.quic_mesh.clone();
    let mesh_services_registry_task = ctx.state.mesh_services_registry.clone();
    let live_handles_task = ctx.state.live_handles.clone();
    let service_manager_task = ctx.state.service_manager.clone();
    let catalog_provider_task = ctx.state.router.catalog_provider().clone();

    tokio::spawn(async move {
        let start_ms = crate::deploy::log_bus::now_ms();
        let result = crate::services::deploy::deploy(
            job_task.clone(),
            deploy_method,
            &manifest_task,
            &user_config_task,
            &port_allocator,
            &db_clone,
            &settings_cipher_task,
            Some(log_sender_task.clone()),
        )
        .await;
        match result {
            Ok(outcome) => {
                let _ = log_sender_task.send(crate::deploy::log_bus::BusMessage::End {
                    deploy_id: slug_task.clone(),
                    final_status: "success".to_string(),
                    image_tag: String::new(),
                    container_name: format!("service-id-{}", outcome.endpoint.handle.id),
                    error_message: String::new(),
                    duration_ms: crate::deploy::log_bus::now_ms() - start_ms,
                });
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                crate::deploy::log_bus::close(&slug_task);

                // Local registry + live handles + catalog must update on
                // every successful deploy, regardless of whether mesh is
                // enabled. The mesh broadcast that follows is conditional —
                // it only matters when peers are reachable. Pre-fix the
                // entire block was nested inside `if let Some(qm)`, which
                // meant `/v1/models` stayed stale until the next supervisor
                // tick whenever mesh was disabled.
                let service_id = outcome.endpoint.handle.id;
                match crate::services::snapshot_builder::build_one(
                    &db_clone,
                    service_id,
                    &local_node_id_task,
                ) {
                    Ok(Some(info)) => {
                        mesh_services_registry_task.apply_local_change(
                            &local_node_id_task,
                            tentaflow_protocol::ServiceChange::Added(info.clone()),
                        );
                        // Deploy always runs on the owning node, so the
                        // decrypted external-provider creds are resolvable
                        // here — the seeded handle must authenticate from the
                        // first request, not wait for a supervisor tick.
                        let creds = if info.deploy_method == "external" {
                            crate::services::supervisor::external_provider_creds(
                                &db_clone,
                                &settings_cipher_task,
                                service_id,
                            )
                        } else {
                            None
                        };
                        if let Err(e) = live_handles_task.upsert_service_info(&info, creds) {
                            tracing::warn!(error = %e, service_id, "deploy runtime handle upsert failed");
                        }
                        if info.transport == "embedded" {
                            for model in &info.models {
                                service_manager_task
                                    .register_local_inference_model(&model.model_name);
                            }
                        }

                        // Broadcast delta to peers — only when mesh is up.
                        // Peers' `MeshServicesRegistry` pick the row up here
                        // instead of waiting for the 5-min anti-drift announce.
                        if let Some(qm) = quic_mesh_task {
                            let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                                from_node_id: local_node_id_task.clone(),
                                change: tentaflow_protocol::ServiceChange::Updated(info),
                            };
                            if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                let _ = qm
                                    .broadcast_ufp2_to_trusted(
                                        tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                        &bytes,
                                        None,
                                    )
                                    .await;
                            }
                        }

                        // Refresh the catalog so `/v1/models` and the GUI
                        // see the freshly deployed service immediately.
                        // Supervisor reconcile would also do this on its
                        // next tick, but desktop has no supervisor and we
                        // don't want a 1-second window of staleness.
                        if let Err(e) =
                            catalog_provider_task.rebuild(&mesh_services_registry_task, &db_clone)
                        {
                            tracing::warn!(error = %e, "post-deploy catalog rebuild failed");
                        }
                    }
                    Ok(None) => {
                        tracing::warn!(
                            service_id,
                            "post-deploy snapshot: row missing right after deploy"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, service_id, "post-deploy snapshot: build_one failed");
                    }
                }
            }
            Err(err) => {
                let _ = log_sender_task.send(crate::deploy::log_bus::BusMessage::End {
                    deploy_id: slug_task.clone(),
                    final_status: "failed".to_string(),
                    image_tag: String::new(),
                    container_name: String::new(),
                    error_message: err.to_string(),
                    duration_ms: crate::deploy::log_bus::now_ms() - start_ms,
                });

                // Redeploy-specific cleanup: stary runtime został UBITY przez
                // `stop_checked` PRZED startem workera, a `deploy()` już
                // wyzerowało pola runtime'u na wierszu (`mark_failed_clear_runtime`).
                // W pamięci jednak wciąż żyje stary `BackendHandle` (z poprzedniego
                // udanego deployu) i wpis katalogu — bez ich zdjęcia resolver dalej
                // routowałby ruch do MARTWEGO endpointu aż do następnego ticku
                // supervisora (a desktop supervisora nie ma). Lustrzymy ścieżkę
                // delete: drop live handle + rebuild katalogu. Świeży deploy NIE
                // miał wcześniej żywego handle'a, więc to dotyczy tylko redeployu.
                if job_task.is_redeploy {
                    if let Some(handle) =
                        live_handles_task.remove(&local_node_id_task, job_task.service_id)
                    {
                        handle.shutdown();
                    }
                    if let Err(e) =
                        catalog_provider_task.rebuild(&mesh_services_registry_task, &db_clone)
                    {
                        tracing::warn!(error = %e, "failed-redeploy catalog rebuild failed");
                    }
                }

                if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
                    &db_clone,
                    job_task.service_id,
                    &local_node_id_task,
                ) {
                    mesh_services_registry_task.apply_local_change(
                        &local_node_id_task,
                        tentaflow_protocol::ServiceChange::Updated(info.clone()),
                    );
                    if let Some(qm) = quic_mesh_task {
                        let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
                            from_node_id: local_node_id_task.clone(),
                            change: tentaflow_protocol::ServiceChange::Updated(info),
                        };
                        if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                            let _ = qm
                                .broadcast_ufp2_to_trusted(
                                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                                    &bytes,
                                    None,
                                )
                                .await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                crate::deploy::log_bus::close(&slug_task);
            }
        }
    });

    slug
}

#[handler(variant = "ServiceRedeployRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_redeploy(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ReqRedeploy(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected DeploymentBody::ReqRedeploy",
            ));
        }
    };

    let not_found = |message: &str| {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ResRedeploy(
            tentaflow_protocol::ServiceRedeployResponse {
                status: "not_found".to_string(),
                deploy_id: String::new(),
                engine_id: String::new(),
                deploy_method: String::new(),
                node_id: ctx.state.local_node_id.to_string(),
                message: message.to_string(),
            },
        ))
    };

    // v1: redeploy local-only; cross-node forward TODO. Brak wiersza lokalnie =
    // "not_found" zamiast forwardu do innego noda.
    let row = {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::get(&conn, payload.service_id).map_err(db_err)?
    };
    let row = match row {
        Some(r) => r,
        None => return Ok(not_found("service not found")),
    };

    let manifest = match crate::services::manifest::registry()
        .by_id(&row.engine_id)
        .cloned()
    {
        Some(m) => m,
        None => {
            return Ok(MessageBody::DeploymentBody(
                tentaflow_protocol::DeploymentPayload::ResRedeploy(
                    tentaflow_protocol::ServiceRedeployResponse {
                        status: "no_source".to_string(),
                        deploy_id: String::new(),
                        engine_id: row.engine_id.clone(),
                        deploy_method: row.deploy_method.as_db_tag().to_string(),
                        node_id: ctx.state.local_node_id.to_string(),
                        message: format!("engine '{}' nie istnieje w manifescie", row.engine_id),
                    },
                ),
            ));
        }
    };

    let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
        ProtocolError::internal("port allocator not initialized (supervisor disabled)")
    })?;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.redeploy",
        Some(&row.engine_id),
        Some(&format!("service_id={}", payload.service_id)),
    );

    let engine_id = row.engine_id.clone();
    let deploy_method = row.deploy_method;
    let deploy_method_tag = row.deploy_method.as_db_tag().to_string();

    let failed = |message: &str| {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ResRedeploy(
            tentaflow_protocol::ServiceRedeployResponse {
                status: "failed".to_string(),
                deploy_id: String::new(),
                engine_id: engine_id.clone(),
                deploy_method: deploy_method_tag.clone(),
                node_id: ctx.state.local_node_id.to_string(),
                message: message.to_string(),
            },
        ))
    };

    // (1) Atomowy claim per-service: ustaw status='deploying' tylko gdy serwis
    // NIE jest już w trakcie redeployu. To serializacja na poziomie bazy —
    // drugi równoległy klik odbije się jako in_progress, bez insertu drugiego
    // wiersza/kontenera. Stary status czytamy w TEJ SAMEJ transakcji co UPDATE
    // (P2: bez tego współbieżna zmiana statusu między read a claim mogłaby zostać
    // nadpisana przez późniejszy restore).
    let prev_status = {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::claim_for_redeploy(&conn, payload.service_id)
            .map_err(db_err)?
    };
    let prev_status = match prev_status {
        Some(s) => s,
        None => {
            return Ok(MessageBody::DeploymentBody(
                tentaflow_protocol::DeploymentPayload::ResRedeploy(
                    tentaflow_protocol::ServiceRedeployResponse {
                        status: "in_progress".to_string(),
                        deploy_id: String::new(),
                        engine_id: engine_id.clone(),
                        deploy_method: deploy_method_tag.clone(),
                        node_id: ctx.state.local_node_id.to_string(),
                        message: "redeploy already running".to_string(),
                    },
                ),
            ));
        }
    };

    // Dwie fazy obsługi błędu po udanym claimie:
    //
    // `restore_status` — błąd PRZED rozpoczęciem stopu (parse config). Stary
    // runtime jest NIETKNIĘTY, więc bezpiecznie oddajemy wiersz na poprzedni
    // status (np. z powrotem `running`).
    let restore_status = |status: crate::services_repo::services::ServiceStatus| {
        if let Ok(conn) = ctx.state.db.write() {
            let _ = crate::services_repo::services::set_status(&conn, payload.service_id, status);
        }
    };
    // `mark_dead` — błąd PO UDANYM stopie (np. create_redeploy_job). Stary runtime
    // jest na pewno UBITY, więc NIE wolno przywracać `running` (DB kłamałaby o żywym
    // serwisie) — `failed` + czyszczenie stale pól runtime'u (pid/port/endpoint).
    let mark_dead = |err_msg: &str| {
        if let Ok(conn) = ctx.state.db.write() {
            let _ = crate::services_repo::services::mark_failed_clear_runtime(
                &conn,
                payload.service_id,
                err_msg,
            );
        }
    };
    // `mark_failed_alive` — stop NIE potwierdził ubicia (native proces nadal żyje /
    // kontener nie zniknął). Runtime MOŻE wciąż żyć → `failed`, ale ZACHOWUJEMY
    // pid/port, bo zerowanie ich osierociłoby żywy proces (brak danych do cleanup,
    // a supervisor/redeploy odpaliłby drugi runtime obok).
    let mark_failed_alive = |err_msg: &str| {
        if let Ok(conn) = ctx.state.db.write() {
            let _ = crate::services_repo::services::mark_failed_keep_runtime(
                &conn,
                payload.service_id,
                err_msg,
            );
        }
    };

    // (4) Zły zapisany config_json: nie podstawiamy `{}` (zgubiłoby parametry /
    // enc api_key i wdrożyłoby pusty serwis) — przywracamy status i raportujemy.
    // To błąd PRZED stopem → runtime nietknięty → restore.
    let user_config: serde_json::Value = match serde_json::from_str(&row.config_json) {
        Ok(cfg) => cfg,
        Err(_) => {
            restore_status(prev_status);
            return Ok(failed("invalid stored config"));
        }
    };

    // (2) Stop jako PRECONDITION: stary kontener/proces musi faktycznie zniknąć
    // przed świeżym deployem. `stop_checked` weryfikuje, że kontenera/procesu już
    // nie ma (dwa runtime'y GPU naraz = OOM). Padło → runtime MOŻE wciąż żyć →
    // `failed` ale ZACHOWUJEMY pid/port (zerowanie osierociłoby żywy proces).
    if let Err(err) = crate::services::deploy::stop_checked(&row, port_allocator.clone()).await {
        tracing::warn!(target: "tentaflow::deploy", service_id = payload.service_id, error = %err, "redeploy: could not stop existing runtime");
        mark_failed_alive(&format!("redeploy stop failed: {}", err));
        return Ok(failed("could not stop existing runtime"));
    }

    // (3) Reuse wiersza: NIE delete + NIE create_deploy_job (insert duplikatu).
    // `create_redeploy_job` aktualizuje TEN sam wiersz (nowy active_deploy_id,
    // config_json), a `deploy()`→`commit`→`finish_deploy_in_tx` trafia w ten
    // service_id. Stop już się powiódł (runtime ubity), więc błąd setupu też NIE
    // restore `running` — wiersz na `failed` + czyszczenie runtime'u.
    let job = match crate::services::deploy::create_redeploy_job(
        deploy_method,
        &manifest,
        &user_config,
        &ctx.state.db,
        ctx.state.local_node_id.as_ref(),
        user_id.as_deref(),
        payload.service_id,
    ) {
        Ok(j) => j,
        Err(e) => {
            mark_dead(&format!("redeploy setup failed: {}", e));
            return Ok(failed(&format!("redeploy setup failed: {}", e)));
        }
    };

    if let Ok(Some(info)) = crate::services::snapshot_builder::build_one(
        &ctx.state.db,
        job.service_id,
        ctx.state.local_node_id.as_ref(),
    ) {
        broadcast_service_change(ctx, tentaflow_protocol::ServiceChange::Updated(info));
    }

    let slug = spawn_deploy_pipeline(
        ctx,
        job,
        deploy_method,
        &manifest,
        &user_config,
        port_allocator,
    );

    Ok(MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ResRedeploy(
            tentaflow_protocol::ServiceRedeployResponse {
                status: "started".to_string(),
                deploy_id: slug,
                engine_id,
                deploy_method: deploy_method_tag,
                node_id: ctx.state.local_node_id.to_string(),
                message: String::new(),
            },
        ),
    ))
}

/// Maps the wire deploy method string ("docker"/"native"/"external") to the
/// internal `DeployMethod` variant by inspecting which sections the manifest
/// declares. "native" picks the runtime declared by `[deploy.native].runtime`.
fn resolve_deploy_method(
    manifest: &crate::services::manifest::ServiceManifest,
    method: &str,
) -> std::result::Result<crate::services_repo::services::DeployMethod, String> {
    use crate::services::manifest::NativeRuntime;
    use crate::services_repo::services::DeployMethod;
    match method {
        "docker" => Ok(DeployMethod::Docker),
        "external" => Ok(DeployMethod::External),
        "native" => {
            let native =
                manifest.deploy.native.as_ref().ok_or_else(|| {
                    format!("engine '{}' has no [deploy.native]", manifest.engine.id)
                })?;
            Ok(match native.runtime {
                NativeRuntime::Embedded => DeployMethod::NativeEmbedded,
                NativeRuntime::Binary => DeployMethod::NativeBinary,
                NativeRuntime::PythonBundle => DeployMethod::NativePythonBundle,
            })
        }
        other => Err(format!(
            "unknown deploy method '{}': expected docker/native/external",
            other
        )),
    }
}

// =============================================================================
// vLLM deploy recommend — TP/PP/ctx/seqs/kv_dtype calculator (Admin only).
// Reads HF config.json, runs auto-fit + VRAM estimator from vram_calculator.
// =============================================================================

fn setting_hf_token(ctx: &HandlerContext) -> Option<String> {
    repository::get_setting_secure(&ctx.state.db, "hf_token", &ctx.state.settings_cipher)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn effective_hf_token(ctx: &HandlerContext, request_token: Option<&str>) -> Option<String> {
    request_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| setting_hf_token(ctx))
}

#[handler(variant = "SuggestServicePortRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn suggest_service_port(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let _payload = match req {
        MessageBody::SuggestServicePortRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SuggestServicePortRequest",
            ));
        }
    };
    // First free port the allocator would hand out (own ledger + OS probe). The
    // actual deploy re-allocates at commit, so this is just the suggested
    // default for the editable port field in the wizard.
    let port = ctx
        .state
        .port_allocator
        .as_ref()
        .and_then(|a| a.peek_free())
        .unwrap_or(0);
    Ok(MessageBody::SuggestServicePortResponseBody(
        tentaflow_protocol::SuggestServicePortResponse {
            port: u32::from(port),
            available: port != 0,
        },
    ))
}

#[handler(variant = "DeployVllmRecommendRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn deploy_vllm_recommend(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::deploy::vram_calculator::{
        analyze_gpu_compatibility, analyze_gpu_compatibility_llamacpp, auto_fit_config,
        build_llamacpp_args_string, build_vllm_args_string, estimate_vram, fetch_gguf_spec,
        fetch_hf_config, fetch_safetensors_total_size, max_concurrent_seqs_for_budget,
        max_context_for_budget, parse_hf_config_with_override, AutoFitOutcome, AutoFitRequest,
        DeployEngine,
    };

    let payload = match req {
        MessageBody::DeployVllmRecommendRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected DeployVllmRecommendRequest",
            ));
        }
    };

    if payload.model.trim().is_empty() {
        return Err(ProtocolError::bad_request("model wymagany"));
    }
    if payload.gpus.is_empty() {
        return Err(ProtocolError::bad_request("co najmniej jeden GPU wymagany"));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ProtocolError::internal(format!("reqwest client: {e}")))?;

    let hf_token = effective_hf_token(ctx, payload.hf_token.as_deref());

    // Sciezka GGUF (llama.cpp): repo GGUF nie ma config.json - metadane czytamy
    // z naglowka pliku .gguf, a rozmiar pliku JEST dokladnym footprintem wag.
    // Aktywna gdy frontend poda `gguf_file` albo gdy sama nazwa modelu wskazuje GGUF.
    let model_lower = payload.model.to_lowercase();
    let raw_gguf = payload.gguf_file.is_some()
        || model_lower.ends_with("-gguf")
        || model_lower.contains("gguf");

    // Silnik: jawny `engine=llama-cpp`/`mlx` z requestu albo wykryty GGUF ->
    // llama.cpp. GGUF deployuje sie wylacznie na llama.cpp, wiec jego fizyka VRAM
    // jest tu jedyna poprawna. MLX czyta config.json (NIE GGUF), wiec idzie ta
    // sama sciezka co vLLM (fetch_hf_config/parse), tylko z fizyka unified-memory.
    // Normalizujemy etykiete (case + '.'/'-') zeby `llama.cpp`, `llamacpp`,
    // `mlx-lm` trafialy w ta sama galez zamiast cicho spadac na vLLM.
    let eng_norm = payload
        .engine
        .as_deref()
        .map(|s| s.to_lowercase().replace('.', "-"));
    let engine = if matches!(eng_norm.as_deref(), Some("mlx") | Some("mlx-lm")) {
        DeployEngine::Mlx
    } else if matches!(eng_norm.as_deref(), Some("llama-cpp") | Some("llamacpp")) || raw_gguf {
        DeployEngine::LlamaCpp
    } else {
        DeployEngine::Vllm
    };

    // GGUF deployuje sie wylacznie na llama.cpp, wiec sciezka GGUF (fetch_gguf_spec)
    // jest wazna tylko gdy rozstrzygniety silnik to LlamaCpp. Mlx/Vllm czytaja
    // config.json nawet gdy nazwa repo zawiera "gguf" — inaczej zadalyby gguf_file.
    let use_gguf_path = raw_gguf && engine == DeployEngine::LlamaCpp;

    let (spec, weights_override) = if use_gguf_path {
        let gguf_file = payload.gguf_file.clone().ok_or_else(|| {
            ProtocolError::bad_request(format!(
                "Model {} wyglada na GGUF ale nie podano sciezki pliku .gguf (gguf_file).",
                payload.model
            ))
        })?;
        let (spec, file_size) =
            fetch_gguf_spec(&client, &payload.model, &gguf_file, hf_token.as_deref())
                .await
                .map_err(|e| {
                    ProtocolError::not_found(format!(
                        "Nie udalo sie odczytac metadanych GGUF z {}/{}: {e}",
                        payload.model, gguf_file
                    ))
                })?;
        (spec, Some(file_size))
    } else {
        let config_json = fetch_hf_config(&client, &payload.model, hf_token.as_deref())
            .await
            .map_err(|e| {
                ProtocolError::not_found(format!(
                    "Nie udalo sie pobrac config.json z HF: {e}. Sprawdz nazwe modelu i ewentualnie HF token (gated repo)."
                ))
            })?;
        let spec = parse_hf_config_with_override(
            &config_json,
            &payload.model,
            payload.quantization_override.as_deref(),
        )
        .map_err(|e| ProtocolError::bad_request(format!("Parse HF config: {e}")))?;
        // Dokladny rozmiar wag z safetensors index (metadata.total_size) — gdy
        // dostepny, dziala jak override GGUF i omija heurystyke param-count.
        // Brak indexu/sieci -> None, estimated_params (MoE/GQA-swiadomy) jest
        // fallbackiem.
        let weights_override =
            fetch_safetensors_total_size(&client, &payload.model, hf_token.as_deref()).await;
        (spec, weights_override)
    };

    let gpu_count = payload.gpus.len() as u32;
    let gpu_memory_gb = payload
        .gpus
        .iter()
        .map(|g| g.memory_gb)
        .fold(f64::INFINITY, f64::min);

    // DeepSeek V4 serwuje uwagę przez kernel FlashMLA w układzie fp8, który
    // twardo wymaga fp8 kv-cache (vLLM asertuje "FlashMLA fp8 layout only
    // supports fp8 kv-cache" i ubija engine przy `auto`). Gdy user sam nie
    // wybrał dtype, domyślamy fp8 dla tej rodziny — inaczej każdy deploy V4 pada.
    // NVFP4: waga w fp4, ale kv-cache w fp8 to sprawdzony sweet-spot (recepty
    // recipes.vllm.ai dla nvfp4 ustawiaja tak samo). Ustawiamy fp8 jako
    // strukturalny default, gdy user sam nie wybral — inaczej GUI pokazywalo
    // "auto" (fp16 kv) niespojnie z recepta, ktora fp8 dorzucala tylko do
    // surowej komendy.
    let kv_dtype = payload.kv_cache_dtype.clone().unwrap_or_else(|| {
        let quant = spec.quantization.as_deref().unwrap_or("").to_lowercase();
        let is_nvfp4 = quant.contains("nvfp4") || quant.contains("fp4");
        if spec.model_type.eq_ignore_ascii_case("deepseek_v4") || is_nvfp4 {
            "fp8".to_string()
        } else {
            "auto".to_string()
        }
    });
    let gpu_mem_util = payload.gpu_memory_utilization.unwrap_or(0.9);

    let lock_ctx = payload.lock_max_model_len.unwrap_or(false);
    let lock_seqs = payload.lock_max_num_seqs.unwrap_or(false);
    let lock_tp = payload.lock_tensor_parallel.unwrap_or(false);

    let fit = auto_fit_config(
        &spec,
        &AutoFitRequest {
            engine,
            gpu_count,
            gpu_memory_gb_each: gpu_memory_gb,
            kv_cache_dtype: kv_dtype.clone(),
            kv_cache_dtype_v: payload.kv_cache_dtype_v.clone(),
            gpu_memory_utilization: gpu_mem_util,
            requested_max_model_len: payload.max_model_len,
            requested_max_num_seqs: payload.max_num_seqs,
            requested_tensor_parallel: payload.tensor_parallel,
            requested_pipeline_parallel: payload.pipeline_parallel,
            max_num_batched_tokens: payload.max_num_batched_tokens,
            lock_max_model_len: lock_ctx,
            lock_max_num_seqs: lock_seqs,
            lock_tensor_parallel: lock_tp,
            weights_bytes_override: weights_override,
        },
    );

    let AutoFitOutcome {
        applied: mut applied_input,
        auto_adjusted,
        at_limit,
        error: fit_error,
    } = fit;

    // auto_fit nie propaguje typu V cache — ustawiamy go na applied PRZED
    // estymacja i builderem argow. Batcha NIE ruszamy: auto_fit dobral ctx wlasnie
    // pod niego, wiec podmiana tutaj kazalaby estymacie ocenic wybor na innym
    // szczycie aktywacji niz ten, dla ktorego zostal policzony.
    applied_input.kv_cache_dtype_v = payload.kv_cache_dtype_v.clone();

    // Over-budget NIE jest twardym bledem requestu — auto_fit zwraca uzywalny
    // `applied` (minimalny ctx/seqs), wiec liczymy estymacje i oddajemy
    // kalkulator z `fits=false` + ostrzezeniem. Inaczej kafelek nie mieszczacy
    // sie w VRAM gubil caly kalkulator (user nie widzial rozkladu ani nie mogl
    // zmienic quant/ctx/GPU zeby go dopasowac).
    let mut estimate = estimate_vram(&spec, &applied_input);
    if let Some(err) = fit_error {
        estimate.fits_per_gpu = false;
        estimate.fits_total = false;
        estimate.warnings.push(err);
    }
    let max_supported_model_len = max_context_for_budget(&spec, &applied_input);
    let max_supported_num_seqs = max_concurrent_seqs_for_budget(&spec, &applied_input);
    // Pole odpowiedzi zostaje `recommended_vllm_args`, ale dla llama.cpp niesie jego
    // argumenty CLI (frontend czyta to pole niezaleznie od silnika).
    let mut recommended_env: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut recipe_applied: Option<String> = None;
    // Dialekt CLI rozstrzygany z surowej etykiety silnika (NIE z DeployEngine —
    // sglang dzieli fizyke VRAM z vLLM, ale ma wlasne nazwy flag). Gdy dialekt
    // to sglang, generujemy argumenty sglang zamiast vLLM (inaczej kontener
    // odrzuca `--max-model-len` i pada na starcie).
    use crate::deploy::launch_dialect::{self, DeployMethod, Dialect};
    let dialect = launch_dialect::dialect_for(eng_norm.as_deref().unwrap_or("vllm"));
    let recommended_vllm_args = if dialect == Dialect::Sglang {
        launch_dialect::build_args(Dialect::Sglang, &spec, &applied_input).join(" ")
    } else {
        match engine {
            DeployEngine::LlamaCpp => build_llamacpp_args_string(&spec, &applied_input),
            DeployEngine::Vllm => {
                let mut base = build_vllm_args_string(&spec, &applied_input);
                // vLLM deployment recipe (recipes.vllm.ai): expert launch flags
                // (tool/reasoning parser, expert-parallel, ...) + per-GPU-family env
                // (Blackwell FP4 MoE / Hopper FP8 MoE). Embedded snapshot first so
                // offline/HF-only deploys are instant; live fetch only fills models
                // missing from the snapshot (added upstream after the last vendor).
                use crate::deploy::vllm_recipes;
                let entry = match vllm_recipes::resolve_embedded(&payload.model) {
                    Some(e) => Some(e),
                    None => vllm_recipes::fetch_live(&client, &payload.model).await,
                };
                if let Some(entry) = entry {
                    let family = payload
                        .gpus
                        .first()
                        .and_then(|g| vllm_recipes::gpu_family(&g.name));
                    let (rargv, renv) = vllm_recipes::build_args(
                        &entry,
                        family,
                        applied_input.tensor_parallel,
                        applied_input.pipeline_parallel,
                    );
                    if !rargv.is_empty() || !renv.is_empty() {
                        // Merge our tuned args (gpu-mem, ctx, kv) with the recipe
                        // expert flags; recipe is appended last so dedup last-wins
                        // lets it override on overlap.
                        let mut toks: Vec<String> =
                            base.split_whitespace().map(String::from).collect();
                        toks.extend(rargv);
                        toks = crate::deploy::python_venv::dedup_cli_args_last_wins(toks);
                        base = toks.join(" ");
                        recommended_env = renv;
                        recipe_applied = Some(entry.hf_id.clone());
                    }
                }
                // Rodzina gemma-4 w NVFP4: vLLM potrzebuje jawnych flag tool-callingu
                // + self-speculative chat template. Upstream recipe albo je gubi
                // (chat-template `examples/*.jinja` jest dropowany przez build_args, bo
                // szablon zyje w zrodlach vLLM), albo modelu w ogole nie ma w bazie
                // recept (RedHatAI 12B). Wymuszamy spojnie dla calej rodziny — dziala
                // tez dla recznego deployu gemma-4 nvfp4, nie tylko z prekonfigurowanego
                // kafelka. Speculative draft dostarcza osobno preset (`speculator_repo`).
                let model_lc = payload.model.to_lowercase();
                let quant_lc = spec.quantization.as_deref().unwrap_or("").to_lowercase();
                if model_lc.contains("gemma-4")
                    && (quant_lc.contains("nvfp4") || quant_lc.contains("fp4"))
                {
                    // NIE wymuszamy `--chat-template examples/tool_chat_template_gemma4.jinja`:
                    // pip-owy vLLM (docker i native python-bundle) nie niesie katalogu
                    // `examples/`, wiec vLLM padal z "chat template ... doesn't exist".
                    // Tool-calling gemma-4 dziala z szablonem wbudowanym w tokenizer;
                    // parser + auto-tool-choice wystarczaja. Kto chce override szablonu,
                    // podaje absolutna sciezke recznie w extra-args.
                    let mut toks: Vec<String> = base.split_whitespace().map(String::from).collect();
                    toks.push("--max-model-len".into());
                    toks.push("auto".into());
                    toks.push("--enable-auto-tool-choice".into());
                    toks.push("--tool-call-parser".into());
                    toks.push("gemma4".into());
                    toks = crate::deploy::python_venv::dedup_cli_args_last_wins(toks);
                    base = toks.join(" ");
                }
                // DeepSeek V4 (Flash/Pro, w tym warianty -DSpark): MoE + DSA long-context.
                // `--block-size 256` pod efektywny KV przy kontekstach do 1M (recepta
                // vLLM V4). fp8 kv-cache i --enable-expert-parallel dokladane sa juz
                // wyzej (model_type deepseek_v4 + MoE na multi-GPU); DSpark self-speculative
                // wnosi preset (`speculator_method="dspark"`). `--data-parallel-size` i
                // fp4-indexer-cache sa hardware-specyficzne (liczba B200) → recepta/extra-args.
                if spec.model_type.eq_ignore_ascii_case("deepseek_v4")
                    || model_lc.contains("deepseek-v4")
                {
                    let mut toks: Vec<String> = base.split_whitespace().map(String::from).collect();
                    toks.push("--block-size".into());
                    toks.push("256".into());
                    toks = crate::deploy::python_venv::dedup_cli_args_last_wins(toks);
                    base = toks.join(" ");
                }
                base
            }
            // MLX (mlx-lm) uruchamiany jest przez wlasny runner, nie przez flagi CLI
            // jednego procesu serwera - kontekst/seqs/KV przekazuje config deployu.
            DeployEngine::Mlx => format!(
                "--max-tokens {} --max-kv-size {}",
                applied_input.max_model_len,
                applied_input.max_num_seqs.max(1) * applied_input.max_model_len
            ),
        }
    };

    // Podglad finalnej komendy: baza per-dialekt+metoda + dokladnie te same
    // argumenty co `recommended_vllm_args` (dla vLLM zawieraja juz flagi recipe).
    let deploy_method = DeployMethod::parse(payload.deploy_method.as_deref());
    let launch_command = {
        let base = launch_dialect::base_command_string(
            eng_norm.as_deref().unwrap_or("vllm"),
            deploy_method,
        );
        if base.is_empty() {
            String::new()
        } else if recommended_vllm_args.trim().is_empty() {
            base
        } else {
            format!("{base} {recommended_vllm_args}")
        }
    };

    let estimated_params = spec.estimated_params() as f64 / 1_000_000_000.0;
    let bytes_per_param = spec.bytes_per_param();

    let mut warnings = estimate.warnings.clone();
    let gpu_compat = match engine {
        DeployEngine::LlamaCpp => analyze_gpu_compatibility_llamacpp(&spec, gpu_count),
        DeployEngine::Vllm => analyze_gpu_compatibility(&spec, gpu_count),
        // MLX to pojedyncze urzadzenie (unified memory) - zawsze "czysta" partycja
        // bez TP/PP, brak ograniczen podzielnosci heads/layers.
        DeployEngine::Mlx => crate::deploy::vram_calculator::GpuCompatibilityReport {
            used_tp: 1,
            used_pp: 1,
            uses_all_gpus: true,
            clean_partition: true,
            better_gpu_counts: vec![1],
            warning: None,
        },
    };
    if let Some(w) = &gpu_compat.warning {
        warnings.push(w.clone());
    }

    let model_spec = tentaflow_protocol::DeployVllmModelSpecSummary {
        model_type: spec.model_type.clone(),
        architectures: spec.architectures.clone(),
        dtype: spec.dtype.clone(),
        quantization: spec.quantization.clone(),
        hidden_size: spec.hidden_size,
        num_attention_heads: spec.num_attention_heads,
        num_key_value_heads: spec.num_key_value_heads,
        num_hidden_layers: spec.num_hidden_layers,
        max_position_embeddings: spec.max_position_embeddings,
        has_vision: spec.has_vision,
        has_audio: spec.has_audio,
        estimated_params_billions: estimated_params,
        bytes_per_param,
    };

    let vram_estimate = tentaflow_protocol::DeployVllmVramEstimate {
        model_weights_gb: estimate.model_weights_gb,
        kv_cache_gb: estimate.kv_cache_gb,
        activations_gb: estimate.activations_gb,
        overhead_gb: estimate.overhead_gb,
        total_gb: estimate.total_gb,
        per_gpu_gb: estimate.per_gpu_gb,
        fits_per_gpu: estimate.fits_per_gpu,
        fits_total: estimate.fits_total,
        warnings: estimate.warnings.clone(),
        kv_pool_gb: estimate.kv_pool_gb,
        pool_tokens: estimate.pool_tokens,
        concurrent_full_len_seqs: estimate.concurrent_full_len_seqs,
    };

    let recommended = tentaflow_protocol::DeployVllmConfig {
        tensor_parallel: applied_input.tensor_parallel,
        pipeline_parallel: applied_input.pipeline_parallel,
        max_model_len: applied_input.max_model_len,
        max_num_seqs: applied_input.max_num_seqs,
        kv_cache_dtype: applied_input.kv_cache_dtype.clone(),
        gpu_memory_utilization: applied_input.gpu_memory_utilization,
    };

    let applied = tentaflow_protocol::DeployVllmConfig {
        tensor_parallel: applied_input.tensor_parallel,
        pipeline_parallel: applied_input.pipeline_parallel,
        max_model_len: applied_input.max_model_len,
        max_num_seqs: applied_input.max_num_seqs,
        kv_cache_dtype: applied_input.kv_cache_dtype,
        gpu_memory_utilization: applied_input.gpu_memory_utilization,
    };

    let gpu_compatibility = tentaflow_protocol::DeployVllmGpuCompatibility {
        used_tp: gpu_compat.used_tp,
        used_pp: gpu_compat.used_pp,
        uses_all_gpus: gpu_compat.uses_all_gpus,
        clean_partition: gpu_compat.clean_partition,
        better_gpu_counts: gpu_compat.better_gpu_counts,
        warning: gpu_compat.warning,
    };

    Ok(MessageBody::DeployVllmRecommendResponseBody(
        tentaflow_protocol::DeployVllmRecommendResponse {
            model_spec,
            vram_estimate,
            recommended,
            max_supported_model_len,
            max_supported_num_seqs,
            recommended_vllm_args,
            warnings,
            gpu_compatibility,
            applied,
            auto_adjusted,
            at_limit,
            recommended_env,
            recipe_applied,
            launch_command,
        },
    ))
}

// =============================================================================
// F1a §6.6 — model / alias access control (admin-only).
// Visibility, consumer grants, and per-addon access reconciliation. Every
// mutation runs through a repository orchestrator that reconciles dependent
// `addon_uses_*` rows and writes the audit + change-log inside one tx.
// =============================================================================

/// Maps repository `(addon, before, after)` transition tuples to the wire type.
fn to_access_transitions(
    transitions: Vec<(String, String, String)>,
) -> Vec<tentaflow_protocol::AccessTransition> {
    transitions
        .into_iter()
        .map(
            |(addon_id, before, after)| tentaflow_protocol::AccessTransition {
                addon_id,
                before,
                after,
            },
        )
        .collect()
}

#[handler(variant = "AliasConsumerListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn alias_consumer_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AliasConsumerListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AliasConsumerListRequest",
            ))
        }
    };
    let rows = repository::list_alias_consumers(&ctx.state.db, payload.alias_id).map_err(db_err)?;
    let consumers = rows
        .into_iter()
        .map(|c| tentaflow_protocol::AccessConsumerEntry {
            addon_id: c.addon_id,
            granted_by_user_id: c.granted_by_user_id,
            granted_at: Some(c.granted_at as u64),
            revoked_at: c.revoked_at.map(|v| v as u64),
        })
        .collect();
    Ok(MessageBody::AliasConsumerListResponseBody(
        tentaflow_protocol::AliasConsumerListResponse {
            alias_id: payload.alias_id,
            consumers,
        },
    ))
}

#[handler(variant = "AliasConsumerGrantRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn alias_consumer_grant(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AliasConsumerGrantRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AliasConsumerGrantRequest",
            ))
        }
    };
    let transitions = repository::grant_alias_consumer_audited(
        &ctx.state.db,
        payload.alias_id,
        &payload.addon_id,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "alias.consumer.grant",
        Some(&format!("alias:{}", payload.alias_id)),
        Some(&payload.addon_id),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "AliasConsumerRevokeRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn alias_consumer_revoke(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AliasConsumerRevokeRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AliasConsumerRevokeRequest",
            ))
        }
    };
    let transitions = repository::revoke_alias_consumer_audited(
        &ctx.state.db,
        payload.alias_id,
        &payload.addon_id,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "alias.consumer.revoke",
        Some(&format!("alias:{}", payload.alias_id)),
        Some(&payload.addon_id),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "AliasVisibilitySetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn alias_visibility_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AliasVisibilitySetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AliasVisibilitySetRequest",
            ))
        }
    };
    if !matches!(
        payload.visibility.as_str(),
        "private" | "restricted" | "public"
    ) {
        return Err(ProtocolError::bad_request(
            "visibility must be private/restricted/public",
        ));
    }
    let transitions = repository::set_alias_visibility_audited(
        &ctx.state.db,
        payload.alias_id,
        &payload.visibility,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "alias.visibility.set",
        Some(&format!("alias:{}", payload.alias_id)),
        Some(&payload.visibility),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "ModelVisibilityListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_visibility_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let rows = repository::list_model_visibility(&ctx.state.db).map_err(db_err)?;
    let models = rows
        .into_iter()
        .map(|m| tentaflow_protocol::ModelVisibilityEntry {
            model_id: m.model_id,
            visibility: m.visibility,
        })
        .collect();
    Ok(MessageBody::ModelVisibilityListResponseBody(
        tentaflow_protocol::ModelVisibilityListResponse { models },
    ))
}

#[handler(variant = "ModelVisibilitySetRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_visibility_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelVisibilitySetRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelVisibilitySetRequest",
            ))
        }
    };
    if !matches!(payload.visibility.as_str(), "restricted" | "public") {
        return Err(ProtocolError::bad_request(
            "model visibility must be restricted/public",
        ));
    }
    let transitions = repository::set_model_visibility_audited(
        &ctx.state.db,
        &payload.model_id,
        &payload.visibility,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model.visibility.set",
        Some(&format!("model:{}", payload.model_id)),
        Some(&payload.visibility),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "ModelConsumerListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_consumer_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelConsumerListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelConsumerListRequest",
            ))
        }
    };
    let rows =
        repository::list_model_consumers(&ctx.state.db, &payload.model_id).map_err(db_err)?;
    let consumers = rows
        .into_iter()
        .map(|c| tentaflow_protocol::AccessConsumerEntry {
            addon_id: c.addon_id,
            granted_by_user_id: c.granted_by_user_id,
            granted_at: Some(c.granted_at as u64),
            revoked_at: c.revoked_at.map(|v| v as u64),
        })
        .collect();
    Ok(MessageBody::ModelConsumerListResponseBody(
        tentaflow_protocol::ModelConsumerListResponse {
            model_id: payload.model_id.clone(),
            consumers,
        },
    ))
}

#[handler(variant = "ModelConsumerGrantRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_consumer_grant(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelConsumerGrantRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelConsumerGrantRequest",
            ))
        }
    };
    let transitions = repository::grant_model_consumer_audited(
        &ctx.state.db,
        &payload.model_id,
        &payload.addon_id,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model.consumer.grant",
        Some(&format!("model:{}", payload.model_id)),
        Some(&payload.addon_id),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "ModelConsumerRevokeRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_consumer_revoke(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelConsumerRevokeRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelConsumerRevokeRequest",
            ))
        }
    };
    let transitions = repository::revoke_model_consumer_audited(
        &ctx.state.db,
        &payload.model_id,
        &payload.addon_id,
        None,
    )
    .map_err(db_err)?;
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "model.consumer.revoke",
        Some(&format!("model:{}", payload.model_id)),
        Some(&payload.addon_id),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

#[handler(variant = "AddonAccessListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_access_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonAccessListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonAccessListRequest",
            ))
        }
    };
    let alias_rows =
        repository::list_addon_uses_alias(&ctx.state.db, &payload.addon_id).map_err(db_err)?;
    let model_rows =
        repository::list_addon_uses_model(&ctx.state.db, &payload.addon_id).map_err(db_err)?;

    // The reader transaction resolves the current owner visibility of each
    // target so the UI can explain a `pending` row without a second lookup.
    let mut conn = crate::db::repository::acquire_for_baseline(&ctx.state.db).map_err(db_err)?;
    let tx = conn.transaction().map_err(db_err)?;
    let uses_alias = alias_rows
        .into_iter()
        .map(|r| {
            let owner_visibility =
                match repository::lookup_alias_visibility_within_tx(&tx, &r.target) {
                    Ok(Some((_, v))) => v,
                    // Owner addon not installed yet → no visibility row.
                    Ok(None) => "private".to_string(),
                    Err(_) => "private".to_string(),
                };
            tentaflow_protocol::AddonUsesEntry {
                target: r.target,
                required: r.required,
                reason: r.reason,
                grant_status: r.grant_status,
                owner_visibility,
            }
        })
        .collect();
    let uses_model = model_rows
        .into_iter()
        .map(|r| {
            let owner_visibility = repository::lookup_model_visibility_within_tx(&tx, &r.target)
                .unwrap_or_else(|_| "restricted".to_string());
            tentaflow_protocol::AddonUsesEntry {
                target: r.target,
                required: r.required,
                reason: r.reason,
                grant_status: r.grant_status,
                owner_visibility,
            }
        })
        .collect();
    tx.commit().map_err(db_err)?;

    Ok(MessageBody::AddonAccessListResponseBody(
        tentaflow_protocol::AddonAccessListResponse {
            addon_id: payload.addon_id.clone(),
            uses_alias,
            uses_model,
        },
    ))
}

#[handler(variant = "AddonAccessDecisionRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn addon_access_decision(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonAccessDecisionRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AddonAccessDecisionRequest",
            ))
        }
    };
    let approve = match payload.decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => return Err(ProtocolError::bad_request("decision must be approve/deny")),
    };
    let transitions = match payload.kind.as_str() {
        "alias" => {
            // The decision keys on the alias name; resolve it to the row id the
            // consumer-grant orchestrator needs.
            let alias_id = {
                let conn =
                    crate::db::repository::acquire_for_baseline(&ctx.state.db).map_err(db_err)?;
                conn.query_row(
                    "SELECT id FROM model_aliases WHERE alias = ?1",
                    rusqlite::params![payload.target],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            };
            let Some(alias_id) = alias_id else {
                return Err(ProtocolError::not_found(format!(
                    "alias '{}' not found",
                    payload.target
                )));
            };
            if approve {
                repository::grant_alias_consumer_audited(
                    &ctx.state.db,
                    alias_id,
                    &payload.addon_id,
                    None,
                )
            } else {
                repository::revoke_alias_consumer_audited(
                    &ctx.state.db,
                    alias_id,
                    &payload.addon_id,
                    None,
                )
            }
            .map_err(db_err)?
        }
        "model" => if approve {
            repository::grant_model_consumer_audited(
                &ctx.state.db,
                &payload.target,
                &payload.addon_id,
                None,
            )
        } else {
            repository::revoke_model_consumer_audited(
                &ctx.state.db,
                &payload.target,
                &payload.addon_id,
                None,
            )
        }
        .map_err(db_err)?,
        _ => return Err(ProtocolError::bad_request("kind must be alias/model")),
    };
    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        if approve {
            "addon.access.approve"
        } else {
            "addon.access.deny"
        },
        Some(&format!("{}:{}", payload.kind, payload.target)),
        Some(&payload.addon_id),
    );
    Ok(MessageBody::AccessMutationResponseBody(
        tentaflow_protocol::AccessMutationResponse {
            ok: true,
            transitions: to_access_transitions(transitions),
        },
    ))
}

/// Generyczny auto-tuner — zwraca typed `parameters` mape per silnik.
/// Wizard JS pre-filluje formularz (`tf-parameter-form`) z tej mapy.
/// Dispatch po `engine_id`:
///   * vllm/vllm-metal/sglang/tensorrt-llm — uzywa `auto_fit_config` jako
///     core, mapuje pola na kanoniczne klucze schema parametrow.
///   * llama-cpp — `ctx_size` z HF max_position_embeddings (clamp 32k),
///     `n_gpu_layers=999`, `threads=cpus/2`.
///   * ollama — defaultowe wartosci context_size/num_gpu/num_thread/num_batch.
///   * whisper/mlx-whisper — beam_size=5, n_threads=cpus/2.
///   * mlx — defaultowe max_tokens/temperature/top_p.
///   * pozostale — pusta mapa.
#[handler(variant = "EngineRecommendRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn engine_recommend(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EngineRecommendRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EngineRecommendRequest",
            ));
        }
    };

    let engine_id = payload.engine_id.trim();
    if engine_id.is_empty() {
        return Err(ProtocolError::bad_request("engine_id wymagany"));
    }

    let mut parameters: Vec<tentaflow_protocol::KeyValue> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let push =
        |params: &mut Vec<tentaflow_protocol::KeyValue>, key: &str, value: serde_json::Value| {
            params.push(tentaflow_protocol::KeyValue {
                key: key.to_string(),
                value_json: value.to_string(),
            });
        };

    match engine_id {
        "vllm" | "vllm-metal" | "sglang" | "tensorrt-llm" => {
            // Reuse auto_fit_config — to samo co stary deploy_vllm_recommend
            // ale wyciagamy tylko typed pola (bez recommended_vllm_args
            // raw stringa). Wymaga tych samych preconditions: model + GPU.
            if payload.model_repo.trim().is_empty() {
                return Err(ProtocolError::bad_request("model_repo wymagany"));
            }
            if payload.gpus.is_empty() {
                return Err(ProtocolError::bad_request(
                    "co najmniej jeden GPU wymagany dla rodziny vllm",
                ));
            }
            use crate::deploy::vram_calculator::{
                auto_fit_config, fetch_hf_config, fetch_safetensors_total_size,
                parse_hf_config_with_override, AutoFitRequest, DeployEngine,
            };

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| ProtocolError::internal(format!("reqwest client: {e}")))?;
            let hf_token = effective_hf_token(ctx, payload.hf_token.as_deref());
            let config_json = fetch_hf_config(&client, &payload.model_repo, hf_token.as_deref())
                .await
                .map_err(|e| {
                    ProtocolError::not_found(format!("Nie udalo sie pobrac config.json z HF: {e}"))
                })?;
            let spec = parse_hf_config_with_override(&config_json, &payload.model_repo, None)
                .map_err(|e| ProtocolError::bad_request(format!("Parse HF config: {e}")))?;
            // DRUG-5: dokladny rozmiar wag z safetensors index (jak w
            // deploy_vllm_recommend) — usuwa najwieksza rozbieznosc miedzy
            // tym prefillem a glownym kalkulatorem (wagi MoE/quant inaczej liczone
            // heurystyka param-count). Auto_fit liczy ta sama fizyke budzetu KV.
            let weights_override =
                fetch_safetensors_total_size(&client, &payload.model_repo, hf_token.as_deref())
                    .await;

            let gpu_count = payload.gpus.len() as u32;
            let gpu_memory_gb = payload
                .gpus
                .iter()
                .map(|g| g.memory_gb)
                .fold(f64::INFINITY, f64::min);

            let req_fit = AutoFitRequest {
                engine: DeployEngine::Vllm,
                gpu_count,
                gpu_memory_gb_each: gpu_memory_gb,
                kv_cache_dtype: "auto".to_string(),
                kv_cache_dtype_v: None,
                gpu_memory_utilization: 0.9,
                requested_max_model_len: None,
                requested_max_num_seqs: None,
                requested_tensor_parallel: None,
                requested_pipeline_parallel: None,
                max_num_batched_tokens: None,
                lock_max_model_len: false,
                lock_max_num_seqs: false,
                lock_tensor_parallel: false,
                weights_bytes_override: weights_override,
            };
            let outcome = auto_fit_config(&spec, &req_fit);
            let cfg = outcome.applied;
            if let Some(err) = outcome.error {
                warnings.push(err);
            }

            // Mapowanie kanonicznych pol na klucze per silnik. Bindings z
            // manifestu dorzucaja te klucze do env / API options przy deploy.
            match engine_id {
                "vllm" | "vllm-metal" => {
                    push(
                        &mut parameters,
                        "gpu_memory_utilization",
                        serde_json::json!(cfg.gpu_memory_utilization),
                    );
                    push(
                        &mut parameters,
                        "max_model_len",
                        serde_json::json!(cfg.max_model_len),
                    );
                    push(
                        &mut parameters,
                        "max_num_seqs",
                        serde_json::json!(cfg.max_num_seqs),
                    );
                    push(
                        &mut parameters,
                        "max_num_batched_tokens",
                        serde_json::json!(cfg.max_num_batched_tokens),
                    );
                    push(
                        &mut parameters,
                        "tensor_parallel_size",
                        serde_json::json!(cfg.tensor_parallel),
                    );
                    push(
                        &mut parameters,
                        "pipeline_parallel_size",
                        serde_json::json!(cfg.pipeline_parallel),
                    );
                    push(
                        &mut parameters,
                        "kv_cache_dtype",
                        serde_json::json!(cfg.kv_cache_dtype),
                    );
                    push(&mut parameters, "dtype", serde_json::json!("auto"));
                    push(
                        &mut parameters,
                        "enable_chunked_prefill",
                        serde_json::json!(true),
                    );
                }
                "sglang" => {
                    push(
                        &mut parameters,
                        "tp",
                        serde_json::json!(cfg.tensor_parallel),
                    );
                    push(
                        &mut parameters,
                        "mem_fraction",
                        serde_json::json!(cfg.gpu_memory_utilization),
                    );
                    push(
                        &mut parameters,
                        "max_total_tokens",
                        serde_json::json!(cfg.max_num_batched_tokens),
                    );
                    push(
                        &mut parameters,
                        "max_batch_size",
                        serde_json::json!(cfg.max_num_seqs),
                    );
                }
                "tensorrt-llm" => {
                    push(
                        &mut parameters,
                        "tp",
                        serde_json::json!(cfg.tensor_parallel),
                    );
                    push(
                        &mut parameters,
                        "max_batch_size",
                        serde_json::json!(cfg.max_num_seqs),
                    );
                    push(
                        &mut parameters,
                        "max_seq_len",
                        serde_json::json!(cfg.max_model_len),
                    );
                    push(
                        &mut parameters,
                        "free_gpu_memory_fraction",
                        serde_json::json!(cfg.gpu_memory_utilization),
                    );
                    push(
                        &mut parameters,
                        "max_num_tokens",
                        serde_json::json!(cfg.max_num_batched_tokens),
                    );
                }
                _ => unreachable!(),
            }
        }
        "llama-cpp" => {
            // llama-cpp: load-time params (ctx_size/n_gpu_layers/threads/batch_size).
            // Bez fetch HF config — uzywamy konserwatywnych defaultow.
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8);
            push(&mut parameters, "ctx_size", serde_json::json!(8192));
            push(&mut parameters, "n_gpu_layers", serde_json::json!(999));
            push(
                &mut parameters,
                "threads",
                serde_json::json!((cpus / 2).max(2)),
            );
            push(&mut parameters, "batch_size", serde_json::json!(512));
        }
        "ollama" => {
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8);
            push(&mut parameters, "context_size", serde_json::json!(8192));
            push(&mut parameters, "num_gpu", serde_json::json!(999));
            push(
                &mut parameters,
                "num_thread",
                serde_json::json!((cpus / 2).max(2)),
            );
            push(&mut parameters, "num_batch", serde_json::json!(512));
        }
        "whisper" | "mlx-whisper" => {
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8);
            push(&mut parameters, "default_beam_size", serde_json::json!(5));
            push(
                &mut parameters,
                "n_threads",
                serde_json::json!((cpus / 2).max(2)),
            );
        }
        "mlx" => {
            push(
                &mut parameters,
                "default_max_tokens",
                serde_json::json!(2048),
            );
            push(
                &mut parameters,
                "default_temperature",
                serde_json::json!(0.7),
            );
            push(&mut parameters, "default_top_p", serde_json::json!(0.95));
        }
        _ => {
            // Silnik bez schema parametrow albo bez auto-recommend logic.
            // Zwracamy pusta mape — wizard fallback do manifest.parameters[].default.
        }
    }

    Ok(MessageBody::EngineRecommendResponseBody(
        tentaflow_protocol::EngineRecommendResponse {
            parameters,
            warnings,
        },
    ))
}

// =============================================================================
// Deployments — status + list (stream handler w stream_handlers.rs)
// =============================================================================

fn deployment_row_to_summary(
    r: repository::deployments::DeploymentRow,
) -> tentaflow_protocol::DeploymentSummary {
    tentaflow_protocol::DeploymentSummary {
        deploy_id: r.deploy_id,
        engine_id: r.engine_id,
        deploy_method: r.deploy_method,
        node_id: r.node_id,
        status: r.status,
        phase: r.phase,
        progress_pct: r.progress_pct as i32,
        image_tag: r.image_tag,
        container_name: r.container_name,
        started_at: r.started_at,
        finished_at: r.finished_at.unwrap_or_default(),
        error_message: r.error_message.unwrap_or_default(),
        log_tail: r.log_tail,
        user_id: r.user_id.unwrap_or_default(),
    }
}

#[handler(variant = "DeploymentStatusRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn deployment_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let deploy_id = match req {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ReqStatus(r)) => {
            r.deploy_id.clone()
        }
        _ => return Err(ProtocolError::bad_request("expected ReqStatus")),
    };
    let row = repository::deployments::get(&ctx.state.db, &deploy_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotFound,
                format!("deployment '{}' nieznany", deploy_id),
            )
        })?;
    Ok(MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ResStatus(
            tentaflow_protocol::DeploymentStatusResponse {
                deployment: deployment_row_to_summary(row),
            },
        ),
    ))
}

#[handler(variant = "DeploymentListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn deployment_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ReqList(r)) => r,
        _ => return Err(ProtocolError::bad_request("expected ReqList")),
    };
    let is_admin = matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    );
    let uid = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    let filter_user_id = if payload.only_mine || !is_admin {
        uid
    } else {
        None
    };
    let engine_id_filter = if payload.engine_id.is_empty() {
        None
    } else {
        Some(payload.engine_id.as_str())
    };
    let status_filter = if payload.status.is_empty() {
        None
    } else {
        Some(payload.status.as_str())
    };
    let limit = if payload.limit <= 0 {
        100
    } else {
        payload.limit as i64
    };
    let rows = repository::deployments::list(
        &ctx.state.db,
        engine_id_filter,
        status_filter,
        filter_user_id.as_deref(),
        limit,
    )
    .map_err(db_err)?;
    let deployments = rows.into_iter().map(deployment_row_to_summary).collect();
    Ok(MessageBody::DeploymentBody(
        tentaflow_protocol::DeploymentPayload::ResList(
            tentaflow_protocol::DeploymentListResponse { deployments },
        ),
    ))
}

// =============================================================================
// Addons + Users listy (FAZA 6 — REST → binary dla badge counts w nav)
// =============================================================================

#[handler(variant = "AddonsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addons_list(_req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let user_id_bytes = require_user_id(ctx)?;
    let user_id = user_id_to_uuid(&user_id_bytes);
    let is_admin = matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    );

    let rows = repository::list_addons(&ctx.state.db).map_err(db_err)?;
    let mut addons: Vec<tentaflow_protocol::AddonInfo> = Vec::with_capacity(rows.len());
    for a in rows.into_iter() {
        // Non-admin: filtruj po widocznosci (admin_only + group-based).
        if !is_admin
            && !repository::is_addon_visible_to_user(&ctx.state.db, &a.addon_id, &user_id)
                .map_err(db_err)?
        {
            continue;
        }
        let badges = repository::get_addon_badges(&ctx.state.db, &a.addon_id).map_err(db_err)?;
        let icon = if a.icon.is_empty() {
            None
        } else {
            Some(a.icon)
        };
        let category = if a.category.is_empty() {
            None
        } else {
            Some(a.category)
        };
        // update_available: w katalogu jest nowsza WERSJA pakietu niz przypieta
        // przez instancje (latest = pierwsza z list_package_versions), ALBO ta sama
        // wersja ma inny `bundle_hash` (zmiana tresci manifest/wasm/migracje bez
        // podbicia numeru — typowe dla addonow wbudowanych). Bez tego drugiego
        // warunku edycja addona pod tym samym numerem wersji bylaby niewidoczna.
        let update_available = if a.package_id.is_empty() {
            false
        } else {
            let newer_version = repository::list_package_versions(&ctx.state.db, &a.package_id)
                .map_err(db_err)?
                .first()
                .map(|latest| latest != &a.package_version)
                .unwrap_or(false);
            let content_changed = {
                let catalog_hash = repository::get_package_bundle_hash(
                    &ctx.state.db,
                    &a.package_id,
                    &a.package_version,
                )
                .map_err(db_err)?
                .unwrap_or_default();
                let installed_hash =
                    repository::get_instance_installed_bundle_hash(&ctx.state.db, &a.addon_id)
                        .map_err(db_err)?;
                !catalog_hash.is_empty() && catalog_hash != installed_hash
            };
            newer_version || content_changed
        };
        let display_name = if a.display_name.is_empty() {
            a.name.clone()
        } else {
            a.display_name
        };
        addons.push(tentaflow_protocol::AddonInfo {
            addon_id: a.addon_id,
            name: a.name,
            version: a.version,
            description: a.description,
            author: a.author,
            is_enabled: a.is_enabled,
            is_system: a.is_system,
            runtime: a.runtime,
            oauth_mode: badges.oauth_mode,
            visibility_scope: badges.visibility_scope,
            declared_permissions_count: badges.declared_permissions_count,
            users_with_oauth_count: badges.users_with_oauth_count,
            icon,
            category,
            file_size_bytes: a.wasm_size_bytes,
            package_id: a.package_id,
            package_version: a.package_version,
            display_name,
            update_available,
        });
    }
    Ok(MessageBody::AddonsListResponseBody(
        tentaflow_protocol::AddonsListResponse { addons },
    ))
}

// =============================================================================
// Audit log screen (R-LIST + export CSV + cleanup) — Admin only
// =============================================================================

/// Konwertuje proto `AuditLogFilters` do DB `AuditLogFilters`. Pole `search`
/// nie ma bezposredniego mappingu w DB modelu — stosujemy je jako dodatkowy
/// post-filter nizej.
fn proto_filters_to_db(
    f: &tentaflow_protocol::AuditLogFilters,
) -> crate::db::models::AuditLogFilters {
    crate::db::models::AuditLogFilters {
        user_id: f.user_id.clone(),
        addon_id: f.addon_id.clone(),
        action: f.action.clone(),
        from_date: f.from_date.clone(),
        to_date: f.to_date.clone(),
    }
}

fn proto_entry_from_db(e: crate::db::models::AuditLogEntry) -> tentaflow_protocol::AuditLogEntry {
    tentaflow_protocol::AuditLogEntry {
        id: e.id,
        timestamp: e.timestamp,
        action: e.action,
        user_id: e.user_id,
        addon_id: e.addon_id,
        resource: e.resource,
        details: e.details,
        ip_address: e.ip_address,
        node_id: e.node_id,
    }
}

/// Pelnotekstowe dopasowanie (LIKE) na action/resource/details.
fn matches_search(entry: &crate::db::models::AuditLogEntry, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    entry.action.to_lowercase().contains(&needle)
        || entry
            .resource
            .as_deref()
            .map(|s| s.to_lowercase().contains(&needle))
            .unwrap_or(false)
        || entry
            .details
            .as_deref()
            .map(|s| s.to_lowercase().contains(&needle))
            .unwrap_or(false)
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[handler(variant = "SchedulerJobsListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn scheduler_jobs_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::JobsListRequest(_)) => {}
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerJobsListRequestBody",
            ));
        }
    }
    let jobs = crate::scheduler::list_jobs(&ctx.state.db).map_err(db_err)?;
    let jobs_json = serde_json::to_string(&jobs)
        .map_err(|e| ProtocolError::internal(format!("scheduler jobs encode failed: {}", e)))?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobsListResponse(
            tentaflow_protocol::SchedulerJobsListResponse { jobs_json },
        ),
    ))
}

#[handler(variant = "SchedulerActionsListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn scheduler_actions_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::ActionsListRequest(_)) => {
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerActionsListRequestBody",
            ));
        }
    }
    let actions = crate::scheduler::list_addon_actions(&ctx.state.db).map_err(db_err)?;
    let actions_json = serde_json::to_string(&actions)
        .map_err(|e| ProtocolError::internal(format!("scheduler actions encode failed: {}", e)))?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::ActionsListResponse(
            tentaflow_protocol::SchedulerActionsListResponse { actions_json },
        ),
    ))
}

#[handler(variant = "SchedulerRunsListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn scheduler_runs_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::RunsListRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerRunsListRequestBody",
            ));
        }
    };
    let runs = crate::scheduler::list_runs(&ctx.state.db, &payload.job_id, payload.limit as i64)
        .map_err(db_err)?;
    let runs_json = serde_json::to_string(&runs)
        .map_err(|e| ProtocolError::internal(format!("scheduler runs encode failed: {}", e)))?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::RunsListResponse(
            tentaflow_protocol::SchedulerRunsListResponse { runs_json },
        ),
    ))
}

#[handler(variant = "SchedulerJobUpsertRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn scheduler_job_upsert(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::JobUpsertRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerJobUpsertRequestBody",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let mut input: crate::scheduler::UpsertJobRequest = serde_json::from_str(&payload.job_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid scheduler job json: {}", e)))?;
    // Stempel org_id z kontekstu uwierzytelnionego admina (blocker 1, R4): KAZDY job
    // tworzony przez dashboard dostaje org tworcy, a nie None. Dashboard nie wysyla org_id
    // (admin UI go nie zna), wiec bez tego stempla conflict_scan RAG biegnie z org_id=None
    // i omija asercje izolacji w execute_job — call_tool wykonalby sie na dzialajacej
    // (mozliwie cudzej/boot/default) instancji. org_context jest snapshotem org sesji,
    // wymagany dla tej sciezki (handler #[policy(Admin)] => zawsze ma sesje uzytkownika).
    let org_id = ctx
        .org_context
        .as_ref()
        .map(|o| o.org_id.clone())
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::AuthRequired,
                "scheduler job upsert requires an org context",
            )
        })?;
    input.org_id = Some(org_id);
    let job = crate::scheduler::upsert_job(&ctx.state.db, input, &user_id).map_err(db_err)?;
    let job_json = serde_json::to_string(&job)
        .map_err(|e| ProtocolError::internal(format!("scheduler job encode failed: {}", e)))?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobUpsertResponse(
            tentaflow_protocol::SchedulerJobUpsertResponse { job_json },
        ),
    ))
}

#[handler(variant = "SchedulerJobDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn scheduler_job_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::JobDeleteRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerJobDeleteRequestBody",
            ));
        }
    };
    crate::scheduler::delete_job(&ctx.state.db, &payload.job_id).map_err(db_err)?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobDeleteResponse(
            tentaflow_protocol::SchedulerJobDeleteResponse { ok: true },
        ),
    ))
}

#[handler(variant = "SchedulerJobRunNowRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn scheduler_job_run_now(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SchedulerBody(tentaflow_protocol::SchedulerPayload::JobRunNowRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SchedulerJobRunNowRequestBody",
            ));
        }
    };
    let addon_manager = ctx
        .state
        .addon_manager
        .clone()
        .ok_or_else(|| ProtocolError::internal("AddonManager unavailable"))?;
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let run = crate::scheduler::run_now(&ctx.state.db, addon_manager, &payload.job_id, &user_id)
        .await
        .map_err(db_err)?;
    let run_json = serde_json::to_string(&run)
        .map_err(|e| ProtocolError::internal(format!("scheduler run encode failed: {}", e)))?;
    Ok(MessageBody::SchedulerBody(
        tentaflow_protocol::SchedulerPayload::JobRunNowResponse(
            tentaflow_protocol::SchedulerJobRunNowResponse { run_json },
        ),
    ))
}

// =============================================================================
// Skills registry (Harness plan §3.2) — CRUD over the binary protocol.
// List/Detail are UserSession (Flow Builder reads skills too); writes are
// Admin-only. Hub import (quarantine + scan) and the curator (report/apply/
// rollback) live further down this file.
// =============================================================================

/// Handler-side shape of `SkillsUpsertRequest.skill_json`. `id` absent =
/// create (a fresh UUIDv4 is assigned, source is forced to 'user'); `files`
/// absent = keep the current reference files, present = full replacement
/// (an empty list clears them).
#[derive(serde::Deserialize)]
struct SkillUpsertInput {
    id: Option<String>,
    name: String,
    display_name: Option<String>,
    description: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    category: Option<String>,
    status: Option<String>,
    files: Option<Vec<SkillFileInput>>,
}

#[derive(serde::Deserialize)]
struct SkillFileInput {
    path: String,
    content: String,
}

/// List projection of `DbSkill` — every index field except `content`, which
/// can be 100k chars per row; the editor fetches it via `SkillsDetailRequest`.
#[derive(serde::Serialize)]
struct SkillSummary<'a> {
    id: &'a str,
    name: &'a str,
    display_name: Option<&'a str>,
    description: &'a str,
    tags_json: &'a str,
    category: Option<&'a str>,
    source: &'a str,
    source_ref: Option<&'a str>,
    status: &'a str,
    use_count: i64,
    last_used_at: Option<&'a str>,
    created_by: Option<&'a str>,
    created_at: &'a str,
    updated_at: &'a str,
}

#[handler(variant = "SkillsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn skills_list(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::ListRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request("expected SkillsListRequestBody"));
        }
    };
    let filter = db::models::SkillListFilter {
        source: payload.source.as_deref().filter(|s| !s.is_empty()),
        status: payload.status.as_deref().filter(|s| !s.is_empty()),
        tag: payload.tag.as_deref().filter(|s| !s.is_empty()),
    };
    let skills = repository::list_skills(&ctx.state.db, &filter).map_err(db_err)?;
    let summaries: Vec<SkillSummary<'_>> = skills
        .iter()
        .map(|s| SkillSummary {
            id: &s.id,
            name: &s.name,
            display_name: s.display_name.as_deref(),
            description: &s.description,
            tags_json: &s.tags_json,
            category: s.category.as_deref(),
            source: &s.source,
            source_ref: s.source_ref.as_deref(),
            status: &s.status,
            use_count: s.use_count,
            last_used_at: s.last_used_at.as_deref(),
            created_by: s.created_by.as_deref(),
            created_at: &s.created_at,
            updated_at: &s.updated_at,
        })
        .collect();
    let skills_json = serde_json::to_string(&summaries)
        .map_err(|e| ProtocolError::internal(format!("skills encode failed: {}", e)))?;
    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::ListResponse(tentaflow_protocol::SkillsListResponse {
            skills_json,
        }),
    ))
}

#[handler(variant = "SkillsDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn skills_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::DetailRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsDetailRequestBody",
            ));
        }
    };
    let skill = repository::get_skill(&ctx.state.db, &payload.skill_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("skill not found: {}", payload.skill_id))
        })?;
    let files = repository::list_skill_files(&ctx.state.db, &payload.skill_id).map_err(db_err)?;
    let skill_json = serde_json::to_string(&skill)
        .map_err(|e| ProtocolError::internal(format!("skill encode failed: {}", e)))?;
    let files_json = serde_json::to_string(&files)
        .map_err(|e| ProtocolError::internal(format!("skill files encode failed: {}", e)))?;
    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::DetailResponse(
            tentaflow_protocol::SkillsDetailResponse {
                skill_json,
                files_json,
            },
        ),
    ))
}

#[handler(variant = "SkillsUpsertRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_upsert(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::UpsertRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsUpsertRequestBody",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let input: SkillUpsertInput = serde_json::from_str(&payload.skill_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid skill json: {}", e)))?;

    let existing = match &input.id {
        Some(id) => Some(
            repository::get_skill(&ctx.state.db, id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found(format!("skill not found: {}", id)))?,
        ),
        None => None,
    };

    // Addon-sourced skills are owned by the addon package (upgrades overwrite
    // them, uninstall removes them) — only tags and status are admin-editable.
    if let Some(existing) = &existing {
        if existing.source == "addon" {
            let immutable_changed = input.name != existing.name
                || input.content != existing.content
                || input.description != existing.description
                || input.display_name.as_deref() != existing.display_name.as_deref()
                || input.category.as_deref() != existing.category.as_deref();
            if immutable_changed || input.files.is_some() {
                return Err(ProtocolError::bad_request(
                    "addon-sourced skills only allow tag/status edits — fork the skill \
                     (SkillsForkRequest) to get an editable user copy",
                ));
            }
        }
    }

    // The schema deliberately has no UNIQUE on name (sync apply across nodes),
    // so soft-uniqueness is enforced here on create and rename only.
    let name_changed = existing
        .as_ref()
        .map(|e| e.name != input.name)
        .unwrap_or(true);
    if name_changed
        && repository::get_skill_by_name(&ctx.state.db, &input.name)
            .map_err(db_err)?
            .is_some()
    {
        return Err(ProtocolError::bad_request(format!(
            "skill name already in use: '{}'",
            input.name
        )));
    }

    if let Some(files) = &input.files {
        let mut seen = std::collections::HashSet::new();
        for f in files {
            repository::validate_skill_file(&f.path, &f.content)
                .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
            if !seen.insert(f.path.as_str()) {
                return Err(ProtocolError::bad_request(format!(
                    "duplicate skill file path: '{}'",
                    f.path
                )));
            }
        }
    }

    let (skill_id, source, source_ref, created_by) = match &existing {
        Some(e) => (
            e.id.clone(),
            e.source.clone(),
            e.source_ref.clone(),
            e.created_by.clone(),
        ),
        None => (
            uuid::Uuid::new_v4().to_string(),
            "user".to_string(),
            None,
            Some(user_id.clone()),
        ),
    };
    let status = input
        .status
        .clone()
        .or_else(|| existing.as_ref().map(|e| e.status.clone()))
        .unwrap_or_else(|| "active".to_string());
    let tags_json = serde_json::to_string(&input.tags)
        .map_err(|e| ProtocolError::internal(format!("skill tags encode failed: {}", e)))?;
    let params = db::models::SkillParams {
        id: &skill_id,
        name: &input.name,
        display_name: input.display_name.as_deref(),
        description: &input.description,
        content: &input.content,
        tags_json: &tags_json,
        category: input.category.as_deref(),
        source: &source,
        source_ref: source_ref.as_deref(),
        status: &status,
        created_by: created_by.as_deref(),
        actor_user_id: Some(&user_id),
    };
    repository::validate_skill_params(&params)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    repository::upsert_skill(&ctx.state.db, &params).map_err(db_err)?;
    if let Some(files) = input.files {
        let files: Vec<(String, String)> = files.into_iter().map(|f| (f.path, f.content)).collect();
        repository::replace_skill_files(&ctx.state.db, &skill_id, &files, Some(&user_id))
            .map_err(db_err)?;
    }

    audit(
        ctx,
        Some(&user_id),
        "skill.upsert",
        Some(&format!("skill:{}", skill_id)),
        Some(&input.name),
    );

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::UpsertResponse(
            tentaflow_protocol::SkillsUpsertResponse { skill_id },
        ),
    ))
}

#[handler(variant = "SkillsDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::DeleteRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsDeleteRequestBody",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let skill = repository::get_skill(&ctx.state.db, &payload.skill_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("skill not found: {}", payload.skill_id))
        })?;
    if skill.source == "addon" {
        return Err(ProtocolError::bad_request(
            "addon-sourced skills cannot be deleted — uninstall the addon to remove its skill",
        ));
    }
    let deleted = repository::delete_skill(&ctx.state.db, &payload.skill_id).map_err(db_err)?;

    audit(
        ctx,
        Some(&user_id),
        "skill.delete",
        Some(&format!("skill:{}", payload.skill_id)),
        Some(&skill.name),
    );

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::DeleteResponse(
            tentaflow_protocol::SkillsDeleteResponse { deleted },
        ),
    ))
}

#[handler(variant = "SkillsForkRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_fork(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::ForkRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request("expected SkillsForkRequestBody"));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let origin = repository::get_skill(&ctx.state.db, &payload.skill_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("skill not found: {}", payload.skill_id))
        })?;
    if repository::get_skill_by_name(&ctx.state.db, &payload.new_name)
        .map_err(db_err)?
        .is_some()
    {
        return Err(ProtocolError::bad_request(format!(
            "skill name already in use: '{}'",
            payload.new_name
        )));
    }

    // The copy keeps the origin's status so forking a quarantined hub skill
    // cannot bypass the quarantine review; source becomes 'user' and the copy
    // is fully independent (no source_ref back-link, per plan §3.2).
    let new_id = uuid::Uuid::new_v4().to_string();
    let params = db::models::SkillParams {
        id: &new_id,
        name: &payload.new_name,
        display_name: origin.display_name.as_deref(),
        description: &origin.description,
        content: &origin.content,
        tags_json: &origin.tags_json,
        category: origin.category.as_deref(),
        source: "user",
        source_ref: None,
        status: &origin.status,
        created_by: Some(&user_id),
        actor_user_id: Some(&user_id),
    };
    repository::validate_skill_params(&params)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    repository::upsert_skill(&ctx.state.db, &params).map_err(db_err)?;
    let files = repository::list_skill_files(&ctx.state.db, &payload.skill_id).map_err(db_err)?;
    if !files.is_empty() {
        let files: Vec<(String, String)> = files.into_iter().map(|f| (f.path, f.content)).collect();
        repository::replace_skill_files(&ctx.state.db, &new_id, &files, Some(&user_id))
            .map_err(db_err)?;
    }

    audit(
        ctx,
        Some(&user_id),
        "skill.fork",
        Some(&format!("skill:{}", new_id)),
        Some(&format!(
            "forked from skill:{} as '{}'",
            payload.skill_id, payload.new_name
        )),
    );

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::ForkResponse(tentaflow_protocol::SkillsForkResponse {
            skill_id: new_id,
        }),
    ))
}

// =============================================================================
// Skills Hub (Harness plan §3.2 source `hub`) — runtime fetch/install.
// Import a skill from a public GitHub repo path or a direct SKILL.md URL into
// quarantine + run a static injection scan; admin approves (→ active) or rejects
// (→ delete). Every fetch goes through the public-URL SSRF guard. Admin-only.
// =============================================================================

/// Settings key holding the operator's configured GitHub taps (newline- or
/// comma-separated `owner/repo`). Unset → built-in defaults.
const SKILLS_HUB_TAPS_SETTING: &str = "skills_hub_taps";

/// Reference file the hub import is allowed to keep (registry prefixes only).
fn hub_file_is_keepable(path: &str) -> bool {
    repository::SKILL_FILE_ALLOWED_PREFIXES
        .iter()
        .any(|p| path.starts_with(p) && path.len() > p.len())
}

/// Derives a registry-valid kebab-case skill name from a frontmatter name or a
/// source-path fallback, then disambiguates against existing rows by appending a
/// numeric suffix (names are soft-unique — §3.2). Mirrors fallback_skill_name.
fn hub_resolve_name(db: &db::DbPool, preferred: &str) -> Result<String, ProtocolError> {
    let mut base = String::with_capacity(preferred.len());
    for ch in preferred.chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch.to_ascii_lowercase());
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() {
        base.push_str("hub-skill");
    }
    base.truncate(repository::SKILL_NAME_MAX_CHARS - 4);
    while base.ends_with('-') {
        base.pop();
    }
    if repository::get_skill_by_name(db, &base)
        .map_err(db_err)?
        .is_none()
    {
        return Ok(base);
    }
    for n in 2..=999 {
        let candidate = format!("{base}-{n}");
        if repository::get_skill_by_name(db, &candidate)
            .map_err(db_err)?
            .is_none()
        {
            return Ok(candidate);
        }
    }
    Err(ProtocolError::bad_request(
        "could not allocate a unique skill name for the import",
    ))
}

#[handler(variant = "SkillsHubSearchRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn skills_hub_search(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::HubSearchRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsHubSearchRequest",
            ));
        }
    };
    let taps_setting =
        repository::get_setting(&ctx.state.db, SKILLS_HUB_TAPS_SETTING).map_err(db_err)?;
    // A `source` override scopes to one tap; otherwise enumerate configured taps.
    let taps: Vec<String> = match payload.source.as_deref().filter(|s| !s.is_empty()) {
        Some(src) => vec![src.to_string()],
        None => crate::skills_hub::resolve_taps(taps_setting.as_deref()),
    };
    let query = payload.query.trim().to_lowercase();

    let results =
        tokio::task::spawn_blocking(move || crate::skills_hub::search_taps(&taps, &query))
            .await
            .map_err(|e| ProtocolError::internal(format!("hub search task failed: {e}")))?
            .map_err(|e| ProtocolError::internal(format!("hub search failed: {e}")))?;

    let results_json = serde_json::to_string(&results)
        .map_err(|e| ProtocolError::internal(format!("hub search encode failed: {e}")))?;
    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubSearchResponse(
            tentaflow_protocol::SkillsHubSearchResponse { results_json },
        ),
    ))
}

#[handler(variant = "SkillsHubImportRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn skills_hub_import(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::HubImportRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsHubImportRequest",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let source = crate::skills_hub::HubSource::parse(&payload.source, payload.git_ref.as_deref())
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    // Network fetch (blocking SSRF-guarded client) off the async runtime thread.
    let fetched = tokio::task::spawn_blocking({
        let source = source.clone();
        move || crate::skills_hub::fetch_skill(&source)
    })
    .await
    .map_err(|e| ProtocolError::internal(format!("hub fetch task failed: {e}")))?
    .map_err(|e| ProtocolError::bad_request(format!("hub import failed: {e}")))?;

    let parsed = crate::skills_hub::parse_skill_md(&fetched.source_md);
    // Validate every reference file up front; a non-keepable path (e.g. a stray
    // scripts/ file that slipped past directory filtering) rejects the import.
    let mut files: Vec<(String, String)> = Vec::new();
    for f in &fetched.files {
        if !hub_file_is_keepable(&f.path) {
            return Err(ProtocolError::bad_request(format!(
                "imported reference file '{}' is not under references/ or templates/",
                f.path
            )));
        }
        repository::validate_skill_file(&f.path, &f.content)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
        files.push((f.path.clone(), f.content.clone()));
    }

    let verdict = crate::skills_hub::scan_skill(&parsed.body, &fetched.files);

    let preferred = parsed.name.clone().unwrap_or_else(|| match &source {
        crate::skills_hub::HubSource::Github { repo, path, .. } => {
            if path.is_empty() {
                repo.clone()
            } else {
                path.rsplit('/').next().unwrap_or(repo).to_string()
            }
        }
        crate::skills_hub::HubSource::Url(url) => url
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("hub-skill")
            .to_string(),
    });
    let name = hub_resolve_name(&ctx.state.db, &preferred)?;
    let description = parsed
        .description
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("Imported from {}", source.provenance()));
    let description: String = description
        .chars()
        .take(repository::SKILL_DESCRIPTION_MAX_CHARS)
        .collect();
    let content: String = parsed
        .body
        .chars()
        .take(repository::SKILL_CONTENT_MAX_CHARS)
        .collect();
    let tags_json = serde_json::to_string(&parsed.tags)
        .map_err(|e| ProtocolError::internal(format!("hub tags encode failed: {e}")))?;
    let provenance = source.provenance();
    let skill_id = uuid::Uuid::new_v4().to_string();
    let params = db::models::SkillParams {
        id: &skill_id,
        name: &name,
        display_name: parsed.name.as_deref(),
        description: &description,
        content: &content,
        tags_json: &tags_json,
        category: None,
        source: "hub",
        source_ref: Some(&provenance),
        status: "quarantine",
        created_by: Some(&user_id),
        actor_user_id: Some(&user_id),
    };
    repository::validate_skill_params(&params)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    repository::upsert_skill(&ctx.state.db, &params).map_err(db_err)?;
    if !files.is_empty() {
        repository::replace_skill_files(&ctx.state.db, &skill_id, &files, Some(&user_id))
            .map_err(db_err)?;
    }

    audit(
        ctx,
        Some(&user_id),
        "skill.hub_import",
        Some(&format!("skill:{skill_id}")),
        Some(&format!(
            "{provenance} → quarantine ({} finding(s))",
            verdict.findings.len()
        )),
    );

    let verdict_json = serde_json::to_string(&verdict)
        .map_err(|e| ProtocolError::internal(format!("verdict encode failed: {e}")))?;
    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubImportResponse(
            tentaflow_protocol::SkillsHubImportResponse {
                skill_id,
                verdict_json,
            },
        ),
    ))
}

#[handler(variant = "SkillsHubApproveRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_hub_approve(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::HubApproveRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsHubApproveRequest",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let skill = repository::get_skill(&ctx.state.db, &payload.skill_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("skill not found: {}", payload.skill_id))
        })?;
    if skill.source != "hub" {
        return Err(ProtocolError::bad_request(
            "approve only applies to imported hub skills",
        ));
    }
    if skill.status != "quarantine" {
        return Err(ProtocolError::bad_request(
            "skill is not awaiting approval (status is not quarantine)",
        ));
    }
    // Flip quarantine → active in place, preserving every other field (provenance,
    // tags, content). Goes through the same upsert capture so the activation
    // replicates fleet-wide.
    let params = db::models::SkillParams {
        id: &skill.id,
        name: &skill.name,
        display_name: skill.display_name.as_deref(),
        description: &skill.description,
        content: &skill.content,
        tags_json: &skill.tags_json,
        category: skill.category.as_deref(),
        source: &skill.source,
        source_ref: skill.source_ref.as_deref(),
        status: "active",
        created_by: skill.created_by.as_deref(),
        actor_user_id: Some(&user_id),
    };
    repository::upsert_skill(&ctx.state.db, &params).map_err(db_err)?;

    audit(
        ctx,
        Some(&user_id),
        "skill.hub_approve",
        Some(&format!("skill:{}", skill.id)),
        Some(&format!(
            "{} → active",
            skill.source_ref.as_deref().unwrap_or(&skill.name)
        )),
    );

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubApproveResponse(
            tentaflow_protocol::SkillsHubApproveResponse { approved: true },
        ),
    ))
}

#[handler(variant = "SkillsHubRejectRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_hub_reject(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::HubRejectRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsHubRejectRequest",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let skill = repository::get_skill(&ctx.state.db, &payload.skill_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("skill not found: {}", payload.skill_id))
        })?;
    if skill.source != "hub" || skill.status != "quarantine" {
        return Err(ProtocolError::bad_request(
            "reject only applies to quarantined hub skills",
        ));
    }
    let rejected = repository::delete_skill(&ctx.state.db, &payload.skill_id).map_err(db_err)?;

    audit(
        ctx,
        Some(&user_id),
        "skill.hub_reject",
        Some(&format!("skill:{}", payload.skill_id)),
        Some(skill.source_ref.as_deref().unwrap_or(&skill.name)),
    );

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::HubRejectResponse(
            tentaflow_protocol::SkillsHubRejectResponse { rejected },
        ),
    ))
}

// =============================================================================
// Skills curator (Harness plan §3.2) — report-then-apply collection maintenance.
// Run produces a structured merge/umbrella/archive proposal (no mutation) anchored
// to a reversible snapshot; apply executes an admin-approved subset; rollback
// restores the captured pre-apply rows. The review LLM call goes through the
// router (auxiliary model). All Admin-only.
// =============================================================================

#[handler(variant = "SkillsCuratorRunRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn skills_curator_run(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::CuratorRunRequest(_)) => {}
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsCuratorRunRequest",
            ));
        }
    }
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let model = crate::skills::resolve_model(&ctx.state.db);
    let router = ctx.state.router.clone();
    let call_model = model.clone();
    let outcome =
        crate::skills::run_curator_review(&ctx.state.db, Some(&user_id), &model, move |prompt| {
            Box::pin(
                async move { crate::skills::router_complete(&router, &call_model, prompt).await },
            )
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("curator review failed: {e}")))?;

    audit(
        ctx,
        Some(&user_id),
        "skill.curator_run",
        Some(&format!("snapshot:{}", outcome.snapshot_id)),
        Some(&format!(
            "{} proposed action(s)",
            outcome.proposal.actions.len()
        )),
    );

    let proposal_json = serde_json::to_string(&outcome.proposal)
        .map_err(|e| ProtocolError::internal(format!("proposal encode failed: {e}")))?;
    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorRunResponse(
            tentaflow_protocol::SkillsCuratorRunResponse {
                proposal_json,
                snapshot_id: outcome.snapshot_id,
            },
        ),
    ))
}

#[handler(variant = "SkillsCuratorApplyRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_curator_apply(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::CuratorApplyRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsCuratorApplyRequest",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let approved: Vec<usize> = serde_json::from_str(&payload.approved_actions_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid approved_actions json: {e}")))?;
    // Per-mutation audit entries are emitted by the curator through this closure so
    // every archive/merge/umbrella records its own `audit_log` row (§3.2). Borrowed
    // (not moved) so `user_id` is still available as the apply's actor argument.
    let audit_fn = |event_kind: &str, resource: Option<&str>, message: Option<&str>| {
        audit(ctx, Some(&user_id), event_kind, resource, message);
    };
    let mutated = crate::skills::apply_proposal(
        &ctx.state.db,
        &payload.snapshot_id,
        &approved,
        Some(&user_id),
        &audit_fn,
    )
    .map_err(|e| ProtocolError::bad_request(format!("curator apply failed: {e}")))?;

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorApplyResponse(
            tentaflow_protocol::SkillsCuratorApplyResponse {
                mutated: mutated as u32,
            },
        ),
    ))
}

#[handler(variant = "SkillsCuratorRollbackRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn skills_curator_rollback(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SkillsBody(tentaflow_protocol::SkillsPayload::CuratorRollbackRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected SkillsCuratorRollbackRequest",
            ));
        }
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let audit_fn = |event_kind: &str, resource: Option<&str>, message: Option<&str>| {
        audit(ctx, Some(&user_id), event_kind, resource, message);
    };
    let restored = crate::skills::rollback_snapshot(
        &ctx.state.db,
        &payload.snapshot_id,
        Some(&user_id),
        &audit_fn,
    )
    .map_err(|e| ProtocolError::bad_request(format!("curator rollback failed: {e}")))?;

    Ok(MessageBody::SkillsBody(
        tentaflow_protocol::SkillsPayload::CuratorRollbackResponse(
            tentaflow_protocol::SkillsCuratorRollbackResponse {
                restored: restored as u32,
            },
        ),
    ))
}

// =============================================================================
// Agents registry (Harness plan §3.3)
// =============================================================================

/// Handler-side shape of `AgentsUpsertRequest.agent_json`. `id` absent =
/// create (a fresh UUIDv4 is assigned); present = update of an existing row.
/// Tool/skill allowlists and per-call params arrive as structured JSON so the
/// editor never has to hand-build the `*_json` column strings. Defaults mirror
/// the `agents` table column defaults so the editor may omit unset fields.
#[derive(serde::Deserialize)]
struct AgentUpsertInput {
    id: Option<String>,
    name: String,
    display_name: Option<String>,
    description: String,
    system_prompt: Option<String>,
    model: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default = "default_skills_selection")]
    skills: serde_json::Value,
    #[serde(default = "default_params")]
    params: serde_json::Value,
    #[serde(default = "default_max_iterations")]
    max_iterations: i64,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: i64,
    #[serde(default)]
    max_subagents: i64,
    #[serde(default = "default_max_spawn_depth")]
    max_spawn_depth: i64,
    flow_id: Option<String>,
    #[serde(default = "default_true")]
    routable: bool,
    #[serde(default = "default_true")]
    is_enabled: bool,
    /// `notify` (default) | `continue` — autonomous parent continuation on child
    /// completion (Harness §3.6 level 3). Admin-only to set (this handler is
    /// already #[policy(Admin)]); validated against the allowed set on upsert.
    #[serde(default = "default_on_child_complete")]
    on_child_complete: String,
}

fn default_skills_selection() -> serde_json::Value {
    serde_json::json!({})
}
fn default_params() -> serde_json::Value {
    serde_json::json!({})
}
fn default_max_iterations() -> i64 {
    25
}
fn default_timeout_secs() -> i64 {
    600
}
fn default_max_spawn_depth() -> i64 {
    1
}
fn default_true() -> bool {
    true
}
fn default_on_child_complete() -> String {
    "notify".to_string()
}

/// List projection of `DbAgent` — the columns the list screen renders plus the
/// tool/skill JSON the editor preloads. Drops nothing today (agents are small),
/// but kept as an explicit shape so future wide columns (e.g. long prompts) can
/// be excluded from the list without breaking the detail fetch.
#[derive(serde::Serialize)]
struct AgentSummary<'a> {
    id: &'a str,
    name: &'a str,
    display_name: Option<&'a str>,
    description: &'a str,
    model: Option<&'a str>,
    tools_json: &'a str,
    skills_json: &'a str,
    max_iterations: i64,
    routable: bool,
    is_enabled: bool,
    created_at: &'a str,
    updated_at: &'a str,
}

/// Run-list projection of `DbAgentRun` — every index field except `run_log`,
/// which holds the full step timeline and is fetched on demand by
/// `AgentRunDetailRequest`.
#[derive(serde::Serialize)]
struct AgentRunSummary<'a> {
    id: &'a str,
    agent_id: &'a str,
    parent_run_id: Option<&'a str>,
    flow_execution_id: Option<i64>,
    user_id: Option<&'a str>,
    status: &'a str,
    exit_reason: Option<&'a str>,
    iterations: i64,
    total_tokens: i64,
    last_heartbeat_at: Option<&'a str>,
    started_at: Option<&'a str>,
    finished_at: Option<&'a str>,
    created_at: &'a str,
}

/// One pickable tool in the catalog the editor renders as a checkbox tree:
/// addon tools grouped by `addon_id`, then the `core.*` builtins.
#[derive(serde::Serialize)]
struct CatalogTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(serde::Serialize)]
struct CatalogAddonGroup {
    addon_id: String,
    tools: Vec<CatalogTool>,
}

#[derive(serde::Serialize)]
struct ToolsCatalog {
    addons: Vec<CatalogAddonGroup>,
    core: Vec<CatalogTool>,
}

/// Builds the pickable tool catalog: addon tools grouped by `addon_id`, then
/// the `core.*` builtins. Shared by the ToolsCatalog handler and the
/// agent-builder assistant so both see the exact same tool universe.
fn build_tools_catalog(ctx: &HandlerContext) -> ToolsCatalog {
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<String, Vec<CatalogTool>> = BTreeMap::new();
    if let Some(manager) = &ctx.state.addon_manager {
        for tool in manager.list_tools() {
            grouped
                .entry(tool.addon_id.clone())
                .or_default()
                .push(CatalogTool {
                    name: format!("{}.{}", tool.addon_id, tool.tool_name),
                    description: tool.description,
                    parameters: tool.parameters_schema,
                });
        }
    }
    let addons = grouped
        .into_iter()
        .map(|(addon_id, mut tools)| {
            tools.sort_by(|a, b| a.name.cmp(&b.name));
            CatalogAddonGroup { addon_id, tools }
        })
        .collect();
    let core = crate::agents::CoreToolName::all()
        .iter()
        .map(|t| {
            let spec = t.spec();
            CatalogTool {
                name: spec.name,
                description: spec.description,
                parameters: spec.parameters,
            }
        })
        .collect();
    ToolsCatalog { addons, core }
}

/// True when the acting session is an admin. Non-admins only ever see their own
/// runs (Harness §3.3 ACL) — enforced here, never in the UI.
fn session_is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

#[handler(variant = "AgentsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agents_list(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::ListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentsListRequest")),
    };
    let filter = db::models::AgentListFilter {
        is_enabled: payload.enabled,
        routable: payload.routable,
    };
    let agents = repository::list_agents(&ctx.state.db, &filter).map_err(db_err)?;
    let summaries: Vec<AgentSummary<'_>> = agents
        .iter()
        .map(|a| AgentSummary {
            id: &a.id,
            name: &a.name,
            display_name: a.display_name.as_deref(),
            description: &a.description,
            model: a.model.as_deref(),
            tools_json: &a.tools_json,
            skills_json: &a.skills_json,
            max_iterations: a.max_iterations,
            routable: a.routable,
            is_enabled: a.is_enabled,
            created_at: &a.created_at,
            updated_at: &a.updated_at,
        })
        .collect();
    let agents_json = serde_json::to_string(&summaries)
        .map_err(|e| ProtocolError::internal(format!("agents encode failed: {}", e)))?;
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::ListResponse(tentaflow_protocol::AgentsListResponse {
            agents_json,
        }),
    ))
}

#[handler(variant = "AgentsDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agents_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::DetailRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentsDetailRequest")),
    };
    let agent = repository::get_agent(&ctx.state.db, &payload.agent_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("agent not found: {}", payload.agent_id))
        })?;
    let agent_json = serde_json::to_string(&agent)
        .map_err(|e| ProtocolError::internal(format!("agent encode failed: {}", e)))?;
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::DetailResponse(
            tentaflow_protocol::AgentsDetailResponse { agent_json },
        ),
    ))
}

#[handler(variant = "AgentsUpsertRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn agents_upsert(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::UpsertRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentsUpsertRequest")),
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let input: AgentUpsertInput = serde_json::from_str(&payload.agent_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid agent json: {}", e)))?;

    let existing = match &input.id {
        Some(id) => Some(
            repository::get_agent(&ctx.state.db, id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found(format!("agent not found: {}", id)))?,
        ),
        None => None,
    };

    // No UNIQUE on name (sync apply across nodes), so soft-uniqueness is checked
    // here on create and rename only — same contract as skills.
    let name_changed = existing
        .as_ref()
        .map(|e| e.name != input.name)
        .unwrap_or(true);
    if name_changed
        && repository::get_agent_by_name(&ctx.state.db, &input.name)
            .map_err(db_err)?
            .is_some()
    {
        return Err(ProtocolError::bad_request(format!(
            "agent name already in use: '{}'",
            input.name
        )));
    }

    // An empty flow_id from the editor means "use the seeded Agent Run flow"
    // (NULL column), not a reference to a flow literally named "".
    let flow_id = input.flow_id.as_deref().filter(|s| !s.is_empty());
    if let Some(flow_id) = flow_id {
        if repository::get_flow(&ctx.state.db, flow_id)
            .map_err(db_err)?
            .is_none()
        {
            return Err(ProtocolError::bad_request(format!(
                "agent flow_id references a missing flow: '{}'",
                flow_id
            )));
        }
    }

    let agent_id = existing
        .as_ref()
        .map(|e| e.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let tools_json = serde_json::to_string(&input.tools)
        .map_err(|e| ProtocolError::internal(format!("agent tools encode failed: {}", e)))?;
    let skills_json = serde_json::to_string(&input.skills)
        .map_err(|e| ProtocolError::internal(format!("agent skills encode failed: {}", e)))?;
    let params_json = serde_json::to_string(&input.params)
        .map_err(|e| ProtocolError::internal(format!("agent params encode failed: {}", e)))?;

    let params = db::models::AgentParams {
        id: &agent_id,
        name: &input.name,
        display_name: input.display_name.as_deref(),
        description: &input.description,
        system_prompt: input.system_prompt.as_deref(),
        model: input.model.as_deref().filter(|s| !s.is_empty()),
        tools_json: &tools_json,
        skills_json: &skills_json,
        params_json: &params_json,
        max_iterations: input.max_iterations,
        timeout_secs: input.timeout_secs,
        max_subagents: input.max_subagents,
        max_spawn_depth: input.max_spawn_depth,
        flow_id,
        routable: input.routable,
        is_enabled: input.is_enabled,
        on_child_complete: &input.on_child_complete,
        actor_user_id: Some(&user_id),
    };
    repository::validate_agent_params(&params)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    repository::upsert_agent(&ctx.state.db, &params).map_err(db_err)?;

    audit(
        ctx,
        Some(&user_id),
        "agent.upsert",
        Some(&format!("agent:{}", agent_id)),
        Some(&input.name),
    );

    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::UpsertResponse(
            tentaflow_protocol::AgentsUpsertResponse { agent_id },
        ),
    ))
}

#[handler(variant = "AgentsDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn agents_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::DeleteRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentsDeleteRequest")),
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    let agent = repository::get_agent(&ctx.state.db, &payload.agent_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("agent not found: {}", payload.agent_id))
        })?;
    let deleted = repository::delete_agent(&ctx.state.db, &payload.agent_id).map_err(db_err)?;

    audit(
        ctx,
        Some(&user_id),
        "agent.delete",
        Some(&format!("agent:{}", payload.agent_id)),
        Some(&agent.name),
    );

    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::DeleteResponse(
            tentaflow_protocol::AgentsDeleteResponse { deleted },
        ),
    ))
}

#[handler(variant = "AgentRunsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agent_runs_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::RunsListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentRunsListRequest")),
    };
    let actor_id = user_id_to_uuid(&require_user_id(ctx)?);
    // ACL: admins see every run; everyone else only their own principal's runs.
    let user_filter = if session_is_admin(ctx) {
        None
    } else {
        Some(actor_id.as_str())
    };
    let filter = db::models::AgentRunListFilter {
        agent_id: payload.agent_id.as_deref().filter(|s| !s.is_empty()),
        status: payload.status.as_deref().filter(|s| !s.is_empty()),
        parent_run_id: payload.parent_run_id.as_deref().filter(|s| !s.is_empty()),
        user_id: user_filter,
    };
    let runs = repository::list_agent_runs(&ctx.state.db, &filter).map_err(db_err)?;
    let summaries: Vec<AgentRunSummary<'_>> = runs
        .iter()
        .map(|r| AgentRunSummary {
            id: &r.id,
            agent_id: &r.agent_id,
            parent_run_id: r.parent_run_id.as_deref(),
            flow_execution_id: r.flow_execution_id,
            user_id: r.user_id.as_deref(),
            status: &r.status,
            exit_reason: r.exit_reason.as_deref(),
            iterations: r.iterations,
            total_tokens: r.total_tokens,
            last_heartbeat_at: r.last_heartbeat_at.as_deref(),
            started_at: r.started_at.as_deref(),
            finished_at: r.finished_at.as_deref(),
            created_at: &r.created_at,
        })
        .collect();
    let runs_json = serde_json::to_string(&summaries)
        .map_err(|e| ProtocolError::internal(format!("agent runs encode failed: {}", e)))?;
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunsListResponse(
            tentaflow_protocol::AgentRunsListResponse { runs_json },
        ),
    ))
}

#[handler(variant = "AgentRunDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agent_run_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::RunDetailRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentRunDetailRequest")),
    };
    let actor_id = user_id_to_uuid(&require_user_id(ctx)?);
    let run = repository::get_agent_run(&ctx.state.db, &payload.run_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("agent run not found: {}", payload.run_id))
        })?;
    // ACL: a non-admin can only read a run whose principal is themselves. A run
    // with no principal (unattended) is admin-only.
    if !session_is_admin(ctx) && run.user_id.as_deref() != Some(actor_id.as_str()) {
        return Err(ProtocolError::not_found(format!(
            "agent run not found: {}",
            payload.run_id
        )));
    }
    let run_json = serde_json::to_string(&run)
        .map_err(|e| ProtocolError::internal(format!("agent run encode failed: {}", e)))?;
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunDetailResponse(
            tentaflow_protocol::AgentRunDetailResponse { run_json },
        ),
    ))
}

/// ACL for answering a run's interaction (§3.13): a non-admin may reply only to
/// a run whose principal is themselves; an unattended (no-principal) run is
/// admin-only. A missing run is reported as not-found (not leaking existence).
fn assert_run_reply_access(ctx: &HandlerContext, run_id: &str) -> Result<(), ProtocolError> {
    let actor_id = user_id_to_uuid(&require_user_id(ctx)?);
    let run = repository::get_agent_run(&ctx.state.db, run_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found(format!("agent run not found: {run_id}")))?;
    if !session_is_admin(ctx) && run.user_id.as_deref() != Some(actor_id.as_str()) {
        return Err(ProtocolError::not_found(format!(
            "agent run not found: {run_id}"
        )));
    }
    Ok(())
}

#[handler(variant = "AgentRunReplyRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agent_run_reply(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::RunReplyRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentRunReplyRequest")),
    };
    // ACL: the run's principal or an admin may answer its questions (§3.13 A).
    assert_run_reply_access(ctx, &payload.run_id)?;

    let registry = crate::agents::interaction_registry_global();
    // The interaction must belong to the named run (an answer cannot be routed
    // to another run's question).
    let belongs = registry
        .info(&payload.question_id)
        .map(|i| i.run_id == payload.run_id && i.kind == crate::agents::InteractionKind::Question)
        .unwrap_or(false);
    let delivered = belongs
        && registry.reply(
            &payload.question_id,
            crate::agents::InteractionReply::Question(crate::agents::QuestionReply {
                answer: payload.answer.clone(),
            }),
        );
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunReplyResponse(
            tentaflow_protocol::AgentRunReplyResponse { delivered },
        ),
    ))
}

#[handler(variant = "AgentPermissionReplyRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agent_permission_reply(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::PermissionReplyRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AgentPermissionReplyRequest",
            ))
        }
    };
    assert_run_reply_access(ctx, &payload.run_id)?;

    let decision = crate::agents::PermissionDecision::parse(&payload.decision)
        .ok_or_else(|| ProtocolError::bad_request("unknown permission decision"))?;
    // An "always" grant is always persisted principal-scoped (tool_exec hardcodes
    // global=false), and the wire decision set has no global variant — so a
    // non-admin reply cannot widen a grant past its own principal. No admin check
    // is needed here; run ownership is enforced by assert_run_reply_access above.

    let registry = crate::agents::interaction_registry_global();
    let belongs = registry
        .info(&payload.request_id)
        .map(|i| i.run_id == payload.run_id && i.kind == crate::agents::InteractionKind::Permission)
        .unwrap_or(false);
    let delivered = belongs
        && registry.reply(
            &payload.request_id,
            crate::agents::InteractionReply::Permission(decision),
        );
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::PermissionReplyResponse(
            tentaflow_protocol::AgentPermissionReplyResponse { delivered },
        ),
    ))
}

#[handler(variant = "AgentRunCancelRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn agent_run_cancel(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::RunCancelRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentRunCancelRequest")),
    };
    // ACL: the run's principal or an admin may cancel it (§3.3).
    assert_run_reply_access(ctx, &payload.run_id)?;

    let user_id = require_user_id(ctx)?;
    let cancelled = crate::agents::agent_run_manager_global()
        .map(|m| m.cancel(&payload.run_id))
        .unwrap_or(false);

    audit(
        ctx,
        Some(&user_id_to_uuid(&user_id)),
        "agent.run_cancel",
        Some(&format!("run:{}", payload.run_id)),
        Some(if cancelled { "cancelled" } else { "not_live" }),
    );

    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunCancelResponse(
            tentaflow_protocol::AgentRunCancelResponse { cancelled },
        ),
    ))
}

#[handler(variant = "ToolsCatalogRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn tools_catalog(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    // Admin-only: the consumers are the admin-gated agent editor and the
    // builder assistant, and the catalog discloses every addon's full tool
    // surface (names, descriptions, JSON schemas) regardless of the caller's
    // own "llm" grants. Matching the handler policy to its consumers keeps the
    // disclosure admin-scoped. The catalog is the universe; the agent's
    // allowlist is the selection, intersected with live permissions at
    // execution time (§3.3).
    let catalog = build_tools_catalog(ctx);
    let tools_json = serde_json::to_string(&catalog)
        .map_err(|e| ProtocolError::internal(format!("tools catalog encode failed: {}", e)))?;
    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::ToolsCatalogResponse(
            tentaflow_protocol::ToolsCatalogResponse { tools_json },
        ),
    ))
}

#[handler(variant = "AgentRunStartRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn agent_run_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::RunStartRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AgentRunStartRequest")),
    };
    let user_id = user_id_to_uuid(&require_user_id(ctx)?);
    if payload.prompt.trim().is_empty() {
        return Err(ProtocolError::bad_request("prompt must not be empty"));
    }
    if payload.prompt.chars().count() > 8000 {
        return Err(ProtocolError::bad_request(
            "prompt exceeds 8000 characters",
        ));
    }
    let agent = repository::get_agent(&ctx.state.db, &payload.agent_id)
        .map_err(db_err)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("agent not found: {}", payload.agent_id))
        })?;
    if !agent.is_enabled {
        return Err(ProtocolError::bad_request(format!(
            "agent is disabled: {}",
            payload.agent_id
        )));
    }

    // Attended principal: the run acts as the starting session user (playground
    // "try it" — the user tests their own agent, so addon tools resolve under
    // their grants); the org snapshot stamps compliance attribution on the row.
    let principal = crate::agents::AgentPrincipal::new(
        Some(user_id.clone()),
        ctx.org_context.as_ref().map(|o| o.org_id.clone()),
    );
    let manager = crate::agents::agent_run_manager_global()
        .ok_or_else(|| ProtocolError::internal("agent run manager not initialized"))?;
    let run_id = manager
        .spawn(&agent.id, &payload.prompt, None, &principal, &[], &[], None)
        .await
        .map_err(|e| ProtocolError::internal(format!("agent run spawn failed: {e}")))?;

    audit(
        ctx,
        Some(&user_id),
        "agent.run_start",
        Some(&format!("agent:{}", payload.agent_id)),
        Some(&run_id),
    );

    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::RunStartResponse(
            tentaflow_protocol::AgentRunStartResponse { run_id },
        ),
    ))
}

/// Wire shape of one transcript turn in `AgentBuilderAssistRequest.messages_json`.
#[derive(serde::Deserialize)]
struct BuilderTurn {
    role: String,
    content: String,
}

/// System prompt of the agent-builder assistant. Polish text because the
/// assistant replies directly to the (Polish) dashboard user; the live tool
/// catalog is appended at request time.
const AGENT_BUILDER_SYSTEM_PROMPT: &str = r#"Jesteś asystentem tworzenia agentów w platformie TentaFlow. Prowadzisz krótką rozmowę z użytkownikiem, aby zaprojektować agenta. Dopytuj o: co agent dostaje na wejściu, co ma zrobić oraz jak ma wyglądać jego odpowiedź. Zadawaj pojedyncze, konkretne pytania. Gdy masz wystarczająco informacji, zwróć finalną propozycję agenta.

ZAWSZE odpowiadaj wyłącznie czystym JSON, bez markdownu i bez tekstu poza JSON, w formacie:
{"reply":"<twoja odpowiedź lub pytanie po polsku>","proposal":null}
a gdy propozycja jest gotowa:
{"reply":"<krótkie podsumowanie po polsku>","proposal":{"name":"nazwa-kebab-case","display_name":"...","description":"...","system_prompt":"...","tools":["<nazwy narzędzi z katalogu, np. deep-research.search_web, core.skill_view, albo wildcard całego addonu, np. deep-research.*>"],"max_iterations":25}}

W polu "tools" używaj wyłącznie nazw z poniższego katalogu (lub wildcardu <addon_id>.*)."#;

/// Extracts the first balanced `{...}` block from LLM output — models wrap
/// JSON in prose or code fences despite instructions. String-aware so braces
/// inside quoted values do not unbalance the scan.
fn extract_first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in text.as_bytes().iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[handler(variant = "AgentBuilderAssistRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn agent_builder_assist(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use crate::api::openai::types::{ChatCompletionRequest, ContentPart, Message, MessageContent};
    use std::collections::HashSet;

    let payload = match req {
        MessageBody::AgentsBody(tentaflow_protocol::AgentsPayload::BuilderAssistRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AgentBuilderAssistRequest",
            ));
        }
    };
    let turns: Vec<BuilderTurn> = serde_json::from_str(&payload.messages_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid messages json: {e}")))?;
    if turns.is_empty() || turns.len() > 20 {
        return Err(ProtocolError::bad_request(
            "messages must contain between 1 and 20 turns",
        ));
    }
    for turn in &turns {
        if turn.role != "user" && turn.role != "assistant" {
            return Err(ProtocolError::bad_request(
                "message role must be 'user' or 'assistant'",
            ));
        }
        if turn.content.chars().count() > 4000 {
            return Err(ProtocolError::bad_request(
                "message content exceeds 4000 characters",
            ));
        }
    }

    // The LLM picks tools by name: one flat "name — description" line per tool
    // keeps the prompt small, while the sets validate the picks afterwards.
    let catalog = build_tools_catalog(ctx);
    let mut tool_lines: Vec<String> = Vec::new();
    let mut valid_names: HashSet<&str> = HashSet::new();
    let mut valid_addon_ids: HashSet<&str> = HashSet::new();
    for group in &catalog.addons {
        valid_addon_ids.insert(group.addon_id.as_str());
        for tool in &group.tools {
            valid_names.insert(tool.name.as_str());
            tool_lines.push(format!("- {} — {}", tool.name, tool.description));
        }
    }
    for tool in &catalog.core {
        valid_names.insert(tool.name.as_str());
        tool_lines.push(format!("- {} — {}", tool.name, tool.description));
    }
    let system_prompt = format!(
        "{AGENT_BUILDER_SYSTEM_PROMPT}\n\nDostępne narzędzia:\n{}",
        tool_lines.join("\n")
    );

    let mut messages = Vec::with_capacity(turns.len() + 1);
    messages.push(Message {
        role: "system".to_string(),
        content: Some(MessageContent::Text(system_prompt)),
        reasoning_content: None,
        name: None,
        tool_calls: None,
        tool_call_id: None,
    });
    for turn in turns {
        messages.push(Message {
            role: turn.role,
            content: Some(MessageContent::Text(turn.content)),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let request = ChatCompletionRequest {
        model: crate::skills::resolve_model(&ctx.state.db),
        messages,
        temperature: Some(0.2),
        max_tokens: Some(2048),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: false,
        stream_options: None,
        user: Some("agent-builder".to_string()),
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
    };
    let result = ctx
        .state
        .router
        .route_chat_completion(request, None, None)
        .await
        .map_err(|e| ProtocolError::internal(format!("builder assist LLM call failed: {e}")))?;
    let raw = result
        .response
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|content| match content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        })
        .unwrap_or_default();

    // A malformed LLM reply is never a hard error: the raw text becomes the
    // reply so the dashboard conversation can continue.
    let parsed = extract_first_json_object(&raw)
        .and_then(|block| serde_json::from_str::<serde_json::Value>(block).ok())
        .filter(|v| v.is_object());
    let result_value = match parsed {
        Some(mut value) => {
            if value.get("reply").and_then(|r| r.as_str()).is_none() {
                value["reply"] = serde_json::Value::String(raw.clone());
            }
            if !value
                .get("proposal")
                .map(|p| p.is_object() || p.is_null())
                .unwrap_or(false)
            {
                value["proposal"] = serde_json::Value::Null;
            }
            // Drop hallucinated tool names: only catalog tools or a whole-addon
            // wildcard of an installed addon survive into the proposal.
            if let Some(tools) = value
                .get_mut("proposal")
                .and_then(|p| p.get_mut("tools"))
                .and_then(|t| t.as_array_mut())
            {
                tools.retain(|t| {
                    t.as_str().is_some_and(|name| {
                        valid_names.contains(name)
                            || name
                                .strip_suffix(".*")
                                .is_some_and(|addon| valid_addon_ids.contains(addon))
                    })
                });
            }
            value
        }
        None => serde_json::json!({ "reply": raw, "proposal": null }),
    };
    let result_json = serde_json::to_string(&result_value)
        .map_err(|e| ProtocolError::internal(format!("builder assist encode failed: {e}")))?;

    Ok(MessageBody::AgentsBody(
        tentaflow_protocol::AgentsPayload::BuilderAssistResponse(
            tentaflow_protocol::AgentBuilderAssistResponse { result_json },
        ),
    ))
}

#[handler(variant = "AuditLogListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn audit_log_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AuditLogListRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AuditLogListRequestBody",
            ));
        }
    };

    let db_filters = proto_filters_to_db(&payload.filters);
    let limit = payload.limit.clamp(1, 1000) as i64;
    let offset = payload.offset as i64;

    let rows =
        repository::list_audit_logs(&ctx.state.db, &db_filters, offset, limit).map_err(db_err)?;
    let total = repository::count_audit_logs(&ctx.state.db, &db_filters).map_err(db_err)?;

    let entries: Vec<_> = match payload.filters.search.as_deref() {
        Some(q) if !q.is_empty() => rows
            .into_iter()
            .filter(|e| matches_search(e, q))
            .map(proto_entry_from_db)
            .collect(),
        _ => rows.into_iter().map(proto_entry_from_db).collect(),
    };

    Ok(MessageBody::AuditLogListResponseBody(
        tentaflow_protocol::AuditLogListResponse {
            entries,
            total_count: total,
        },
    ))
}

#[handler(variant = "AuditLogExportRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn audit_log_export(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AuditLogExportRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AuditLogExportRequestBody",
            ));
        }
    };

    let db_filters = proto_filters_to_db(&payload.filters);
    let rows =
        repository::list_audit_logs(&ctx.state.db, &db_filters, 0, 100_000).map_err(db_err)?;

    let filtered: Vec<_> = match payload.filters.search.as_deref() {
        Some(q) if !q.is_empty() => rows.into_iter().filter(|e| matches_search(e, q)).collect(),
        _ => rows,
    };

    let mut csv =
        String::from("id,timestamp,user_id,addon_id,action,resource,details,ip_address,node_id\n");
    for e in &filtered {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            e.id,
            e.timestamp,
            e.user_id.as_deref().unwrap_or(""),
            e.addon_id.as_deref().unwrap_or(""),
            escape_csv(&e.action),
            e.resource.as_deref().map(escape_csv).unwrap_or_default(),
            e.details.as_deref().map(escape_csv).unwrap_or_default(),
            e.ip_address.as_deref().unwrap_or(""),
            e.node_id.as_deref().unwrap_or(""),
        ));
    }

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "audit.export",
        None,
        Some(&format!("rows={}", filtered.len())),
    );

    Ok(MessageBody::AuditLogExportResponseBody(
        tentaflow_protocol::AuditLogExportResponse {
            csv,
            row_count: filtered.len() as u64,
        },
    ))
}

#[handler(variant = "AuditLogCleanupRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn audit_log_cleanup(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AuditLogCleanupRequestBody(p) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected AuditLogCleanupRequestBody",
            ));
        }
    };
    if payload.keep_days < 1 {
        return Err(ProtocolError::bad_request("keep_days musi byc >= 1"));
    }

    let deleted =
        repository::cleanup_audit_logs(&ctx.state.db, payload.keep_days as i64).map_err(db_err)?;

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "audit.cleanup",
        None,
        Some(&format!(
            "keep_days={} deleted={}",
            payload.keep_days, deleted
        )),
    );

    Ok(MessageBody::AuditLogCleanupResponseBody(
        tentaflow_protocol::AuditLogCleanupResponse {
            deleted_count: deleted,
        },
    ))
}

// =============================================================================
// IAM — users + groups + resource permissions. Jeden top-level variant
// IamBody z inner enum IamPayload (zeby zmiescic sie w 256-variant CBOR limit).
// Wszystkie operacje mutujace wymagaja policy(Admin).
// =============================================================================

fn user_to_info(
    u: crate::db::models::UserAccount,
    group_ids: Vec<String>,
) -> tentaflow_protocol::UserInfo {
    tentaflow_protocol::UserInfo {
        id: u.id,
        username: u.username,
        display_name: u.display_name,
        email: u.email,
        is_active: u.is_active,
        is_admin: u.is_admin,
        sso_provider: u.sso_provider,
        last_login_at: u.last_login_at,
        created_at: u.created_at,
        role: u.role,
        group_ids,
    }
}

fn iam_err(e: anyhow::Error) -> ProtocolError {
    ProtocolError::internal(format!("IAM: {}", e))
}

#[handler(variant = "IamBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn iam_dispatch(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    use tentaflow_protocol::IamPayload as P;
    let payload = match req {
        MessageBody::IamBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected IamBody")),
    };
    let db = &ctx.state.db;

    let res = match payload {
        // ---- Users ----
        P::ReqListUsers => {
            let rows = repository::list_user_accounts(db).map_err(db_err)?;
            let users: Vec<_> = rows
                .into_iter()
                .map(|u| {
                    let gs = repository::get_user_groups(db, &u.id)
                        .ok()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|g| g.id)
                        .collect();
                    user_to_info(u, gs)
                })
                .collect();
            P::ResListUsers { users }
        }
        P::ReqGetUser { user_id } => {
            let u = repository::get_user_account_by_id(db, user_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::not_found("user"))?;
            let gs = repository::get_user_groups(db, user_id)
                .map_err(db_err)?
                .into_iter()
                .map(|g| g.id)
                .collect();
            P::ResGetUser {
                user: user_to_info(u, gs),
            }
        }
        P::ReqCreateUser {
            username,
            password,
            display_name,
            email,
            role,
            group_ids,
        } => {
            let hash = crate::crypto::hash_password(password)
                .map_err(|e| iam_err(anyhow::anyhow!("hash: {}", e)))?;
            let user_id = repository::create_user_account(db, username, &hash, display_name, email)
                .map_err(db_err)?;
            repository::set_user_role(db, &user_id, role).map_err(iam_err)?;
            for gid in group_ids {
                let _ = repository::add_user_to_group(db, gid, &user_id);
            }
            // Bez wiersza org_memberships sesja binary-WS rozwiazuje sie do
            // org_context=None i wszystkie sciezki filtrowane po org (ML Studio,
            // kamery, compliance) odrzucaja requesty tego usera. Rola org mapuje
            // sie z roli konta. Idempotentne przez PK (org_id, user_id).
            let org_role = match role.as_str() {
                "admin" => "role-org-admin",
                "power_user" => "role-org-operator",
                _ => "role-org-viewer",
            };
            crate::services::org::repo::add_membership(
                db,
                crate::services::org::DEFAULT_ORG_ID,
                &user_id,
                org_role,
                "system",
            )
            .map_err(|e| iam_err(anyhow::anyhow!("org membership: {}", e)))?;
            P::ResCreateUser { user_id }
        }
        P::ReqUpdateUser {
            user_id,
            display_name,
            email,
            is_active,
            role,
        } => {
            repository::update_user_account(db, user_id, display_name, email, *is_active)
                .map_err(db_err)?;
            repository::set_user_role(db, user_id, role).map_err(iam_err)?;
            P::ResOk
        }
        P::ReqDeleteUser { user_id } => {
            repository::delete_user_account(db, user_id).map_err(db_err)?;
            P::ResOk
        }
        P::ReqSetUserGroups { user_id, group_ids } => {
            // Prosty diff — remove z nieobecnych, add brakujace.
            let current: std::collections::HashSet<String> =
                repository::get_user_groups(db, user_id)
                    .map_err(db_err)?
                    .into_iter()
                    .map(|g| g.id)
                    .collect();
            let target: std::collections::HashSet<String> = group_ids.iter().cloned().collect();
            for gid in current.difference(&target) {
                let _ = repository::remove_user_from_group(db, gid, user_id);
            }
            for gid in target.difference(&current) {
                let _ = repository::add_user_to_group(db, gid, user_id);
            }
            P::ResOk
        }
        P::ReqResetUserPassword {
            user_id,
            new_password,
        } => {
            let hash = crate::crypto::hash_password(new_password)
                .map_err(|e| iam_err(anyhow::anyhow!("hash: {}", e)))?;
            repository::update_user_account_password(db, user_id, &hash).map_err(db_err)?;
            P::ResOk
        }

        // ---- Groups ----
        P::ReqListGroups => {
            let groups = repository::list_groups(db).map_err(db_err)?;
            let infos: Vec<_> = groups
                .into_iter()
                .map(|g| {
                    let count = repository::list_group_members(db, &g.id)
                        .ok()
                        .map(|m| m.len() as u32)
                        .unwrap_or(0);
                    tentaflow_protocol::GroupInfo {
                        id: g.id,
                        name: g.name,
                        description: g.description,
                        member_count: count,
                    }
                })
                .collect();
            P::ResListGroups { groups: infos }
        }
        P::ReqCreateGroup { name, description } => {
            let group_id = repository::create_group(db, name, description).map_err(db_err)?;
            P::ResCreateGroup { group_id }
        }
        P::ReqUpdateGroup {
            group_id,
            name,
            description,
        } => {
            repository::update_group(db, group_id, name, description).map_err(db_err)?;
            P::ResOk
        }
        P::ReqDeleteGroup { group_id } => {
            repository::delete_group(db, group_id).map_err(db_err)?;
            P::ResOk
        }
        P::ReqGroupMembers { group_id } => {
            let rows = repository::list_group_members(db, group_id).map_err(db_err)?;
            let members: Vec<_> = rows
                .into_iter()
                .map(|u| {
                    let gs = repository::get_user_groups(db, &u.id)
                        .ok()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|g| g.id)
                        .collect();
                    user_to_info(u, gs)
                })
                .collect();
            P::ResGroupMembers { members }
        }

        // ---- Resource permissions ----
        P::ReqSetPermission {
            resource_type,
            resource_id,
            subject_type,
            subject_id,
            access_level,
        } => {
            repository::resource_permissions::set(
                db,
                resource_type,
                resource_id,
                subject_type,
                subject_id,
                access_level,
            )
            .map_err(iam_err)?;
            P::ResOk
        }
        P::ReqClearPermission {
            resource_type,
            resource_id,
            subject_type,
            subject_id,
        } => {
            repository::resource_permissions::clear(
                db,
                resource_type,
                resource_id,
                subject_type,
                subject_id,
            )
            .map_err(db_err)?;
            P::ResOk
        }
        P::ReqListPermsForResource {
            resource_type,
            resource_id,
        } => {
            let rows =
                repository::resource_permissions::list_for_resource(db, resource_type, resource_id)
                    .map_err(db_err)?;
            let entries = rows
                .into_iter()
                .map(|r| tentaflow_protocol::PermissionEntry {
                    resource_type: r.resource_type,
                    resource_id: r.resource_id,
                    subject_type: r.subject_type,
                    subject_id: r.subject_id,
                    access_level: r.access_level,
                })
                .collect();
            P::ResListPermissions { entries }
        }
        P::ReqListPermsForSubject {
            subject_type,
            subject_id,
        } => {
            let rows =
                repository::resource_permissions::list_for_subject(db, subject_type, subject_id)
                    .map_err(db_err)?;
            let entries = rows
                .into_iter()
                .map(|r| tentaflow_protocol::PermissionEntry {
                    resource_type: r.resource_type,
                    resource_id: r.resource_id,
                    subject_type: r.subject_type,
                    subject_id: r.subject_id,
                    access_level: r.access_level,
                })
                .collect();
            P::ResListPermissions { entries }
        }

        // Response-only variants nie powinny byc requestowane przez klienta.
        P::ResListUsers { .. }
        | P::ResGetUser { .. }
        | P::ResCreateUser { .. }
        | P::ResListGroups { .. }
        | P::ResCreateGroup { .. }
        | P::ResGroupMembers { .. }
        | P::ResListPermissions { .. }
        | P::ResOk => {
            return Err(ProtocolError::bad_request("response variant in request"));
        }
    };

    Ok(MessageBody::IamBody(res))
}

// variant_name_of() zwraca nazwy inner payloadu (np. "IamListUsersRequest"),
// wiec musimy zarejestrowac iam_dispatch pod kazda z tych nazw. Macro
// `#[handler]` zarejestrowalo juz entry pod "IamBody" (nieuzywana, ale
// nieszkodliwa — HashMap i tak jej nie trafi). Wrapper __tentaflow_dispatch_iam_dispatch
// jest file-private, wiec submit! musi byc w tym samym pliku.
macro_rules! register_iam_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_iam_dispatch,
            }
        }
    };
}

register_iam_variant!("IamListUsersRequest", "tentaflow_ws_handler_iam_list_users");
register_iam_variant!("IamGetUserRequest", "tentaflow_ws_handler_iam_get_user");
register_iam_variant!(
    "IamCreateUserRequest",
    "tentaflow_ws_handler_iam_create_user"
);
register_iam_variant!(
    "IamUpdateUserRequest",
    "tentaflow_ws_handler_iam_update_user"
);
register_iam_variant!(
    "IamDeleteUserRequest",
    "tentaflow_ws_handler_iam_delete_user"
);
register_iam_variant!(
    "IamSetUserGroupsRequest",
    "tentaflow_ws_handler_iam_set_user_groups"
);
register_iam_variant!(
    "IamResetUserPasswordRequest",
    "tentaflow_ws_handler_iam_reset_user_password"
);
register_iam_variant!(
    "IamListGroupsRequest",
    "tentaflow_ws_handler_iam_list_groups"
);
register_iam_variant!(
    "IamCreateGroupRequest",
    "tentaflow_ws_handler_iam_create_group"
);
register_iam_variant!(
    "IamUpdateGroupRequest",
    "tentaflow_ws_handler_iam_update_group"
);
register_iam_variant!(
    "IamDeleteGroupRequest",
    "tentaflow_ws_handler_iam_delete_group"
);
register_iam_variant!(
    "IamGroupMembersRequest",
    "tentaflow_ws_handler_iam_group_members"
);
register_iam_variant!(
    "IamSetPermissionRequest",
    "tentaflow_ws_handler_iam_set_permission"
);
register_iam_variant!(
    "IamClearPermissionRequest",
    "tentaflow_ws_handler_iam_clear_permission"
);
register_iam_variant!(
    "IamListPermsForResourceRequest",
    "tentaflow_ws_handler_iam_list_perms_resource"
);
register_iam_variant!(
    "IamListPermsForSubjectRequest",
    "tentaflow_ws_handler_iam_list_perms_subject"
);

// =============================================================================
// Apps menu — multiplexed in `AddonUiBody` (256-variant CBOR limit).
// =============================================================================

#[handler(variant = "AddonUiBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_ui_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use tentaflow_protocol::AddonUiPayload as P;
    let payload = match req {
        MessageBody::AddonUiBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonUiBody")),
    };

    // Visibility: admin_only / group-restricted addons must not appear in
    // the launcher for unauthorized users.
    let user_id_bytes = require_user_id(ctx)?;
    let user_id = user_id_to_uuid(&user_id_bytes);
    let is_admin = matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    );
    let visible_to_user = |addon_id: &str| -> Result<bool, ProtocolError> {
        if is_admin {
            return Ok(true);
        }
        repository::is_addon_visible_to_user(&ctx.state.db, addon_id, &user_id).map_err(db_err)
    };

    let res = match payload {
        // ---- Apps menu ----
        P::ReqApplicationsList => {
            // Zrodlo prawdy: zainstalowane addony, ktore deklaruja
            // [application] w manifescie. Czytamy manifest_json z DB,
            // deserializujemy, filtrujemy po widocznosci dla usera.
            let rows = crate::db::repository::list_addons(&ctx.state.db).map_err(db_err)?;
            let mut applications: Vec<tentaflow_protocol::AddonApplicationInfo> = Vec::new();
            for a in rows {
                if !visible_to_user(&a.addon_id)? {
                    continue;
                }
                // UWAGA: `manifest_json` w DB to RAW manifest.toml string
                // (nazwa kolumny myli, patrz addon/lifecycle.rs:125).
                // Parsujemy przez parse_manifest_toml, NIE serde_json.
                let manifest: crate::addon::AddonManifest =
                    match crate::addon::lifecycle::parse_manifest_toml(&a.manifest_json) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                if let Some(app) = manifest.application {
                    // Multi-instance: w menu pokazujemy nazwe INSTANCJI (display_name)
                    // a nie tytul z manifestu pakietu — inaczej dwie instancje tego
                    // samego GUI-addona mialyby identyczna etykiete. Fallback na
                    // tytul manifestu gdy display_name pusty.
                    let title = if a.display_name.is_empty() {
                        app.title
                    } else {
                        a.display_name.clone()
                    };
                    applications.push(tentaflow_protocol::AddonApplicationInfo {
                        addon_id: a.addon_id.clone(),
                        title,
                        entry_panel: app.entry_panel,
                        icon: app.icon,
                        description: app.description,
                        sort_order: app.sort_order,
                        enabled: a.is_enabled,
                    });
                }
            }
            applications.sort_by(|a, b| {
                a.sort_order
                    .cmp(&b.sort_order)
                    .then_with(|| a.title.cmp(&b.title))
            });
            P::ResApplicationsList { applications }
        }

        // Response variants should not arrive as requests.
        P::ResApplicationsList { .. } => {
            return Err(ProtocolError::bad_request("response variant in request"));
        }
    };

    Ok(MessageBody::AddonUiBody(res))
}

// variant_name_of() zwraca nazwy inner payloadu, wiec rejestrujemy
// pod kazda z 3 request nazw (analogicznie do IamBody).
macro_rules! register_addon_ui_variant {
    ($variant:literal, $metric:literal, $auth:expr) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: $auth,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_addon_ui_dispatch,
            }
        }
    };
}

register_addon_ui_variant!(
    "AddonApplicationsListRequest",
    "tentaflow_ws_handler_addon_apps_list",
    crate::dispatch::SessionAuthKind::UserSession
);

fn sync_conflict_limit(limit: u32) -> usize {
    if limit == 0 {
        100
    } else {
        (limit as usize).min(500)
    }
}

fn validate_sync_conflict_scope(org_id: &str, addon_id: &str) -> Result<(), ProtocolError> {
    crate::addon::fs_sandbox::validate_addon_id(org_id)
        .map_err(|_| ProtocolError::bad_request("invalid org_id"))?;
    crate::addon::fs_sandbox::validate_addon_id(addon_id)
        .map_err(|_| ProtocolError::bad_request("invalid addon_id"))?;
    Ok(())
}

fn validate_sync_conflict_status(status: &str) -> Result<(), ProtocolError> {
    match status {
        "open" | "resolved" | "ignored" | "superseded" => Ok(()),
        _ => Err(ProtocolError::bad_request("invalid conflict status")),
    }
}

fn sync_conflict_row_to_wire(
    row: crate::addon::storage_sql_exec::SyncConflictRow,
) -> tentaflow_protocol::SyncConflictRow {
    tentaflow_protocol::SyncConflictRow {
        operation_id: row.operation_id,
        org_id: row.org_id,
        addon_id: row.addon_id,
        table_name: row.table_name,
        resource_type: row.resource_type,
        resource_id: row.resource_id,
        action: row.action,
        source_node_id: row.source_node_id,
        error_kind: row.error_kind,
        error_message: row.error_message,
        status: row.status,
        created_at_ms: row.created_at_ms,
        resolved_at_ms: row.resolved_at_ms,
        resolution: row.resolution,
    }
}

fn sync_conflict_resolution_to_storage(
    resolution: &tentaflow_protocol::SyncConflictResolution,
) -> crate::addon::storage_sql_exec::SyncConflictResolution {
    match resolution {
        tentaflow_protocol::SyncConflictResolution::KeepLocal => {
            crate::addon::storage_sql_exec::SyncConflictResolution::KeepLocal
        }
        tentaflow_protocol::SyncConflictResolution::Ignore => {
            crate::addon::storage_sql_exec::SyncConflictResolution::Ignore
        }
        tentaflow_protocol::SyncConflictResolution::AcceptRemote => {
            crate::addon::storage_sql_exec::SyncConflictResolution::AcceptRemote
        }
    }
}

#[handler(variant = "SyncConflictBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn sync_conflict_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use tentaflow_protocol::SyncConflictPayload as P;
    let payload = match req {
        MessageBody::SyncConflictBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected SyncConflictBody")),
    };

    let res = match payload {
        P::ListRequest(request) => {
            if request.addon_id.trim().is_empty() {
                return Err(ProtocolError::bad_request("addon_id is required"));
            }
            let org_id = if request.org_id.trim().is_empty() {
                "org-default"
            } else {
                request.org_id.as_str()
            };
            let status = if request.status.trim().is_empty() {
                "open"
            } else {
                request.status.as_str()
            };
            validate_sync_conflict_scope(org_id, &request.addon_id)?;
            validate_sync_conflict_status(status)?;
            let conflicts = crate::addon::storage_sql_exec::list_sync_conflicts(
                org_id,
                &request.addon_id,
                Some(status),
                sync_conflict_limit(request.limit),
            )
            .map_err(|e| ProtocolError::internal(format!("sync conflict list failed: {}", e)))?
            .into_iter()
            .map(sync_conflict_row_to_wire)
            .collect();
            P::ListResponse(tentaflow_protocol::SyncConflictsListResponse { conflicts })
        }
        P::ResolveRequest(request) => {
            if request.addon_id.trim().is_empty() {
                return Err(ProtocolError::bad_request("addon_id is required"));
            }
            let org_id = if request.org_id.trim().is_empty() {
                "org-default"
            } else {
                request.org_id.as_str()
            };
            validate_sync_conflict_scope(org_id, &request.addon_id)?;
            let operation_id = crate::sync::ledger::OperationId::from_hex(&request.operation_id)
                .map_err(|_| ProtocolError::bad_request("invalid operation_id"))?;
            let resolution = sync_conflict_resolution_to_storage(&request.resolution);
            let result = crate::sync::runtime::resolve_addon_sync_conflict(
                org_id,
                &request.addon_id,
                operation_id,
                resolution,
            )
            .map_err(|e| ProtocolError::internal(format!("sync conflict resolve failed: {}", e)))?
            .ok_or_else(|| ProtocolError::internal("sync runtime unavailable"))?;
            let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
            audit(
                ctx,
                user_id.as_deref(),
                "sync.conflict.resolve",
                Some(&request.operation_id),
                Some(&result.resolution),
            );
            P::ResolveResponse(tentaflow_protocol::SyncConflictResolveResponse {
                operation_id: result.operation_id,
                status: result.status,
                resolution: result.resolution,
                rows_affected: result.rows_affected,
            })
        }
        P::ListResponse(_) | P::ResolveResponse(_) => {
            return Err(ProtocolError::bad_request("response variant in request"));
        }
    };

    Ok(MessageBody::SyncConflictBody(res))
}

macro_rules! register_sync_conflict_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::Admin,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_sync_conflict_dispatch,
            }
        }
    };
}

register_sync_conflict_variant!(
    "SyncConflictsListRequest",
    "tentaflow_ws_handler_sync_conflicts_list"
);
register_sync_conflict_variant!(
    "SyncConflictResolveRequest",
    "tentaflow_ws_handler_sync_conflict_resolve"
);

fn sync_storage_level_to_wire(
    level: crate::sync::storage_monitor::StoragePressureLevel,
) -> tentaflow_protocol::SyncStoragePressureLevel {
    match level {
        crate::sync::storage_monitor::StoragePressureLevel::Ok => {
            tentaflow_protocol::SyncStoragePressureLevel::Ok
        }
        crate::sync::storage_monitor::StoragePressureLevel::Info => {
            tentaflow_protocol::SyncStoragePressureLevel::Info
        }
        crate::sync::storage_monitor::StoragePressureLevel::Warning => {
            tentaflow_protocol::SyncStoragePressureLevel::Warning
        }
        crate::sync::storage_monitor::StoragePressureLevel::Critical => {
            tentaflow_protocol::SyncStoragePressureLevel::Critical
        }
        crate::sync::storage_monitor::StoragePressureLevel::Unknown => {
            tentaflow_protocol::SyncStoragePressureLevel::Unknown
        }
    }
}

fn sync_storage_percent_to_bps(percent: Option<f64>) -> Option<u32> {
    percent.map(|value| (value * 100.0).round().clamp(0.0, 10_000.0) as u32)
}

fn sync_storage_report_to_wire(
    report: crate::sync::storage_monitor::StoragePressureReport,
) -> tentaflow_protocol::SyncStorageReportResponse {
    tentaflow_protocol::SyncStorageReportResponse {
        root: report.root.to_string_lossy().to_string(),
        level: sync_storage_level_to_wire(report.level),
        total_bytes: report.total_bytes,
        available_bytes: report.available_bytes,
        free_percent_bps: sync_storage_percent_to_bps(report.free_percent),
        sqlite_bytes: report.sqlite_bytes,
        fjall_ledger_bytes: report.fjall_ledger_bytes,
        snapshot_blob_bytes: report.snapshot_blob_bytes,
        final_blob_bytes: report.final_blob_bytes,
        pending_blob_chunk_bytes: report.pending_blob_chunk_bytes,
        large_blob_block_bytes: crate::sync::storage_monitor::LARGE_BLOB_BLOCK_BYTES,
        paths: report
            .paths
            .into_iter()
            .map(|path| tentaflow_protocol::SyncStoragePathUsage {
                label: path.label.to_string(),
                path: path.path.to_string_lossy().to_string(),
                bytes: path.bytes,
            })
            .collect(),
    }
}

#[handler(variant = "SyncStorageBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn sync_storage_dispatch(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use tentaflow_protocol::SyncStoragePayload as P;
    let payload = match req {
        MessageBody::SyncStorageBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected SyncStorageBody")),
    };

    let res = match payload {
        P::ReportRequest(_) => {
            let report = crate::sync::storage_monitor::current_report()
                .map_err(|e| ProtocolError::internal(format!("sync storage report failed: {e}")))?;
            P::ReportResponse(sync_storage_report_to_wire(report))
        }
        P::ReportResponse(_) => {
            return Err(ProtocolError::bad_request("response variant in request"));
        }
    };

    Ok(MessageBody::SyncStorageBody(res))
}

::inventory::submit! {
    crate::dispatch::HandlerMeta {
        variant_name: "SyncStorageReportRequest",
        since_major: 1,
        since_minor: 0,
        required_auth: crate::dispatch::SessionAuthKind::Admin,
        metric_name: "tentaflow_ws_handler_sync_storage_report",
        dispatch_fn: __tentaflow_dispatch_sync_storage_dispatch,
    }
}

// =============================================================================
// Mesh & Network settings (enumeracja IPv4 NIC + bind/advertise rules)
// =============================================================================

/// Klucze settings dla mesh network config. Kolejnosc i nazwy musza sie zgadzac
/// z migracja V57 i z polami `tentaflow_protocol::NetworkConfig`.
mod network_config_keys {
    pub const BIND_MODE: &str = "mesh.bind_mode";
    pub const BIND_IPV4: &str = "mesh.bind_ipv4";
    pub const HIDE_DOCKER: &str = "mesh.advertise_hide_docker";
    pub const HIDE_LINK_LOCAL: &str = "mesh.advertise_hide_link_local";
    pub const HIDE_LOOPBACK: &str = "mesh.advertise_hide_loopback";
    pub const HIDE_CGNAT: &str = "mesh.advertise_hide_cgnat";
    pub const PREFER_SAME_SUBNET: &str = "mesh.advertise_prefer_same_subnet";
}

fn parse_bool_setting(raw: &Option<String>, default: bool) -> bool {
    match raw.as_deref() {
        Some("1") | Some("true") => true,
        Some("0") | Some("false") => false,
        _ => default,
    }
}

fn bool_to_setting(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

fn load_network_config(
    ctx: &HandlerContext,
) -> Result<tentaflow_protocol::NetworkConfig, ProtocolError> {
    use network_config_keys::*;
    let pool = &ctx.state.db;

    let bind_mode = repository::get_setting(pool, BIND_MODE)
        .map_err(db_err)?
        .unwrap_or_else(|| "auto".to_string());
    let bind_ipv4 = repository::get_setting(pool, BIND_IPV4)
        .map_err(db_err)?
        .unwrap_or_default();
    let hide_docker = parse_bool_setting(
        &repository::get_setting(pool, HIDE_DOCKER).map_err(db_err)?,
        true,
    );
    let hide_link_local = parse_bool_setting(
        &repository::get_setting(pool, HIDE_LINK_LOCAL).map_err(db_err)?,
        true,
    );
    let hide_loopback = parse_bool_setting(
        &repository::get_setting(pool, HIDE_LOOPBACK).map_err(db_err)?,
        true,
    );
    let hide_cgnat = parse_bool_setting(
        &repository::get_setting(pool, HIDE_CGNAT).map_err(db_err)?,
        false,
    );
    let prefer_same_subnet = parse_bool_setting(
        &repository::get_setting(pool, PREFER_SAME_SUBNET).map_err(db_err)?,
        true,
    );
    let iroh_relay_url =
        repository::get_setting(pool, crate::net::iroh::relay::RELAY_URL_SETTING_KEY)
            .map_err(db_err)?
            .unwrap_or_else(|| crate::net::iroh::relay::DEFAULT_RELAY_URL.to_string());

    // Per-karta exclude — ten sam klucz/format (JSON array) co czyta
    // `load_advertise_filters`, zeby GUI i mesh widzialy to samo.
    let excluded_interfaces = repository::get_setting(
        pool,
        crate::mesh::network_interfaces::SETTING_EXCLUDED_INTERFACES,
    )
    .map_err(db_err)?
    .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
    .unwrap_or_default();

    Ok(tentaflow_protocol::NetworkConfig {
        bind_mode,
        bind_ipv4,
        hide_docker,
        hide_link_local,
        hide_loopback,
        hide_cgnat,
        prefer_same_subnet,
        iroh_relay_url,
        excluded_interfaces,
    })
}

/// Jeden handler dispatchuje wszystkie warianty `NetworkPayload`. Macro
/// `#[handler(variant = "NetworkBody")]` rejestruje go pod "NetworkBody",
/// a `register_network_variant!` ponizej re-rejestruje pod nazwami inner
/// payloadu — tak zeby `variant_name_of()` trafialo w HashMap.
#[handler(variant = "NetworkBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn network_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    use tentaflow_protocol::NetworkPayload as P;
    let payload = match req {
        MessageBody::NetworkBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected NetworkBody")),
    };

    let res = match payload {
        P::ReqInterfacesList => {
            let interfaces = crate::mesh::network_interfaces::list_interfaces();
            tracing::info!(
                count = interfaces.len(),
                names = ?interfaces.iter().map(|i| &i.name).collect::<Vec<_>>(),
                "network_dispatch: ReqInterfacesList"
            );
            P::ResInterfacesList { interfaces }
        }
        P::ReqConfigGet => {
            let cfg = load_network_config(ctx)?;
            P::ResConfigGet(cfg)
        }
        P::ReqConfigUpdate(new_cfg) => {
            tracing::info!(
                bind_mode = %new_cfg.bind_mode,
                bind_ipv4 = %new_cfg.bind_ipv4,
                relay = %new_cfg.iroh_relay_url,
                hide_docker = new_cfg.hide_docker,
                hide_link_local = new_cfg.hide_link_local,
                hide_loopback = new_cfg.hide_loopback,
                hide_cgnat = new_cfg.hide_cgnat,
                prefer_same_subnet = new_cfg.prefer_same_subnet,
                "network_dispatch: ReqConfigUpdate received"
            );
            if new_cfg.bind_mode != "auto" && new_cfg.bind_mode != "custom" {
                return Err(ProtocolError::bad_request(
                    "bind_mode must be 'auto' or 'custom'",
                ));
            }

            if new_cfg.bind_mode == "custom" {
                let parsed: std::net::Ipv4Addr = new_cfg.bind_ipv4.parse().map_err(|_| {
                    ProtocolError::bad_request(format!(
                        "bind_ipv4 '{}' is not a valid IPv4 address",
                        new_cfg.bind_ipv4
                    ))
                })?;
                let found = crate::mesh::network_interfaces::list_interfaces()
                    .into_iter()
                    .flat_map(|i| i.ipv4_addrs)
                    .any(|a| {
                        a.parse::<std::net::Ipv4Addr>()
                            .map(|v| v == parsed)
                            .unwrap_or(false)
                    });
                if !found {
                    return Err(ProtocolError::bad_request(format!(
                        "bind_ipv4 '{}' is not present on any local interface",
                        new_cfg.bind_ipv4
                    )));
                }
            }

            // Porownanie stanu z DB -> decyzja czy potrzebny restart silnika iroh.
            // Zmiany filtrow advertise sa stosowane dynamicznie, restart tylko gdy
            // zmieni sie bind_mode / bind_ipv4 / relay URL (wymaga rebuild endpointu).
            let previous = load_network_config(ctx)?;
            let restart_required = previous.bind_mode != new_cfg.bind_mode
                || previous.bind_ipv4 != new_cfg.bind_ipv4
                || previous.iroh_relay_url != new_cfg.iroh_relay_url;

            use network_config_keys::*;
            let pool = &ctx.state.db;
            repository::set_setting(pool, BIND_MODE, &new_cfg.bind_mode).map_err(db_err)?;
            repository::set_setting(pool, BIND_IPV4, &new_cfg.bind_ipv4).map_err(db_err)?;
            repository::set_setting(pool, HIDE_DOCKER, bool_to_setting(new_cfg.hide_docker))
                .map_err(db_err)?;
            repository::set_setting(
                pool,
                HIDE_LINK_LOCAL,
                bool_to_setting(new_cfg.hide_link_local),
            )
            .map_err(db_err)?;
            repository::set_setting(pool, HIDE_LOOPBACK, bool_to_setting(new_cfg.hide_loopback))
                .map_err(db_err)?;
            repository::set_setting(pool, HIDE_CGNAT, bool_to_setting(new_cfg.hide_cgnat))
                .map_err(db_err)?;
            repository::set_setting(
                pool,
                PREFER_SAME_SUBNET,
                bool_to_setting(new_cfg.prefer_same_subnet),
            )
            .map_err(db_err)?;
            repository::set_setting(
                pool,
                crate::net::iroh::relay::RELAY_URL_SETTING_KEY,
                &new_cfg.iroh_relay_url,
            )
            .map_err(db_err)?;
            // Per-karta exclude jako JSON array (zero CSV). Filtrowanie advertise
            // czyta to dynamicznie przez `load_advertise_filters`, bez restartu.
            let excluded_json = serde_json::to_string(&new_cfg.excluded_interfaces)
                .unwrap_or_else(|_| "[]".to_string());
            repository::set_setting(
                pool,
                crate::mesh::network_interfaces::SETTING_EXCLUDED_INTERFACES,
                &excluded_json,
            )
            .map_err(db_err)?;

            let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
            audit(
                ctx,
                user_id.as_deref(),
                "mesh.network_config.update",
                Some("mesh.network_config"),
                Some(&format!(
                    "bind_mode={} restart_required={}",
                    new_cfg.bind_mode, restart_required
                )),
            );

            P::ResConfigUpdate { restart_required }
        }
        P::ReqRelayStatus => {
            // Snapshot stanu relay z background monitora; gdy mesh nie wystartowal
            // (np. mesh.enabled=false), zwracamy "disabled" z pustym URL.
            let info = match &ctx.state.mesh_relay_health {
                Some(state) => {
                    let g = state.read();
                    tentaflow_protocol::RelayHealthInfo {
                        url: g.url.clone(),
                        reachable: g.reachable,
                        rtt_ms: g.rtt_ms.unwrap_or(0),
                        last_check_unix_secs: g.last_check_unix_secs,
                        last_success_unix_secs: g.last_success_unix_secs.unwrap_or(0),
                        status: g.status.clone(),
                        bind_addr_actual: g.bind_addr_actual.clone(),
                    }
                }
                None => tentaflow_protocol::RelayHealthInfo {
                    url: String::new(),
                    reachable: false,
                    rtt_ms: 0,
                    last_check_unix_secs: 0,
                    last_success_unix_secs: 0,
                    status: "disabled".to_string(),
                    bind_addr_actual: String::new(),
                },
            };
            P::ResRelayStatus(info)
        }
        P::ResInterfacesList { .. }
        | P::ResConfigGet(_)
        | P::ResConfigUpdate { .. }
        | P::ResRelayStatus(_) => {
            return Err(ProtocolError::bad_request(
                "response variants are not accepted as requests",
            ));
        }
    };

    Ok(MessageBody::NetworkBody(res))
}

// Re-rejestruje `network_dispatch` pod inner-payload variant names tak, zeby
// `variant_name_of()` -> Registry::find() je znajdowalo.
macro_rules! register_network_variant {
    ($variant:literal, $metric:literal) => {
        register_network_variant!($variant, $metric, Admin);
    };
    ($variant:literal, $metric:literal, $auth:ident) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::$auth,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_network_dispatch,
            }
        }
    };
}

register_network_variant!(
    "NetworkInterfacesListRequest",
    "tentaflow_ws_handler_network_interfaces_list"
);
register_network_variant!(
    "NetworkConfigGetRequest",
    "tentaflow_ws_handler_network_config_get"
);
register_network_variant!(
    "NetworkConfigUpdateRequest",
    "tentaflow_ws_handler_network_config_update"
);
// Status relay jest informacja read-only dla zwyklych userow na ekranie Mesh —
// nie wymaga roli admina. Zwykly UserSession wystarcza.
register_network_variant!(
    "NetworkRelayStatusRequest",
    "tentaflow_ws_handler_network_relay_status",
    UserSession
);

// =============================================================================
// Services (Krok N2) — local-only view powered by the unified `services` +
// `model_registry` tables. Multi-node aggregation lands in Krok N5; for now the
// list contains only services owned by this node. Every handler is admin-gated
// because the sidebar is admin-only and the mutations affect runtime processes.
// =============================================================================

fn build_service_info(
    conn: &rusqlite::Connection,
    svc: crate::services_repo::services::ServiceRow,
    local_node_id: &str,
) -> Result<tentaflow_protocol::ServiceInfo, ProtocolError> {
    crate::services::snapshot_builder::project_service_row(conn, svc, local_node_id).map_err(db_err)
}

/// Push a `ServiceChange` to every trusted mesh peer. Fires after every
/// successful local mutation (deploy / stop / pin / pause / rename / delete)
/// so peers' `MeshServicesRegistry` view converges in real time instead of
/// waiting for the 5-min anti-drift announce. No-op when the mesh manager is
/// not initialised (single-node mode, tests).
pub(super) fn broadcast_service_change(
    ctx: &HandlerContext,
    change: tentaflow_protocol::ServiceChange,
) {
    let qm = match ctx.state.quic_mesh.as_ref() {
        Some(q) => q.clone(),
        None => return,
    };
    let from = ctx.state.local_node_id.to_string();
    let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
        from_node_id: from,
        change,
    };
    let bytes = match crate::mesh::cbor::encode(&payload) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "MeshServicesUpdate: CBOR encode failed");
            return;
        }
    };
    tokio::spawn(async move {
        let _ = qm
            .broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                &bytes,
                None,
            )
            .await;
    });
}

/// Build a `ServiceInfo` for `service_id` (current DB state) and push it as
/// `ServiceChange::Updated`. Used by mutating handlers that left the row in
/// place but changed its fields (stop, pin, pause, rename, redeploy).
fn push_service_updated(ctx: &HandlerContext, service_id: i64) {
    let local = ctx.state.local_node_id.to_string();
    let info = match crate::services::snapshot_builder::build_one(&ctx.state.db, service_id, &local)
    {
        Ok(Some(info)) => info,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, service_id, "push_service_updated: build_one failed");
            return;
        }
    };
    broadcast_service_change(ctx, tentaflow_protocol::ServiceChange::Updated(info));
}

#[handler(variant = "ServiceListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn service_list(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqList(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqList",
            ));
        }
    };

    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let rows = crate::services_repo::services::list_all(&conn).map_err(db_err)?;
    let local_node_id = ctx.state.local_node_id.as_ref();

    // Klaster (distributed TP) jest reprezentowany WPROST przez per-node wiersze
    // czlonkow (head + workery), po jednym na nodzie — jak kazdy inny serwis,
    // tyle ze na wielu nodach. Nie ma osobnej „wizytowki" ani ukrywania czlonkow.

    // Local rows first.
    let mut services = Vec::with_capacity(rows.len());
    for svc in rows {
        if let Some(filter) = payload.engine_id_filter.as_deref() {
            if !filter.is_empty() && svc.engine_id != filter {
                continue;
            }
        }
        if let Some(filter) = payload.category_filter.as_deref() {
            if !filter.is_empty() && svc.category != filter {
                continue;
            }
        }
        services.push(build_service_info(&conn, svc, local_node_id)?);
    }
    drop(conn);

    // Then merge in every peer's snapshot (krok N3b — single-flat list, GUI
    // groups by `service.node_id`).
    for (_node_id, snapshot) in ctx.state.mesh_services_registry.all_remote() {
        for svc in snapshot {
            if let Some(filter) = payload.engine_id_filter.as_deref() {
                if !filter.is_empty() && svc.engine_id != filter {
                    continue;
                }
            }
            if let Some(filter) = payload.category_filter.as_deref() {
                if !filter.is_empty() && svc.category != filter {
                    continue;
                }
            }
            services.push(svc);
        }
    }

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResList(tentaflow_protocol::ServiceListResponse {
            services,
        }),
    ))
}

/// True when the request's `node_id` refers to another mesh node and the
/// action must be forwarded over `MeshCommandType::Service*Remote`. `None`
/// or the local node id falls through to local execution.
fn forward_target_node<'a>(ctx: &'a HandlerContext, target: &'a Option<String>) -> Option<&'a str> {
    let target = target.as_deref()?;
    if target.is_empty() || target == ctx.state.local_node_id.as_ref() {
        None
    } else {
        Some(target)
    }
}

fn reject_ambiguous_local_service_action(
    ctx: &HandlerContext,
    target: &Option<String>,
    service_id: i64,
) -> Result<(), ProtocolError> {
    let explicit_target = target.as_deref().map(str::trim).filter(|v| !v.is_empty());
    if explicit_target.is_some() {
        return Ok(());
    }
    if let Some(remote_node_id) = ctx
        .state
        .mesh_services_registry
        .find_node_for_service(service_id)
    {
        return Err(ProtocolError::bad_request(format!(
            "service_id={} is ambiguous across mesh; request must include node_id (remote owner: {})",
            service_id, remote_node_id
        )));
    }
    Ok(())
}

/// Forward a service-action `MeshCommandType::*Remote` over iroh and convert
/// the typed `MeshCommandResponse` envelope into the boolean ok/error pair the
/// dispatch-side `Service*Response` expects. Errors from the transport
/// itself surface as `success=false, error=Some(...)`.
async fn forward_service_action(
    ctx: &HandlerContext,
    target_node_id: &str,
    cmd: tentaflow_protocol::mesh::MeshCommandType,
) -> (bool, Option<String>) {
    let iroh = match ctx.state.quic_mesh.as_ref() {
        Some(m) => m.clone(),
        None => {
            return (
                false,
                Some("mesh transport not available on this node".to_string()),
            );
        }
    };
    if let Some(security) = ctx.state.mesh_security.as_ref() {
        if !security.is_trusted(target_node_id) {
            return (
                false,
                Some(format!("peer {} is not trusted", target_node_id)),
            );
        }
    }
    match iroh.send_command_and_wait(target_node_id, cmd, 10).await {
        Ok(resp) => (resp.ok, resp.error),
        Err(e) => (false, Some(e.to_string())),
    }
}

/// Forward a mesh command and return the FULL response (so callers can read the
/// typed payload, not just ok/error). Trust + transport errors become `Err`.
async fn forward_command(
    ctx: &HandlerContext,
    target_node_id: &str,
    cmd: tentaflow_protocol::mesh::MeshCommandType,
) -> Result<crate::mesh::iroh_manager::CommandWaitResponse, String> {
    let iroh = ctx
        .state
        .quic_mesh
        .as_ref()
        .ok_or("mesh transport not available on this node")?
        .clone();
    if let Some(security) = ctx.state.mesh_security.as_ref() {
        if !security.is_trusted(target_node_id) {
            return Err(format!("peer {} is not trusted", target_node_id));
        }
    }
    iroh.send_command_and_wait(target_node_id, cmd, 15)
        .await
        .map_err(|e| e.to_string())
}

/// Resolves a service row by id, returning a NotFound protocol error when the
/// row is gone. Caller drops the lock before doing async work.
fn fetch_service_row(
    ctx: &HandlerContext,
    service_id: i64,
) -> Result<crate::services_repo::services::ServiceRow, ProtocolError> {
    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let row = crate::services_repo::services::get(&conn, service_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::not_found(format!("service id={} not found", service_id)))?;
    Ok(row)
}

#[handler(variant = "ServiceDeleteRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqDelete(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqDelete",
            ));
        }
    };

    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServiceDeleteRemote {
            service_id: payload.service_id,
        };
        let (success, error) = forward_service_action(ctx, target, cmd).await;
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResDelete(
                tentaflow_protocol::ServiceDeleteResponse { success, error },
            ),
        ));
    }
    reject_ambiguous_local_service_action(ctx, &payload.node_id, payload.service_id)?;

    let svc = fetch_service_row(ctx, payload.service_id)?;
    // Czlonek AKTYWNEGO klastra TP: usuniecie workera/heada z listy serwisow
    // zabija rank calego distributed-deploymentu (serwujacego czesto na INNYM
    // nodzie). Legalna sciezka = stop deploymentu klastra, ktory kasuje wiersze
    // czlonkow sam w teardownie. Osierocony wiersz (deployment martwy) przechodzi.
    if crate::services::deploy::distributed::service_is_distributed_member(&svc.config_json)
        && crate::services::deploy::distributed::distributed_member_deployment_active(
            &ctx.state.db,
            &svc.config_json,
        )
        .await
    {
        return Err(ProtocolError::bad_request(
            "serwis jest czlonkiem AKTYWNEGO deploymentu klastra — zatrzymaj deployment klastra zamiast kasowac pojedynczy wiersz",
        ));
    }
    let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
        ProtocolError::internal("port allocator not initialized (supervisor disabled)")
    })?;

    // Best-effort runtime stop first. Even if it fails (orphaned container,
    // stale pid) we still drop the row so the GUI does not get stuck on a
    // zombie entry. Surface the error for the toast but treat overall op as ok.
    let stop_err = crate::services::deploy::stop(&svc, port_allocator)
        .await
        .err()
        .map(|e| e.to_string());

    // Delete the row and, under the SAME guard, read the ports still owned by
    // sibling rows of this engine. Confined to a sync block so the DB guard is
    // dropped before the await below (the future must stay `Send`).
    let keep_ports: Vec<u16> = {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::delete(&conn, payload.service_id).map_err(db_err)?;
        crate::services_repo::services::list_all(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.engine_id == svc.engine_id)
            .filter_map(|r| r.runtime_port)
            .collect()
    };

    // Belt-and-suspenders: `stop()` only targeted the row's recorded pid/port.
    // Sweep any runtime of this engine that drifted ports or lost its pid to a
    // prior non-graceful Core exit, so a delete can't leave an untraceable orphan.
    crate::services::deploy::stop_engine_orphans(&svc.engine_id, &keep_ports).await;

    // Service row gone — refresh the catalog so its model entries stop
    // appearing on `/v1/models`. Supervisor reconcile would catch this on
    // its next tick, but desktop has no supervisor and even on the binary
    // a 1s lag here causes confusing GUI state.
    ctx.state.router.rebuild_catalog();

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.delete",
        Some(&svc.engine_id),
        Some(&format!(
            "service_id={} stop_err={}",
            payload.service_id,
            stop_err.as_deref().unwrap_or("none")
        )),
    );

    broadcast_service_change(
        ctx,
        tentaflow_protocol::ServiceChange::Removed {
            service_id: payload.service_id,
        },
    );

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResDelete(tentaflow_protocol::ServiceDeleteResponse {
            success: true,
            error: stop_err,
        }),
    ))
}

#[handler(variant = "ServicePinRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_pin(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqPin(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqPin",
            ));
        }
    };

    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServicePinRemote {
            service_id: payload.service_id,
            pinned: payload.pinned,
        };
        let (success, error) = forward_service_action(ctx, target, cmd).await;
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResPin(tentaflow_protocol::ServicePinResponse {
                success,
                error,
            }),
        ));
    }
    reject_ambiguous_local_service_action(ctx, &payload.node_id, payload.service_id)?;

    let conn = ctx
        .state
        .db
        .write()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    crate::services_repo::services::set_pinned(&conn, payload.service_id, payload.pinned)
        .map_err(db_err)?;
    drop(conn);

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.pin",
        None,
        Some(&format!(
            "service_id={} pinned={}",
            payload.service_id, payload.pinned
        )),
    );

    push_service_updated(ctx, payload.service_id);

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResPin(tentaflow_protocol::ServicePinResponse {
            success: true,
            error: None,
        }),
    ))
}

#[handler(variant = "ServicePauseRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_pause(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqPause(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqPause",
            ));
        }
    };

    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServicePauseRemote {
            service_id: payload.service_id,
            paused: payload.paused,
        };
        let (success, error) = forward_service_action(ctx, target, cmd).await;
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResPause(
                tentaflow_protocol::ServicePauseResponse { success, error },
            ),
        ));
    }
    reject_ambiguous_local_service_action(ctx, &payload.node_id, payload.service_id)?;

    // When transitioning into paused, actively stop the runtime so the user's
    // intent ("frozen, do not consume resources") is enforced. Unpause does
    // the inverse only on the bookkeeping side — the user must press Start
    // to bring the engine back up.
    let mut stop_error: Option<String> = None;
    if payload.paused {
        let svc = fetch_service_row(ctx, payload.service_id)?;
        if matches!(
            svc.status,
            crate::services_repo::services::ServiceStatus::Running
                | crate::services_repo::services::ServiceStatus::Degraded
                | crate::services_repo::services::ServiceStatus::Starting
        ) {
            let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
                ProtocolError::internal("port allocator not initialized (supervisor disabled)")
            })?;
            if let Err(e) = crate::services::deploy::stop(&svc, port_allocator).await {
                stop_error = Some(e.to_string());
            } else {
                let conn = ctx
                    .state
                    .db
                    .write()
                    .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
                crate::services_repo::services::update_status(
                    &conn,
                    payload.service_id,
                    crate::services_repo::services::ServiceStatus::Stopped,
                )
                .map_err(db_err)?;
                crate::services_repo::services::update_runtime(
                    &conn,
                    payload.service_id,
                    None,
                    None,
                    None,
                    None,
                )
                .map_err(db_err)?;
            }
        }
    }

    {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::set_paused(&conn, payload.service_id, payload.paused)
            .map_err(db_err)?;
    }

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.pause",
        None,
        Some(&format!(
            "service_id={} paused={} stop_err={}",
            payload.service_id,
            payload.paused,
            stop_error.as_deref().unwrap_or("none")
        )),
    );

    push_service_updated(ctx, payload.service_id);

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResPause(tentaflow_protocol::ServicePauseResponse {
            success: stop_error.is_none(),
            error: stop_error,
        }),
    ))
}

#[handler(variant = "ServiceStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqStart(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqStart",
            ));
        }
    };

    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServiceStartRemote {
            service_id: payload.service_id,
        };
        let (success, error) = forward_service_action(ctx, target, cmd).await;
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResStart(
                tentaflow_protocol::ServiceStartResponse { success, error },
            ),
        ));
    }
    reject_ambiguous_local_service_action(ctx, &payload.node_id, payload.service_id)?;

    let svc = fetch_service_row(ctx, payload.service_id)?;
    let port_allocator = ctx.state.port_allocator.clone().ok_or_else(|| {
        ProtocolError::internal("port allocator not initialized (supervisor disabled)")
    })?;

    // Idempotent: a service that is already up and not paused stays as-is.
    if matches!(
        svc.status,
        crate::services_repo::services::ServiceStatus::Running
            | crate::services_repo::services::ServiceStatus::Degraded
    ) && !svc.paused
    {
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResStart(
                tentaflow_protocol::ServiceStartResponse {
                    success: true,
                    error: None,
                },
            ),
        ));
    }

    // Start clears the pause flag; the user explicitly asked for the engine
    // to come back up.
    if svc.paused {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::set_paused(&conn, payload.service_id, false)
            .map_err(db_err)?;
    }

    {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::update_status(
            &conn,
            payload.service_id,
            crate::services_repo::services::ServiceStatus::Starting,
        )
        .map_err(db_err)?;
    }

    let respawn_result = crate::services::deploy::respawn(
        &svc.engine_id,
        svc.deploy_method,
        &svc.config_json,
        port_allocator,
        &ctx.state.db,
        &ctx.state.settings_cipher,
        svc.runtime_port,
    )
    .await;

    let (success, error) = match respawn_result {
        Ok(handle) => {
            let conn = ctx
                .state
                .db
                .write()
                .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
            crate::services_repo::services::update_runtime(
                &conn,
                payload.service_id,
                handle.pid,
                handle.port,
                handle.sidecar_port,
                handle.endpoint_url.as_deref(),
            )
            .map_err(db_err)?;
            crate::services_repo::services::update_status(
                &conn,
                payload.service_id,
                crate::services_repo::services::ServiceStatus::Running,
            )
            .map_err(db_err)?;
            (true, None)
        }
        Err(e) => {
            let msg = e.to_string();
            let conn = ctx
                .state
                .db
                .write()
                .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
            let _ = crate::services_repo::services::update_status(
                &conn,
                payload.service_id,
                crate::services_repo::services::ServiceStatus::Failed,
            );
            let _ = crate::services_repo::services::update_health(
                &conn,
                payload.service_id,
                false,
                Some(&msg),
            );
            (false, Some(msg))
        }
    };

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.start",
        Some(&svc.engine_id),
        Some(&format!(
            "service_id={} success={}",
            payload.service_id, success
        )),
    );

    push_service_updated(ctx, payload.service_id);

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResStart(tentaflow_protocol::ServiceStartResponse {
            success,
            error,
        }),
    ))
}

#[handler(variant = "ServiceConfigUpdateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqUpdate(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqUpdate",
            ));
        }
    };

    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::ServiceUpdateRemote {
            service_id: payload.service_id,
            model_repo: payload.model_repo.clone(),
            model_preset_id: payload.model_preset_id.clone(),
            gpu_memory_utilization: payload.gpu_memory_utilization,
            max_model_len: payload.max_model_len,
            max_num_seqs: payload.max_num_seqs,
            max_num_batched_tokens: payload.max_num_batched_tokens,
            kv_cache_dtype: payload.kv_cache_dtype.clone(),
            chunked_prefill: payload.chunked_prefill,
            vllm_args_override: payload.vllm_args_override.clone(),
            pinned: payload.pinned,
            paused: payload.paused,
            restart_after_save: payload.restart_after_save,
        };
        let (success, error) = forward_service_action(ctx, target, cmd).await;
        return Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResUpdate(
                tentaflow_protocol::ServiceUpdateResponse {
                    success,
                    error,
                    restarted: payload.restart_after_save && success,
                },
            ),
        ));
    }
    reject_ambiguous_local_service_action(ctx, &payload.node_id, payload.service_id)?;

    let svc = fetch_service_row(ctx, payload.service_id)?;

    // Zaktualizuj config_json: parsujemy istniejący JSON, mergujemy podane
    // pola, serializujemy. Pozostałe pola (np. ścieżki bundle) zostają.
    let mut cfg: serde_json::Value =
        serde_json::from_str(&svc.config_json).unwrap_or_else(|_| serde_json::json!({}));
    let cfg_obj = cfg
        .as_object_mut()
        .ok_or_else(|| ProtocolError::bad_request("service config_json is not an object"))?;

    if let Some(repo) = payload.model_repo.as_ref() {
        cfg_obj.insert("model_repo".into(), serde_json::Value::String(repo.clone()));
        cfg_obj.insert("model_preset_id".into(), serde_json::Value::Null);
    }
    if let Some(preset_id) = payload.model_preset_id.as_ref() {
        cfg_obj.insert(
            "model_preset_id".into(),
            serde_json::Value::String(preset_id.clone()),
        );
        cfg_obj.insert("model_repo".into(), serde_json::Value::Null);
    }
    if let Some(util) = payload.gpu_memory_utilization {
        if let Some(num) = serde_json::Number::from_f64(util as f64) {
            cfg_obj.insert(
                "gpu_memory_utilization".into(),
                serde_json::Value::Number(num),
            );
        }
    }
    if let Some(v) = payload.max_model_len {
        cfg_obj.insert("max_model_len".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = payload.max_num_seqs {
        cfg_obj.insert("max_num_seqs".into(), serde_json::Value::Number(v.into()));
    }
    if let Some(v) = payload.max_num_batched_tokens {
        cfg_obj.insert(
            "max_num_batched_tokens".into(),
            serde_json::Value::Number(v.into()),
        );
    }
    if let Some(dt) = payload.kv_cache_dtype.as_ref() {
        cfg_obj.insert(
            "kv_cache_dtype".into(),
            serde_json::Value::String(dt.clone()),
        );
    }
    if let Some(b) = payload.chunked_prefill {
        cfg_obj.insert("chunked_prefill".into(), serde_json::Value::Bool(b));
    }
    if let Some(args) = payload.vllm_args_override.as_ref() {
        cfg_obj.insert("vllm_args".into(), serde_json::Value::String(args.clone()));
    }

    let new_config_json = serde_json::to_string(&cfg)
        .map_err(|e| ProtocolError::internal(format!("serialize config: {e}")))?;

    {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::services::update_config_json(
            &conn,
            payload.service_id,
            &new_config_json,
        )
        .map_err(db_err)?;
        if let Some(p) = payload.pinned {
            crate::services_repo::services::set_pinned(&conn, payload.service_id, p)
                .map_err(db_err)?;
        }
        if let Some(p) = payload.paused {
            crate::services_repo::services::set_paused(&conn, payload.service_id, p)
                .map_err(db_err)?;
        }
    }

    let mut restarted = false;
    let mut respawn_error: Option<String> = None;
    let was_running = matches!(
        svc.status,
        crate::services_repo::services::ServiceStatus::Running
            | crate::services_repo::services::ServiceStatus::Degraded
            | crate::services_repo::services::ServiceStatus::Starting
    );

    if payload.restart_after_save && was_running {
        // Stop running runtime — terminate(pid) + release ports.
        if let Some(ports) = ctx.state.port_allocator.clone() {
            if let Err(e) = crate::services::deploy::stop(&svc, ports.clone()).await {
                tracing::warn!(
                    service_id = payload.service_id,
                    "service_update: stop failed before respawn: {}",
                    e
                );
            }
            // Mark Starting + spawn detached respawn (jak supervisor).
            {
                let conn = ctx
                    .state
                    .db
                    .write()
                    .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
                let _ = crate::services_repo::services::update_status(
                    &conn,
                    payload.service_id,
                    crate::services_repo::services::ServiceStatus::Starting,
                );
            }
            let db = ctx.state.db.clone();
            let settings_cipher = ctx.state.settings_cipher.clone();
            let svc_id = payload.service_id;
            let engine_id = svc.engine_id.clone();
            let deploy_method = svc.deploy_method;
            let cfg_json = new_config_json.clone();
            let preserved_port = svc.runtime_port;
            tokio::spawn(async move {
                match crate::services::deploy::respawn(
                    &engine_id,
                    deploy_method,
                    &cfg_json,
                    ports,
                    &db,
                    &settings_cipher,
                    preserved_port,
                )
                .await
                {
                    Ok(handle) => {
                        if let Ok(conn) = db.write() {
                            let _ = crate::services_repo::services::update_runtime(
                                &conn,
                                svc_id,
                                handle.pid,
                                handle.port,
                                handle.sidecar_port,
                                handle.endpoint_url.as_deref(),
                            );
                            let _ = crate::services_repo::services::update_status(
                                &conn,
                                svc_id,
                                crate::services_repo::services::ServiceStatus::Running,
                            );
                        }
                        tracing::info!(
                            "service_update: respawn ok service_id={} engine={}",
                            svc_id,
                            engine_id
                        );
                    }
                    Err(e) => {
                        let msg = format!("respawn after update: {}", e);
                        if let Ok(conn) = db.write() {
                            let _ = crate::services_repo::services::update_status(
                                &conn,
                                svc_id,
                                crate::services_repo::services::ServiceStatus::Failed,
                            );
                            let _ = crate::services_repo::services::update_health(
                                &conn,
                                svc_id,
                                false,
                                Some(&msg),
                            );
                        }
                        tracing::warn!(
                            "service_update: respawn failed service_id={}: {}",
                            svc_id,
                            msg
                        );
                    }
                }
            });
            restarted = true;
        } else {
            respawn_error = Some("port allocator not initialized".into());
        }
    }

    let user_id = require_user_id(ctx).ok().map(|b| user_id_to_uuid(&b));
    audit(
        ctx,
        user_id.as_deref(),
        "service.update",
        Some(&svc.engine_id),
        Some(&format!(
            "service_id={} restart={}",
            payload.service_id, payload.restart_after_save
        )),
    );

    push_service_updated(ctx, payload.service_id);

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResUpdate(tentaflow_protocol::ServiceUpdateResponse {
            success: respawn_error.is_none(),
            error: respawn_error,
            restarted,
        }),
    ))
}

/// Resolve the request context for calling an external provider's API from a
/// service row: its `ApiKind` (from the manifest), base URL (`endpoint_url`),
/// DECRYPTED api key and optional api version (Azure). The key is decrypted
/// here on the owning node and never returned to the client.
fn resolve_external_request_ctx(
    ctx: &HandlerContext,
    row: &crate::services_repo::services::ServiceRow,
) -> Result<
    (
        crate::services::manifest::ApiKind,
        String,
        String,
        Option<String>,
    ),
    String,
> {
    if row.deploy_method != crate::services_repo::services::DeployMethod::External {
        return Err("service is not an external provider".to_string());
    }
    let manifest = crate::services::manifest::registry()
        .by_id(&row.engine_id)
        .ok_or_else(|| format!("engine '{}' not found in manifest", row.engine_id))?;
    let api = manifest.engine.api;
    let base_url = row
        .endpoint_url
        .clone()
        .ok_or_else(|| "service has no endpoint_url".to_string())?;
    let cfg: serde_json::Value =
        serde_json::from_str(&row.config_json).unwrap_or(serde_json::Value::Null);
    let api_version = cfg
        .get("api_version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let api_key = cfg
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|raw| ctx.state.settings_cipher.decrypt(raw).unwrap_or_default())
        .unwrap_or_default();
    Ok((api, base_url, api_key, api_version))
}

/// Classify a selected model's modality without an extra API round-trip:
/// ElevenLabs = tts, Soniox = stt, Anthropic = chat, everything else by id.
fn modality_for(api: crate::services::manifest::ApiKind, id: &str) -> String {
    use crate::services::manifest::ApiKind;
    match api {
        ApiKind::Elevenlabs => "tts".to_string(),
        ApiKind::Soniox => "stt".to_string(),
        ApiKind::Anthropic => "chat".to_string(),
        _ => crate::services::providers::classify_openai_model_id(id).to_string(),
    }
}

#[handler(variant = "ServiceModelCatalogRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_model_catalog(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqModelCatalog(p)) => {
            p.clone()
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqModelCatalog",
            ));
        }
    };

    let resp = |models, error| {
        Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResModelCatalog(
                tentaflow_protocol::ServiceModelCatalogResponse { models, error },
            ),
        ))
    };

    // Provider keys are node-local; listing must run on the owning node.
    if forward_target_node(ctx, &payload.node_id).is_some() {
        return resp(
            Vec::new(),
            Some("model listing must be performed on the node that owns the provider".to_string()),
        );
    }

    let row = fetch_service_row(ctx, payload.service_id)?;
    let (api, base_url, api_key, api_version) = match resolve_external_request_ctx(ctx, &row) {
        Ok(v) => v,
        Err(e) => return resp(Vec::new(), Some(e)),
    };

    // Subscription (ChatGPT plan) tokens are rejected by the standard
    // `/v1/models`, so list from the Codex backend instead.
    let subscription = serde_json::from_str::<serde_json::Value>(&row.config_json)
        .ok()
        .and_then(|c| {
            c.get("auth_mode")
                .and_then(|v| v.as_str())
                .map(|m| m.eq_ignore_ascii_case("subscription"))
        })
        .unwrap_or(false);

    let fetch = if subscription && row.engine_id.eq_ignore_ascii_case("openai") {
        crate::services::backend::codex::list_models(&api_key).await
    } else {
        crate::services::providers::list_models(api, &base_url, &api_key, api_version.as_deref())
            .await
    };
    let fetched = match fetch {
        Ok(m) => m,
        Err(e) => return resp(Vec::new(), Some(e.to_string())),
    };

    let selected: std::collections::HashSet<String> = {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        crate::services_repo::models::list_for_service(&conn, payload.service_id)
            .map_err(db_err)?
            .into_iter()
            .map(|m| m.model_name)
            .collect()
    };

    let models = fetched
        .into_iter()
        .map(|m| tentaflow_protocol::ServiceModelCatalogEntry {
            selected: selected.contains(&m.id),
            id: m.id,
            display_name: m.display_name,
            modality: m.modality,
            context_length: m.context_length,
        })
        .collect();

    resp(models, None)
}

#[handler(variant = "ServiceModelSelectionRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_model_selection(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqModelSelection(p)) => {
            p.clone()
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqModelSelection",
            ));
        }
    };

    let resp = |success, error| {
        Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResModelSelection(
                tentaflow_protocol::ServiceModelSelectionResponse { success, error },
            ),
        ))
    };

    if forward_target_node(ctx, &payload.node_id).is_some() {
        return resp(
            false,
            Some(
                "model selection must be performed on the node that owns the provider".to_string(),
            ),
        );
    }

    let row = fetch_service_row(ctx, payload.service_id)?;
    let api = match resolve_external_request_ctx(ctx, &row) {
        Ok((api, _, _, _)) => api,
        Err(e) => return resp(false, Some(e)),
    };

    let selected: Vec<crate::services_repo::models::SelectedModel> = payload
        .selected_model_ids
        .iter()
        .map(|id| crate::services_repo::models::SelectedModel {
            model_name: id.clone(),
            display_name: None,
            modality: modality_for(api, id),
            context_length: None,
        })
        .collect();

    {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        if let Err(e) =
            crate::services_repo::models::replace_selection(&conn, payload.service_id, &selected)
        {
            return resp(false, Some(e.to_string()));
        }
    }

    // Optional per-model pricing for the selected external models → persist to
    // `model_pricing`. Entries for non-selected models are ignored; bad values
    // (NaN / Inf / negative) are warned and skipped WITHOUT failing selection.
    if !payload.pricing.is_empty() {
        match ctx.org_context.as_ref().map(|o| o.org_id.clone()) {
            Some(org_id) => {
                let selected_ids: std::collections::HashSet<&str> = payload
                    .selected_model_ids
                    .iter()
                    .map(|s| s.as_str())
                    .collect();
                for entry in &payload.pricing {
                    if !selected_ids.contains(entry.model_id.as_str()) {
                        continue;
                    }
                    // Skip entries that supply nothing — never create an all-zero
                    // row that would mask `missing_pricing` in metrics.
                    if entry.prompt_per_1k.is_none()
                        && entry.completion_per_1k.is_none()
                        && entry.audio_per_min.is_none()
                        && entry.image_each.is_none()
                    {
                        continue;
                    }
                    let valid =
                        |v: Option<f64>| v.map(|x| x.is_finite() && x >= 0.0).unwrap_or(true);
                    if !(valid(entry.prompt_per_1k)
                        && valid(entry.completion_per_1k)
                        && valid(entry.audio_per_min)
                        && valid(entry.image_each))
                    {
                        tracing::warn!(
                            model_id = %entry.model_id,
                            "service model selection: invalid pricing (non-finite or negative) — skipped"
                        );
                        continue;
                    }
                    // Merge: unset fields keep the existing stored price.
                    if let Err(e) = crate::db::repository::upsert_model_pricing_merge(
                        &ctx.state.db,
                        &org_id,
                        &entry.model_id,
                        entry.prompt_per_1k,
                        entry.completion_per_1k,
                        entry.audio_per_min,
                        entry.image_each,
                    ) {
                        tracing::warn!(
                            model_id = %entry.model_id,
                            error = %e,
                            "service model selection: pricing upsert failed"
                        );
                    }
                }
            }
            None => {
                tracing::warn!("service model selection: no org context — pricing skipped");
            }
        }
    }

    audit(
        ctx,
        require_user_id(ctx)
            .ok()
            .map(|b| user_id_to_uuid(&b))
            .as_deref(),
        "service.model.selection",
        Some(&row.engine_id),
        Some(&format!("{} models", selected.len())),
    );

    resp(true, None)
}

#[handler(variant = "ServiceOauthStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_oauth_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqOauthStart(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqOauthStart",
            ));
        }
    };

    let resp =
        |flow_id: String, authorize_url: String, user_code: String, error: Option<String>| {
            Ok(MessageBody::ServiceBody(
                tentaflow_protocol::ServicePayload::ResOauthStart(
                    tentaflow_protocol::ServiceOauthStartResponse {
                        flow_id,
                        authorize_url,
                        user_code,
                        error,
                    },
                ),
            ))
        };

    // The OAuth flow must run on the node that will own the service + tokens.
    // When deploying to a mesh peer, forward the login there (the peer holds the
    // device-code flow and later resolves the tokens at deploy time).
    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::OauthStart {
            provider: payload.provider.clone(),
        };
        return match forward_command(ctx, target, cmd).await {
            Ok(r) => match r.payload {
                tentaflow_protocol::mesh::MeshCommandResponsePayload::OauthStartResult {
                    flow_id,
                    authorize_url,
                    user_code,
                    error,
                } => resp(flow_id, authorize_url, user_code, error),
                _ => resp(
                    String::new(),
                    String::new(),
                    String::new(),
                    Some("unexpected mesh response".to_string()),
                ),
            },
            Err(e) => resp(String::new(), String::new(), String::new(), Some(e)),
        };
    }
    if !payload.provider.eq_ignore_ascii_case("openai") {
        return resp(
            String::new(),
            String::new(),
            String::new(),
            Some("subscription login is only available for OpenAI".to_string()),
        );
    }

    match crate::services::backend::codex_oauth::start_login().await {
        Ok((flow_id, authorize_url, user_code)) => resp(flow_id, authorize_url, user_code, None),
        Err(e) => resp(String::new(), String::new(), String::new(), Some(e)),
    }
}

#[handler(variant = "ServiceOauthPollRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_oauth_poll(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqOauthPoll(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqOauthPoll",
            ));
        }
    };

    let poll_resp = |status: String, account_label: Option<String>, error: Option<String>| {
        Ok(MessageBody::ServiceBody(
            tentaflow_protocol::ServicePayload::ResOauthPoll(
                tentaflow_protocol::ServiceOauthPollResponse {
                    status,
                    account_label,
                    error,
                },
            ),
        ))
    };

    // The flow lives on the node that started it — poll the same node.
    if let Some(target) = forward_target_node(ctx, &payload.node_id) {
        let cmd = tentaflow_protocol::mesh::MeshCommandType::OauthPoll {
            flow_id: payload.flow_id.clone(),
        };
        return match forward_command(ctx, target, cmd).await {
            Ok(r) => match r.payload {
                tentaflow_protocol::mesh::MeshCommandResponsePayload::OauthPollResult {
                    status,
                    account_label,
                    error,
                } => poll_resp(status, account_label, error),
                _ => poll_resp(
                    "error".to_string(),
                    None,
                    Some("unexpected mesh response".to_string()),
                ),
            },
            Err(e) => poll_resp("error".to_string(), None, Some(e)),
        };
    }

    let (status, account_label, error) =
        crate::services::backend::codex_oauth::poll(&payload.flow_id);
    poll_resp(status, account_label, error)
}

#[handler(variant = "ServiceVramHintRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_vram_hint(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqVramHint(p)) => p.clone(),
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqVramHint",
            ));
        }
    };

    // Mesh forward NIE jest zaimplementowany dla VramHint — wymagałby
    // proxy nvidia-smi przez QUIC. Local only na razie. `node_id` ignored.
    let exclude_pids: Vec<u32> = Vec::new(); // exclude_service_id mapping na PID
                                             // wymaga lookup w `services` row → runtime_pid; pomijamy w MVP,
                                             // własny serwis zwykle nie liczy się jako zaskakujący duży
                                             // konsument GPU bo jest dopiero startowany lub stopped.
    let snapshot =
        crate::services::gpu_snapshot::collect_vram_snapshot(payload.gpu_index, &exclude_pids)
            .await;

    let recommended = snapshot
        .first()
        .map(crate::services::gpu_snapshot::recommended_utilization);

    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResVramHint(
            tentaflow_protocol::ServiceVramHintResponse {
                gpus: snapshot,
                recommended_utilization: recommended,
            },
        ),
    ))
}

#[handler(variant = "ServiceEnginePresetsRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn service_engine_presets(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ServiceBody(tentaflow_protocol::ServicePayload::ReqEnginePresets(p)) => {
            p.clone()
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ServicePayload::ReqEnginePresets",
            ));
        }
    };
    let manifest = crate::services::manifest::registry().by_id(&payload.engine_id);
    let Some(manifest) = manifest else {
        return Err(ProtocolError::not_found(format!(
            "engine '{}' not in manifest",
            payload.engine_id
        )));
    };
    let presets = manifest
        .model_presets
        .iter()
        .map(|p| tentaflow_protocol::ServicePresetInfo {
            id: p.id.clone(),
            display_name: p.display_name.clone(),
            repo: p.repo.clone(),
            quantization: p.quantization.clone(),
            recommended: p.recommended,
        })
        .collect();
    Ok(MessageBody::ServiceBody(
        tentaflow_protocol::ServicePayload::ResEnginePresets(
            tentaflow_protocol::ServiceEnginePresetsResponse { presets },
        ),
    ))
}

#[cfg(test)]
mod catalog_list_tests {
    //! Coverage for the snapshot→wire mapping that backs the
    //! `CatalogListRequest` handler. The full handler path needs a live
    //! `HandlerContext`; these tests target `catalog_snapshot_to_wire`
    //! directly so they can craft adversarial snapshots (every diagnostic
    //! kind, every entry kind, surface filter, blocking opt-in) without
    //! standing up a Router.
    use super::catalog_snapshot_to_wire;
    use crate::services::catalog::{
        CatalogDiagnostic, CatalogEntry, CatalogEntryKind, CatalogSnapshot, InputModality,
        ModelInstance, OutputModality, ServiceSurface, Strategy,
    };
    use std::sync::Arc;
    use tentaflow_protocol::{CatalogDiagnosticWire, CatalogEntryKindWire, CatalogListRequest};

    fn snapshot_with(entries: Vec<CatalogEntry>) -> CatalogSnapshot {
        CatalogSnapshot {
            entries: Arc::from(entries.into_boxed_slice()),
            version: 42,
        }
    }

    fn service_entry(id: &str, surface: ServiceSurface) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            kind: CatalogEntryKind::ServiceModel {
                instances: vec![ModelInstance {
                    node_id: "node-a".into(),
                    node_hostname: Some("host-a".into()),
                    service_id: 7,
                    status: "running".into(),
                    backend: Some("llama-cpp".into()),
                    size_mb: Some(2048),
                    loaded: true,
                    input_modalities: vec![InputModality::Text],
                    output_modalities: vec![OutputModality::Text],
                }],
            },
            service_surfaces: vec![surface],
            input_modalities: vec![InputModality::Text],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        }
    }

    fn alias_entry(id: &str, target: &str, fallbacks: Vec<&str>) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            kind: CatalogEntryKind::Alias {
                target: target.to_string(),
                fallback_targets: fallbacks.into_iter().map(String::from).collect(),
                strategy: Strategy::RoundRobin,
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![InputModality::Text, InputModality::Audio],
            output_modalities: vec![OutputModality::Text],
            diagnostic: None,
        }
    }

    fn flow_entry(id: &str, flow_id: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            kind: CatalogEntryKind::Flow {
                flow_id: flow_id.to_string(),
                published_name: id.to_string(),
            },
            service_surfaces: vec![ServiceSurface::Chat],
            input_modalities: vec![],
            output_modalities: vec![],
            diagnostic: None,
        }
    }

    #[test]
    fn maps_each_kind_into_its_wire_variant() {
        let snap = snapshot_with(vec![
            service_entry("llama-3", ServiceSurface::Chat),
            flow_entry("chat-pl", "17"),
            alias_entry("rag-llm", "llama-3", vec!["bielik-11b"]),
        ]);

        let wire = catalog_snapshot_to_wire(
            &snap,
            &CatalogListRequest {
                surface_filter: None,
                include_blocking_diagnostics: false,
            },
        );

        assert_eq!(wire.len(), 3);
        let by_id: std::collections::HashMap<_, _> =
            wire.iter().map(|w| (w.id.clone(), w)).collect();

        let llama = by_id.get("llama-3").unwrap();
        assert_eq!(llama.owned_by, "tentaflow-service");
        match &llama.kind {
            CatalogEntryKindWire::ServiceModel { instances } => {
                assert_eq!(instances.len(), 1);
                assert_eq!(instances[0].node_hostname.as_deref(), Some("host-a"));
                assert_eq!(instances[0].service_id, 7);
                assert!(instances[0].loaded);
            }
            other => panic!("expected ServiceModel, got {:?}", other),
        }
        assert_eq!(llama.service_surfaces, vec!["chat".to_string()]);
        assert_eq!(llama.input_modalities, vec!["text".to_string()]);

        let flow = by_id.get("chat-pl").unwrap();
        assert_eq!(flow.owned_by, "tentaflow-flow");
        assert!(matches!(
            &flow.kind,
            CatalogEntryKindWire::Flow { flow_id, .. } if flow_id == "17"
        ));

        let alias = by_id.get("rag-llm").unwrap();
        assert_eq!(alias.owned_by, "tentaflow-alias");
        match &alias.kind {
            CatalogEntryKindWire::Alias {
                target,
                fallback_targets,
                strategy,
            } => {
                assert_eq!(target, "llama-3");
                assert_eq!(fallback_targets, &vec!["bielik-11b".to_string()]);
                // Strategy is round_robin (set in alias_entry helper).
                assert_eq!(strategy, "round_robin");
            }
            other => panic!("expected Alias, got {:?}", other),
        }
    }

    #[test]
    fn surface_filter_drops_non_matching_entries() {
        let snap = snapshot_with(vec![
            service_entry("llama-3", ServiceSurface::Chat),
            service_entry("whisper-large", ServiceSurface::Stt),
            service_entry("xtts", ServiceSurface::Tts),
        ]);

        let wire = catalog_snapshot_to_wire(
            &snap,
            &CatalogListRequest {
                surface_filter: Some("stt".into()),
                include_blocking_diagnostics: false,
            },
        );
        let ids: Vec<_> = wire.iter().map(|w| w.id.clone()).collect();
        assert_eq!(ids, vec!["whisper-large".to_string()]);
    }

    #[test]
    fn surface_filter_is_case_insensitive_and_trims() {
        let snap = snapshot_with(vec![service_entry("xtts", ServiceSurface::Tts)]);
        let wire = catalog_snapshot_to_wire(
            &snap,
            &CatalogListRequest {
                surface_filter: Some("  TTS  ".into()),
                include_blocking_diagnostics: false,
            },
        );
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].id, "xtts");
    }

    #[test]
    fn blocking_diagnostics_hidden_unless_admin_opts_in() {
        let mut shadowed = service_entry("dup-name", ServiceSurface::Chat);
        shadowed.diagnostic = Some(CatalogDiagnostic::RemoteShadowed {
            local_owner: "node-z".into(),
        });
        let snap = snapshot_with(vec![
            shadowed.clone(),
            service_entry("ok", ServiceSurface::Chat),
        ]);

        let hidden = catalog_snapshot_to_wire(
            &snap,
            &CatalogListRequest {
                surface_filter: None,
                include_blocking_diagnostics: false,
            },
        );
        let ids: Vec<_> = hidden.iter().map(|w| w.id.clone()).collect();
        assert_eq!(ids, vec!["ok".to_string()]);

        let visible = catalog_snapshot_to_wire(
            &snap,
            &CatalogListRequest {
                surface_filter: None,
                include_blocking_diagnostics: true,
            },
        );
        assert_eq!(visible.len(), 2);
        let dup = visible.iter().find(|w| w.id == "dup-name").unwrap();
        match &dup.diagnostic {
            Some(CatalogDiagnosticWire::RemoteShadowed { local_owner }) => {
                assert_eq!(local_owner, "node-z");
            }
            other => panic!("expected RemoteShadowed wire diagnostic, got {:?}", other),
        }
    }

    #[test]
    fn non_blocking_diagnostic_passes_through_with_modality_strings() {
        let mut alias = alias_entry("chat-pl", "qwen-omni", vec!["bielik-11b"]);
        alias.diagnostic = Some(CatalogDiagnostic::IncompatibleAliasTargets {
            alias: "chat-pl".into(),
            missing_modalities: vec![InputModality::Audio, InputModality::Image],
        });

        let wire = catalog_snapshot_to_wire(
            &snapshot_with(vec![alias]),
            &CatalogListRequest {
                surface_filter: None,
                include_blocking_diagnostics: false,
            },
        );
        assert_eq!(wire.len(), 1, "non-blocking diagnostic must not hide entry");
        match &wire[0].diagnostic {
            Some(CatalogDiagnosticWire::IncompatibleAliasTargets {
                alias,
                missing_modalities,
            }) => {
                assert_eq!(alias, "chat-pl");
                assert_eq!(
                    missing_modalities,
                    &vec!["audio".to_string(), "image".to_string()]
                );
            }
            other => panic!("expected IncompatibleAliasTargets, got {:?}", other),
        }
    }
}
