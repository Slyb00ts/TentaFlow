// ============ File: tests/role_catalog_dispatch_test.rs ============
//
// Pelne pokrycie binarnych RPC dispatcha katalogu rol
// (`dispatch::role_catalog::role_catalog_dispatch`).
//
// Testy wywoluja realny dispatch wokol `services::role_catalog::repo`
// (zadne mocki) na DB stworzonej przez `AppState::for_test()`. Migracje v40
// + v41 sa wykonywane przez `db::init`, wiec po starcie testu mamy 14
// seedowanych rol w organizacji `org-default` plus 2 aktywne locale (pl, en).

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_core::dispatch::role_catalog::role_catalog_dispatch;
use tentaflow_core::dispatch::state::AppState;
use tentaflow_core::dispatch::HandlerContext;
use tentaflow_core::services::rbac::OrgContext;
use tentaflow_protocol::{
    MessageBody, PlatformLocaleSummary, ProtocolErrorCode, RoleCatalogCreateRequest,
    RoleCatalogDetail, RoleCatalogListFilter, RoleCatalogPayload, RoleCatalogUpdateRequest,
    SessionAuth,
};

const ORG_DEFAULT: &str = "org-default";

// =============================================================================
// Fixtures
// =============================================================================

fn make_ctx(state: Arc<AppState>, role: &str) -> HandlerContext {
    let mut user_id_bytes = [0u8; 16];
    user_id_bytes[0] = 0xFF;
    let user_le = 7i64.to_le_bytes();
    user_id_bytes[8..].copy_from_slice(&user_le);
    HandlerContext {
        session: SessionAuth::UserSession {
            user_id: user_id_bytes,
            role: Some(role.to_string()),
        },
        correlation_id: 1,
        connection_id: 0,
        resume_secret: None,
        state,
        origin: tentaflow_core::dispatch::RequestOrigin::Local,
        org_context: Some(OrgContext {
            user_id: "user-dispatch-test".to_string(),
            org_id: ORG_DEFAULT.to_string(),
            role_id: "role-test".to_string(),
            permissions: HashSet::new(),
        }),
    }
}

fn admin_ctx(state: Arc<AppState>) -> HandlerContext {
    make_ctx(state, "admin")
}

fn user_ctx(state: Arc<AppState>) -> HandlerContext {
    make_ctx(state, "user")
}

async fn run(ctx: &HandlerContext, body: MessageBody) -> MessageBody {
    role_catalog_dispatch(&body, ctx)
        .await
        .unwrap_or_else(|e| MessageBody::Error(e))
}

fn pair(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

fn pl_en(pl: &str, en: &str) -> Vec<(String, String)> {
    vec![pair("pl", pl), pair("en", en)]
}

fn expect_list(body: MessageBody) -> Vec<tentaflow_protocol::RoleCatalogSummary> {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::ListResponse { roles }) => roles,
        other => panic!("expected ListResponse, got {:?}", other),
    }
}

fn expect_get(body: MessageBody) -> Option<RoleCatalogDetail> {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::GetResponse { role }) => role,
        other => panic!("expected GetResponse, got {:?}", other),
    }
}

fn expect_locales(body: MessageBody) -> Vec<PlatformLocaleSummary> {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::ListLocalesResponse { locales }) => {
            locales
        }
        other => panic!("expected ListLocalesResponse, got {:?}", other),
    }
}

fn expect_create(body: MessageBody) -> RoleCatalogDetail {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateResponse(detail)) => detail,
        other => panic!("expected CreateResponse, got {:?}", other),
    }
}

fn expect_update(body: MessageBody) -> RoleCatalogDetail {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::UpdateResponse(detail)) => detail,
        other => panic!("expected UpdateResponse, got {:?}", other),
    }
}

fn expect_deactivated(body: MessageBody) -> bool {
    match body {
        MessageBody::RoleCatalogBody(RoleCatalogPayload::DeactivateResponse { deactivated }) => {
            deactivated
        }
        other => panic!("expected DeactivateResponse, got {:?}", other),
    }
}

fn expect_error(body: MessageBody) -> tentaflow_protocol::ProtocolError {
    match body {
        MessageBody::Error(e) => e,
        other => panic!("expected Error, got {:?}", other),
    }
}

// =============================================================================
// Read API
// =============================================================================

#[tokio::test]
async fn test_dispatch_role_catalog_list() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let body = MessageBody::RoleCatalogBody(RoleCatalogPayload::ListRequest(
        RoleCatalogListFilter::default(),
    ));
    let resp = run(&ctx, body).await;
    let roles = expect_list(resp);
    assert_eq!(roles.len(), 14, "v41 seed must produce 14 roles");
}

#[tokio::test]
async fn test_dispatch_role_catalog_list_filter_by_kind_sales() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let body =
        MessageBody::RoleCatalogBody(RoleCatalogPayload::ListRequest(RoleCatalogListFilter {
            kind: Some("sales".to_string()),
            ..Default::default()
        }));
    let resp = run(&ctx, body).await;
    let roles = expect_list(resp);
    assert_eq!(roles.len(), 3, "seed has 3 sales roles");
    assert!(roles.iter().all(|r| r.kind == "sales"));
}

#[tokio::test]
async fn test_dispatch_role_catalog_list_filter_inactive_initially_empty() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let body =
        MessageBody::RoleCatalogBody(RoleCatalogPayload::ListRequest(RoleCatalogListFilter {
            is_active: Some(false),
            ..Default::default()
        }));
    let resp = run(&ctx, body).await;
    let roles = expect_list(resp);
    assert!(
        roles.is_empty(),
        "seed v41 inserts every role as active, so is_active=false must return zero rows"
    );
}

#[tokio::test]
async fn test_dispatch_role_catalog_get_by_id() {
    let state = AppState::for_test();
    let ctx = user_ctx(state.clone());

    let list = expect_list(
        run(
            &ctx,
            MessageBody::RoleCatalogBody(RoleCatalogPayload::ListRequest(
                RoleCatalogListFilter::default(),
            )),
        )
        .await,
    );
    let h1 = list
        .iter()
        .find(|r| r.slug == "handlowiec_l1")
        .expect("seed handlowiec_l1");
    let id = h1.id.clone();

    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::GetRequest { id: id.clone() }),
    )
    .await;
    let detail = expect_get(resp).expect("Some(detail)");
    assert_eq!(detail.id, id);
    assert_eq!(detail.slug, "handlowiec_l1");
    assert_eq!(detail.name_translations.len(), 2);
    let codes: HashSet<&str> = detail
        .name_translations
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    assert!(codes.contains("pl") && codes.contains("en"));
}

#[tokio::test]
async fn test_dispatch_role_catalog_get_by_slug() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::GetBySlugRequest {
            slug: "handlowiec_l1".to_string(),
        }),
    )
    .await;
    let detail = expect_get(resp).expect("Some(detail)");
    assert_eq!(detail.slug, "handlowiec_l1");
}

#[tokio::test]
async fn test_dispatch_role_catalog_get_nonexistent_returns_none() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::GetRequest {
            id: "does-not-exist".to_string(),
        }),
    )
    .await;
    assert!(expect_get(resp).is_none());
}

#[tokio::test]
async fn test_dispatch_role_catalog_list_locales() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::ListLocalesRequest),
    )
    .await;
    let locales = expect_locales(resp);
    assert_eq!(locales.len(), 2, "seed has pl + en");
    // pl is_default, sortuje sie pierwszy (ORDER BY is_default DESC).
    assert_eq!(locales[0].code, "pl");
    assert!(locales[0].is_default);
    assert_eq!(locales[0].display_name, "Polski");
    assert_eq!(locales[1].code, "en");
    assert!(!locales[1].is_default);
    assert_eq!(locales[1].display_name, "English");
}

// =============================================================================
// Create
// =============================================================================

fn fresh_create_request(slug: &str) -> RoleCatalogCreateRequest {
    RoleCatalogCreateRequest {
        slug: slug.to_string(),
        kind: "other".to_string(),
        name_translations: pl_en("Testowa rola", "Test role"),
        description_translations: Vec::new(),
        icon: None,
        color_hint: None,
        is_manager: false,
        default_visibility_scope: "assigned".to_string(),
    }
}

#[tokio::test]
async fn test_dispatch_role_catalog_create_admin_success() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let req = fresh_create_request("nowa_rola");
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let detail = expect_create(resp);
    assert_eq!(detail.slug, "nowa_rola");
    assert_eq!(detail.kind, "other");
    assert_eq!(detail.default_visibility_scope, "assigned");
    assert!(detail.is_active);
    assert_eq!(detail.created_by.as_deref(), Some("user-dispatch-test"));
}

#[tokio::test]
async fn test_dispatch_role_catalog_create_non_admin_denied() {
    let state = AppState::for_test();
    let ctx = user_ctx(state);
    let req = fresh_create_request("nowa_rola_denied");
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    assert!(
        err.message.contains("admin required"),
        "message should mention admin: {}",
        err.message
    );
}

#[tokio::test]
async fn test_dispatch_role_catalog_create_invalid_kind() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let mut req = fresh_create_request("nowa_rola_kind");
    req.kind = "wrong_kind".to_string();
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert!(
        err.message.to_lowercase().contains("kind"),
        "message should mention 'kind': {}",
        err.message
    );
}

#[tokio::test]
async fn test_dispatch_role_catalog_create_missing_translation() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let mut req = fresh_create_request("nowa_rola_missing");
    req.name_translations = vec![pair("pl", "Tylko PL")]; // brakuje en
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert!(
        err.message.contains("en"),
        "message should mention missing locale 'en': {}",
        err.message
    );
}

#[tokio::test]
async fn test_dispatch_role_catalog_create_duplicate_slug() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let mut req = fresh_create_request("handlowiec_l1");
    req.name_translations = pl_en("Duplikat", "Duplicate");
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::Conflict);
    assert!(err.message.contains("handlowiec_l1"));
}

// =============================================================================
// Update
// =============================================================================

async fn fetch_role_id_by_slug(ctx: &HandlerContext, slug: &str) -> String {
    let resp = run(
        ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::GetBySlugRequest {
            slug: slug.to_string(),
        }),
    )
    .await;
    expect_get(resp).expect("role exists").id
}

#[tokio::test]
async fn test_dispatch_role_catalog_update_admin_success() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let id = fetch_role_id_by_slug(&ctx, "handlowiec_l2").await;
    let req = RoleCatalogUpdateRequest {
        id: id.clone(),
        is_manager: Some(true),
        ..Default::default()
    };
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::UpdateRequest(req)),
    )
    .await;
    let detail = expect_update(resp);
    assert_eq!(detail.id, id);
    assert!(detail.is_manager);
}

#[tokio::test]
async fn test_dispatch_role_catalog_update_non_admin_denied() {
    let state = AppState::for_test();
    let admin = admin_ctx(state.clone());
    let id = fetch_role_id_by_slug(&admin, "handlowiec_l1").await;

    let ctx = user_ctx(state);
    let req = RoleCatalogUpdateRequest {
        id,
        is_manager: Some(true),
        ..Default::default()
    };
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::UpdateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
}

#[tokio::test]
async fn test_dispatch_role_catalog_update_translations_missing_locale_after_change() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let id = fetch_role_id_by_slug(&ctx, "handlowiec_l1").await;
    let req = RoleCatalogUpdateRequest {
        id,
        name_translations: Some(vec![pair("pl", "Tylko po polsku")]),
        ..Default::default()
    };
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::UpdateRequest(req)),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    assert!(err.message.contains("en"));
}

// =============================================================================
// Deactivate
// =============================================================================

#[tokio::test]
async fn test_dispatch_role_catalog_deactivate_admin_success() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let id = fetch_role_id_by_slug(&ctx, "handlowiec_l1").await;

    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::DeactivateRequest { id: id.clone() }),
    )
    .await;
    assert!(expect_deactivated(resp));

    let active_only = expect_list(
        run(
            &ctx,
            MessageBody::RoleCatalogBody(RoleCatalogPayload::ListRequest(RoleCatalogListFilter {
                is_active: Some(true),
                ..Default::default()
            })),
        )
        .await,
    );
    assert!(active_only.iter().all(|r| r.id != id));
}

#[tokio::test]
async fn test_dispatch_role_catalog_deactivate_idempotent() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state);
    let id = fetch_role_id_by_slug(&ctx, "handlowiec_l1").await;

    let first = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::DeactivateRequest { id: id.clone() }),
    )
    .await;
    assert!(expect_deactivated(first));

    let second = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::DeactivateRequest { id }),
    )
    .await;
    assert!(!expect_deactivated(second));
}

#[tokio::test]
async fn test_dispatch_role_catalog_deactivate_non_admin_denied() {
    let state = AppState::for_test();
    let admin = admin_ctx(state.clone());
    let id = fetch_role_id_by_slug(&admin, "handlowiec_l1").await;

    let ctx = user_ctx(state);
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::DeactivateRequest { id }),
    )
    .await;
    let err = expect_error(resp);
    assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
}

// =============================================================================
// Audit
// =============================================================================

#[tokio::test]
async fn test_dispatch_role_catalog_audit_log_created_entry() {
    let state = AppState::for_test();
    let ctx = admin_ctx(state.clone());
    let req = fresh_create_request("nowa_rola_audit");
    let resp = run(
        &ctx,
        MessageBody::RoleCatalogBody(RoleCatalogPayload::CreateRequest(req)),
    )
    .await;
    let detail = expect_create(resp);

    let conn = state.db.read().expect("db mutex");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log \
             WHERE action = 'role_catalog.created' \
               AND resource_id = ?1 \
               AND org_id = ?2",
            rusqlite::params![detail.id, ORG_DEFAULT],
            |r| r.get(0),
        )
        .expect("audit_log query");
    assert_eq!(count, 1, "audit_log must record the create");
}
