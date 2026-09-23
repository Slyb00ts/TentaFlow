// =============================================================================
// File: tests/api_key_scope_dispatch.rs
// Purpose: Handler-level coverage for plaster 5 — API key creation with explicit
//          scopes (general keys), scope list/set/clear, rotation, admin-only
//          policy and audit logging. Drives the binary dispatch with an Admin
//          session and asserts wire responses plus reconciled DB state.
// =============================================================================


use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::db::models::AuditLogFilters;
use tentaflow_core::db::repository;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::{dispatch, HandlerContext};
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{ApiKeyCreateRequest, MessageBody, ResourceRef, SessionAuth};

/// Admin session backed by a real, active account: the dispatcher refuses a
/// session whose account does not exist ("account unavailable") before any
/// handler runs, which would make every assertion below vacuous.
fn admin_ctx(state: Arc<AppState>) -> HandlerContext {
    let id =
        repository::create_user_account(&state.db, "scope-admin", "not-a-login-hash", "Admin", "")
            .expect("admin account");
    state
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET must_change_password = 0, is_active = 1 WHERE id = ?1",
            [&id],
        )
        .expect("activate admin account");
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: *uuid::Uuid::parse_str(&id).unwrap().as_bytes(),
            role: Some("admin".to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: "user-admin".to_string(),
            org_id: "org-test".to_string(),
            role_id: "role-admin".to_string(),
            permissions: HashSet::new(),
        }),
    }
}

fn non_admin_ctx(state: Arc<AppState>) -> HandlerContext {
    let id =
        repository::create_user_account(&state.db, "scope-power", "not-a-login-hash", "Power", "")
            .expect("power-user account");
    state
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET must_change_password = 0, is_active = 1 WHERE id = ?1",
            [&id],
        )
        .expect("activate power-user account");
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: *uuid::Uuid::parse_str(&id).unwrap().as_bytes(),
            role: Some("power_user".to_string()),
        },
        correlation_id: 2,
        connection_id: 0,
        resume_secret: None,
        state,
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: None,
    }
}

async fn create_general_key(ctx: &HandlerContext, name: &str, scopes: Vec<ResourceRef>) -> String {
    let req = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name: name.to_string(),
        key_type: "general".to_string(),
        subject_id: None,
        scope_resources: scopes,
    });
    let (resp, is_err) = dispatch(&req, ctx).await;
    assert!(!is_err, "create general key should succeed: {:?}", resp);
    match resp {
        MessageBody::ApiKeyCreateResponseBody(r) => {
            assert!(r.token.starts_with("sk-"));
            r.key_id
        }
        other => panic!("expected ApiKeyCreateResponse, got {:?}", other),
    }
}

#[tokio::test]
async fn create_general_persists_scope_resources() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());

    let uid = create_general_key(
        &ctx,
        "svc-general",
        vec![
            ResourceRef {
                resource_type: "model".into(),
                resource_id: "gpt-4o".into(),
                action: None,
            },
            ResourceRef {
                resource_type: "flow".into(),
                resource_id: "flow-123".into(),
                action: None,
            },
        ],
    )
    .await;

    // The seeded scopes must be persisted as resource_permissions for the key.
    let rows = repository::resource_permissions::list_for_subject(&state.db, "api_key", &uid)
        .expect("list scopes");
    assert_eq!(rows.len(), 2, "two seeded scopes expected");
    assert!(rows.iter().all(|r| r.subject_type == "api_key"));
    assert!(rows
        .iter()
        .all(|r| r.subject_id == uid && r.access_level == "allow"));

    // scope_list RPC returns the same entries.
    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeListRequest {
            key_uid: uid.clone(),
        },
        &ctx,
    )
    .await;
    assert!(!is_err);
    match resp {
        MessageBody::ApiKeyScopeListResponse { entries } => assert_eq!(entries.len(), 2),
        other => panic!("expected ApiKeyScopeListResponse, got {:?}", other),
    }
}

#[tokio::test]
async fn scope_set_and_clear_round_trip() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let uid = create_general_key(&ctx, "svc-scope", vec![]).await;

    // set adds an allow scope.
    let (_resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeSetRequest {
            key_uid: uid.clone(),
            resource_type: "alias".into(),
            resource_id: "fast".into(),
            access_level: "allow".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(!is_err);
    assert_eq!(
        repository::resource_permissions::count_for_subject(&state.db, "api_key", &uid).unwrap(),
        1
    );

    // clear removes it.
    let (_resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeClearRequest {
            key_uid: uid.clone(),
            resource_type: "alias".into(),
            resource_id: "fast".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(!is_err);
    assert_eq!(
        repository::resource_permissions::count_for_subject(&state.db, "api_key", &uid).unwrap(),
        0
    );
}

#[tokio::test]
async fn rotate_changes_verifier_and_invalidates_old_token() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let uid = create_general_key(&ctx, "svc-rotate", vec![]).await;

    let before = repository::get_api_key_by_uid(&state.db, &uid)
        .unwrap()
        .unwrap()
        .key_verifier;

    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyRotateRequest {
            key_uid: uid.clone(),
        },
        &ctx,
    )
    .await;
    assert!(!is_err);
    let new_token = match resp {
        MessageBody::ApiKeyRotateResponse { token } => token,
        other => panic!("expected ApiKeyRotateResponse, got {:?}", other),
    };
    assert!(new_token.starts_with("sk-"));

    let after = repository::get_api_key_by_uid(&state.db, &uid)
        .unwrap()
        .unwrap()
        .key_verifier;
    assert_ne!(before, after, "rotation must change the stored verifier");

    // The old verifier no longer matches any active key.
    let old_match = repository::verify_api_key(&state.db, &before).unwrap();
    assert!(old_match.is_none(), "old token must no longer verify");
    // The new token's verifier resolves to this key.
    let pepper =
        repository::get_or_create_api_key_pepper(&state.db, &state.settings_cipher).unwrap();
    let new_verifier = tentaflow_core::api::dashboard::auth::api_key_verifier(&new_token, &pepper);
    let new_match = repository::verify_api_key(&state.db, &new_verifier).unwrap();
    assert_eq!(new_match.unwrap().uid, uid);
}

#[tokio::test]
async fn create_and_scope_are_audited() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let uid = create_general_key(&ctx, "svc-audit", vec![]).await;

    let _ = dispatch(
        &MessageBody::ApiKeyScopeSetRequest {
            key_uid: uid.clone(),
            resource_type: "model".into(),
            resource_id: "gpt-4o".into(),
            access_level: "allow".into(),
            action: None,
        },
        &ctx,
    )
    .await;

    let create_logs = repository::list_audit_logs(
        &state.db,
        &AuditLogFilters {
            action: Some("apikey.create".to_string()),
            ..Default::default()
        },
        0,
        100,
    )
    .unwrap();
    assert!(
        !create_logs.is_empty(),
        "apikey.create must produce an audit entry"
    );

    let scope_logs = repository::list_audit_logs(
        &state.db,
        &AuditLogFilters {
            action: Some("apikey.scope.set".to_string()),
            ..Default::default()
        },
        0,
        100,
    )
    .unwrap();
    assert!(
        !scope_logs.is_empty(),
        "apikey.scope.set must produce an audit entry"
    );
}

#[tokio::test]
async fn scope_set_rejects_invalid_resource() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let uid = create_general_key(&ctx, "svc-badset", vec![]).await;

    // Garbage resource_type is rejected before any write.
    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeSetRequest {
            key_uid: uid.clone(),
            resource_type: "garbage".into(),
            resource_id: "x".into(),
            access_level: "allow".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(is_err, "invalid resource_type must be rejected");
    assert!(matches!(resp, MessageBody::Error(_)));

    // Empty resource_id is rejected too.
    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeSetRequest {
            key_uid: uid.clone(),
            resource_type: "model".into(),
            resource_id: "".into(),
            access_level: "allow".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(is_err, "empty resource_id must be rejected");
    assert!(matches!(resp, MessageBody::Error(_)));

    // Nothing was written despite the two bad requests.
    assert_eq!(
        repository::resource_permissions::count_for_subject(&state.db, "api_key", &uid).unwrap(),
        0
    );
}

#[tokio::test]
async fn scope_clear_rejects_invalid_resource() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let uid = create_general_key(&ctx, "svc-badclear", vec![]).await;

    // Clear must validate too — otherwise it would emit a garbage tombstone.
    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeClearRequest {
            key_uid: uid.clone(),
            resource_type: "garbage".into(),
            resource_id: "x".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(is_err, "invalid resource_type on clear must be rejected");
    assert!(matches!(resp, MessageBody::Error(_)));

    let (resp, is_err) = dispatch(
        &MessageBody::ApiKeyScopeClearRequest {
            key_uid: uid.clone(),
            resource_type: "model".into(),
            resource_id: "".into(),
            action: None,
        },
        &ctx,
    )
    .await;
    assert!(is_err, "empty resource_id on clear must be rejected");
    assert!(matches!(resp, MessageBody::Error(_)));
}

#[tokio::test]
async fn create_with_invalid_scope_creates_no_key() {
    // A bad scope in the create payload is rejected up front and must not leave a
    // half-created key behind (validation precedes the atomic create).
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let before = repository::list_api_keys(&state.db).unwrap().len();

    let req = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name: "bad-scope".into(),
        key_type: "general".into(),
        subject_id: None,
        scope_resources: vec![ResourceRef {
            resource_type: "garbage".into(),
            resource_id: "x".into(),
            action: None,
        }],
    });
    let (resp, is_err) = dispatch(&req, &ctx).await;
    assert!(is_err, "create with a bad scope must fail");
    assert!(
        matches!(&resp, MessageBody::Error(e) if e.code == tentaflow_protocol::ProtocolErrorCode::BadRequest),
        "refused by scope validation, not by an earlier gate: {resp:?}"
    );
    assert_eq!(
        repository::list_api_keys(&state.db).unwrap().len(),
        before,
        "no key may be created when a scope is invalid"
    );
}

#[tokio::test]
async fn non_admin_is_policy_denied_on_create() {
    let state = AppState::for_test();
    let ctx = non_admin_ctx(state.clone());
    let req = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name: "nope".into(),
        key_type: "general".into(),
        subject_id: None,
        scope_resources: vec![],
    });
    let (resp, is_err) = dispatch(&req, &ctx).await;
    assert!(is_err);
    assert!(
        matches!(&resp, MessageBody::Error(e) if e.code == tentaflow_protocol::ProtocolErrorCode::PolicyDenied),
        "refused by the Admin policy: {resp:?}"
    );
}

#[tokio::test]
async fn user_key_requires_existing_active_subject() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let req = MessageBody::ApiKeyCreateRequestBody(ApiKeyCreateRequest {
        name: "ghost".into(),
        key_type: "user".into(),
        subject_id: Some(uuid::Uuid::new_v4().to_string()),
        scope_resources: vec![],
    });
    let (resp, is_err) = dispatch(&req, &ctx).await;
    assert!(is_err, "user key with non-existent subject must fail");
    assert!(
        matches!(&resp, MessageBody::Error(e) if e.code == tentaflow_protocol::ProtocolErrorCode::NotFound),
        "refused because the subject does not exist: {resp:?}"
    );
}
