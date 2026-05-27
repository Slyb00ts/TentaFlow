// ============ File: dispatch/role_catalog.rs — binarne RPC dla katalogu rol ============
//
// Jeden slot `MessageBody::RoleCatalogBody` obsluguje 7 operacji nad
// `services::role_catalog`. Wzorzec spojny z `dispatch::camera_admin` i
// `dispatch::legal_admin`: jedna funkcja dispatchujaca + `register_*_variant!`
// dla kazdego inner Request variantu wskazujace na ten sam `dispatch_fn`.
//
// Polityka uprawnien:
//   * `List / Get / GetBySlug / ListLocales` — kazdy zalogowany user
//     (`UserSession`). Read-only.
//   * `Create / Update / Deactivate` — wylacznie admin (`is_admin(ctx)` na
//     poziomie inner dispatchu; rejestrujemy variant z `UserSession` zeby
//     komunikat o braku uprawnien szedl jako `PolicyDenied` z czytelnym
//     `"admin required"` zamiast generycznego `AuthRequired` z gate'u policy).
//
// Audit log emitowany jest przez warstwe `services::role_catalog::repo`
// (`audit::emit_*`) — handler nie dubluje wpisow.

use std::collections::BTreeMap;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, PlatformLocaleSummary, ProtocolError, ProtocolErrorCode, RoleCatalogCreateRequest,
    RoleCatalogDetail, RoleCatalogListFilter, RoleCatalogPayload, RoleCatalogSummary,
    RoleCatalogUpdateRequest, SessionAuth,
};

use super::HandlerContext;
use crate::services::rbac::OrgContext;
use crate::services::role_catalog::{
    PlatformLocale, Role, RoleCatalogError, RoleCreateInput, RoleKind, RoleListFilter,
    RoleUpdateInput, VisibilityScope, create_role, deactivate_role, get_role, get_role_by_slug,
    list_active_locales, list_roles, update_role,
};

// =============================================================================
// Pomocnicze gate'y i konwersje
// =============================================================================

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

fn require_admin(ctx: &HandlerContext) -> Result<(), ProtocolError> {
    if !is_admin(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "admin required",
        ));
    }
    Ok(())
}

fn vec_to_btreemap(v: Vec<(String, String)>) -> BTreeMap<String, String> {
    v.into_iter().collect()
}

fn btreemap_to_vec(m: &BTreeMap<String, String>) -> Vec<(String, String)> {
    m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn role_to_summary(role: &Role) -> RoleCatalogSummary {
    RoleCatalogSummary {
        id: role.id.clone(),
        slug: role.slug.clone(),
        kind: role.kind.as_db_str().to_string(),
        name_translations: btreemap_to_vec(&role.name_translations),
        icon: role.icon.clone(),
        color_hint: role.color_hint.clone(),
        is_manager: role.is_manager,
        default_visibility_scope: role.default_visibility_scope.as_db_str().to_string(),
        is_active: role.is_active,
    }
}

fn role_to_detail(role: Role) -> RoleCatalogDetail {
    let name_translations = btreemap_to_vec(&role.name_translations);
    let description_translations = btreemap_to_vec(&role.description_translations);
    RoleCatalogDetail {
        id: role.id,
        org_id: role.org_id,
        slug: role.slug,
        kind: role.kind.as_db_str().to_string(),
        name_translations,
        description_translations,
        icon: role.icon,
        color_hint: role.color_hint,
        is_manager: role.is_manager,
        default_visibility_scope: role.default_visibility_scope.as_db_str().to_string(),
        is_active: role.is_active,
        created_at: role.created_at,
        updated_at: role.updated_at,
        created_by: role.created_by,
    }
}

fn locale_to_summary(loc: PlatformLocale) -> PlatformLocaleSummary {
    PlatformLocaleSummary {
        code: loc.code,
        display_name: loc.display_name,
        is_default: loc.is_default,
    }
}

/// Mapuje `RoleCatalogError` na `ProtocolError` z kodem zgodnym z semantyka
/// bledu. Komunikaty zachowuja oryginalny tekst `RoleCatalogError`, aby UI
/// mialo czytelna podpowiedz (brakujace locale, niepoprawny slug, itd.).
fn map_repo_err(err: RoleCatalogError) -> ProtocolError {
    match err {
        RoleCatalogError::NotFound(_) => ProtocolError::not_found(err.to_string()),
        RoleCatalogError::SlugConflict { .. } => {
            ProtocolError::new(ProtocolErrorCode::Conflict, err.to_string())
        }
        RoleCatalogError::InvalidSlug(_)
        | RoleCatalogError::InvalidKind(_)
        | RoleCatalogError::InvalidScope(_)
        | RoleCatalogError::MissingTranslations { .. }
        | RoleCatalogError::EmptyTranslation { .. }
        | RoleCatalogError::UnknownIcon(_)
        | RoleCatalogError::InvalidColorHint(_)
        | RoleCatalogError::NoActiveLocales(_) => ProtocolError::bad_request(err.to_string()),
        RoleCatalogError::InvalidJson(_) | RoleCatalogError::DbError(_) => {
            ProtocolError::internal(err.to_string())
        }
    }
}

fn filter_from_protocol(f: RoleCatalogListFilter) -> Result<RoleListFilter, ProtocolError> {
    let kind = match f.kind {
        Some(s) => Some(RoleKind::from_db_str(&s).map_err(map_repo_err)?),
        None => None,
    };
    Ok(RoleListFilter {
        kind,
        is_active: f.is_active,
        search: f.search,
        limit: f.limit.map(|v| v as usize),
        offset: f.offset.map(|v| v as usize),
    })
}

fn create_input_from_protocol(
    org_id: &str,
    req: RoleCatalogCreateRequest,
) -> Result<RoleCreateInput, ProtocolError> {
    let kind = RoleKind::from_db_str(&req.kind).map_err(map_repo_err)?;
    let scope =
        VisibilityScope::from_db_str(&req.default_visibility_scope).map_err(map_repo_err)?;
    Ok(RoleCreateInput {
        org_id: org_id.to_string(),
        slug: req.slug,
        kind,
        name_translations: vec_to_btreemap(req.name_translations),
        description_translations: vec_to_btreemap(req.description_translations),
        icon: req.icon,
        color_hint: req.color_hint,
        is_manager: req.is_manager,
        default_visibility_scope: scope,
    })
}

fn update_input_from_protocol(
    req: &RoleCatalogUpdateRequest,
) -> Result<RoleUpdateInput, ProtocolError> {
    let kind = match req.kind.as_deref() {
        Some(s) => Some(RoleKind::from_db_str(s).map_err(map_repo_err)?),
        None => None,
    };
    let scope = match req.default_visibility_scope.as_deref() {
        Some(s) => Some(VisibilityScope::from_db_str(s).map_err(map_repo_err)?),
        None => None,
    };
    Ok(RoleUpdateInput {
        kind,
        name_translations: req.name_translations.clone().map(vec_to_btreemap),
        description_translations: req.description_translations.clone().map(vec_to_btreemap),
        icon: req.icon.clone(),
        color_hint: req.color_hint.clone(),
        is_manager: req.is_manager,
        default_visibility_scope: scope,
    })
}

// =============================================================================
// Publiczny entry — pojedynczy slot `RoleCatalogBody`
// =============================================================================

#[handler(variant = "RoleCatalogBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn role_catalog_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::RoleCatalogBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected RoleCatalogBody")),
    };
    match payload {
        RoleCatalogPayload::ListRequest(filter) => {
            let resp = role_catalog_list_v1(ctx, filter.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::GetRequest { id } => {
            let resp = role_catalog_get_v1(ctx, id.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::GetBySlugRequest { slug } => {
            let resp = role_catalog_get_by_slug_v1(ctx, slug.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::ListLocalesRequest => {
            let resp = role_catalog_list_locales_v1(ctx)?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::CreateRequest(create_req) => {
            let resp = role_catalog_create_v1(ctx, create_req.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::UpdateRequest(update_req) => {
            let resp = role_catalog_update_v1(ctx, update_req.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::DeactivateRequest { id } => {
            let resp = role_catalog_deactivate_v1(ctx, id.clone())?;
            Ok(MessageBody::RoleCatalogBody(resp))
        }
        RoleCatalogPayload::ListResponse { .. }
        | RoleCatalogPayload::GetResponse { .. }
        | RoleCatalogPayload::ListLocalesResponse { .. }
        | RoleCatalogPayload::CreateResponse(_)
        | RoleCatalogPayload::UpdateResponse(_)
        | RoleCatalogPayload::DeactivateResponse { .. } => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_role_catalog_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_role_catalog_dispatch,
            }
        }
    };
}

register_role_catalog_variant!(
    "RoleCatalogListRequest",
    "tentaflow_ws_handler_role_catalog_list"
);
register_role_catalog_variant!(
    "RoleCatalogGetRequest",
    "tentaflow_ws_handler_role_catalog_get"
);
register_role_catalog_variant!(
    "RoleCatalogGetBySlugRequest",
    "tentaflow_ws_handler_role_catalog_get_by_slug"
);
register_role_catalog_variant!(
    "RoleCatalogListLocalesRequest",
    "tentaflow_ws_handler_role_catalog_list_locales"
);
register_role_catalog_variant!(
    "RoleCatalogCreateRequest",
    "tentaflow_ws_handler_role_catalog_create"
);
register_role_catalog_variant!(
    "RoleCatalogUpdateRequest",
    "tentaflow_ws_handler_role_catalog_update"
);
register_role_catalog_variant!(
    "RoleCatalogDeactivateRequest",
    "tentaflow_ws_handler_role_catalog_deactivate"
);

// =============================================================================
// List
// =============================================================================

fn role_catalog_list_v1(
    ctx: &HandlerContext,
    filter: RoleCatalogListFilter,
) -> Result<RoleCatalogPayload, ProtocolError> {
    let org = require_org(ctx)?;
    let repo_filter = filter_from_protocol(filter)?;
    let roles = list_roles(&ctx.state.db, &org.org_id, repo_filter).map_err(map_repo_err)?;
    let summaries = roles.iter().map(role_to_summary).collect();
    Ok(RoleCatalogPayload::ListResponse { roles: summaries })
}

// =============================================================================
// Get / GetBySlug
// =============================================================================

fn role_catalog_get_v1(
    ctx: &HandlerContext,
    id: String,
) -> Result<RoleCatalogPayload, ProtocolError> {
    let org = require_org(ctx)?;
    match get_role(&ctx.state.db, &org.org_id, &id) {
        Ok(role) => Ok(RoleCatalogPayload::GetResponse {
            role: Some(role_to_detail(role)),
        }),
        Err(RoleCatalogError::NotFound(_)) => Ok(RoleCatalogPayload::GetResponse { role: None }),
        Err(e) => Err(map_repo_err(e)),
    }
}

fn role_catalog_get_by_slug_v1(
    ctx: &HandlerContext,
    slug: String,
) -> Result<RoleCatalogPayload, ProtocolError> {
    let org = require_org(ctx)?;
    match get_role_by_slug(&ctx.state.db, &org.org_id, &slug) {
        Ok(role) => Ok(RoleCatalogPayload::GetResponse {
            role: Some(role_to_detail(role)),
        }),
        Err(RoleCatalogError::NotFound(_)) => Ok(RoleCatalogPayload::GetResponse { role: None }),
        Err(e) => Err(map_repo_err(e)),
    }
}

// =============================================================================
// List locales
// =============================================================================

fn role_catalog_list_locales_v1(ctx: &HandlerContext) -> Result<RoleCatalogPayload, ProtocolError> {
    let org = require_org(ctx)?;
    let locales = list_active_locales(&ctx.state.db, &org.org_id).map_err(map_repo_err)?;
    let summaries = locales.into_iter().map(locale_to_summary).collect();
    Ok(RoleCatalogPayload::ListLocalesResponse { locales: summaries })
}

// =============================================================================
// Create (admin)
// =============================================================================

fn role_catalog_create_v1(
    ctx: &HandlerContext,
    req: RoleCatalogCreateRequest,
) -> Result<RoleCatalogPayload, ProtocolError> {
    require_admin(ctx)?;
    let org = require_org(ctx)?;
    let input = create_input_from_protocol(&org.org_id, req)?;
    let actor = org.user_id.clone();
    let role = create_role(&ctx.state.db, &actor, input).map_err(map_repo_err)?;
    Ok(RoleCatalogPayload::CreateResponse(role_to_detail(role)))
}

// =============================================================================
// Update (admin)
// =============================================================================

fn role_catalog_update_v1(
    ctx: &HandlerContext,
    req: RoleCatalogUpdateRequest,
) -> Result<RoleCatalogPayload, ProtocolError> {
    require_admin(ctx)?;
    let org = require_org(ctx)?;
    let patch = update_input_from_protocol(&req)?;
    let actor = org.user_id.clone();
    let role =
        update_role(&ctx.state.db, &actor, &org.org_id, &req.id, patch).map_err(map_repo_err)?;
    Ok(RoleCatalogPayload::UpdateResponse(role_to_detail(role)))
}

// =============================================================================
// Deactivate (admin) — idempotentne: drugi deactivate zwraca `deactivated=false`
// =============================================================================

fn role_catalog_deactivate_v1(
    ctx: &HandlerContext,
    id: String,
) -> Result<RoleCatalogPayload, ProtocolError> {
    require_admin(ctx)?;
    let org = require_org(ctx)?;
    let actor = org.user_id.clone();
    match deactivate_role(&ctx.state.db, &actor, &org.org_id, &id) {
        Ok(()) => Ok(RoleCatalogPayload::DeactivateResponse { deactivated: true }),
        Err(RoleCatalogError::NotFound(_)) => {
            Ok(RoleCatalogPayload::DeactivateResponse { deactivated: false })
        }
        Err(e) => Err(map_repo_err(e)),
    }
}
