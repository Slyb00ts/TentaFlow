// =============================================================================
// Plik: auth/acl.rs
// Opis: Helper do sprawdzania ACL (resource_permissions) przed wykonaniem
//       operacji routingowej. Wolany przez chat/embedding/tts handlery gdy
//       maja user context (z HandlerContext). Priorytet:
//         user_deny > user_allow > group_deny > group_allow > default_allow.
// =============================================================================

use crate::db::DbPool;
use anyhow::Result;

/// Kontekst uzytkownika propagowany przez warstwe routingu — pozwala ACL
/// check-om na zasoby (modele, flowy, addony) na zidentyfikowanie wlasciciela
/// requestu. `None` = internal caller (np. flow engine wewnetrzne,
/// reverse_request), ACL jest wtedy skipowane (fail-open).
#[derive(Debug, Clone)]
pub struct UserContext {
    pub user_id: String,
    pub role: String,
}

impl UserContext {
    pub fn new(user_id: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            role: role.into(),
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// Podmiot egzekwowany na warstwie /v1 (Tier 2). Rozroznia uzytkownika
/// (z rola — admin-bypass), grupe (bez admin-bypass) oraz klucz API
/// (wylacznie reguly `subject_type='api_key'`, bez admin-bypass).
#[derive(Debug, Clone)]
pub enum Principal {
    User { user_id: String, role: String },
    Group { group_id: String },
    ApiKey { uid: String },
}

/// Sprawdza czy user moze uzyc zasobu `(resource_type, resource_id)`.
/// user_role pobrane z HandlerContext (JWT claims) — admin omija ACL.
/// Wariant Tier1: default ALLOW (public by default).
pub fn check_access(
    db: &DbPool,
    resource_type: &str,
    resource_id: &str,
    user_id: &str,
    user_role: &str,
) -> Result<bool> {
    crate::db::repository::resource_permissions::check_default_allow(
        db,
        resource_type,
        resource_id,
        user_id,
        user_role,
    )
}

/// Bezpieczna wersja Tier1 — zwraca true przy bledzie DB (fail-open) zeby
/// pojedyncza awaria DB nie blokowala calego mesha. Bledy logowane do warn.
pub fn check_access_safe(
    db: &DbPool,
    resource_type: &str,
    resource_id: &str,
    user_id: &str,
    user_role: &str,
) -> bool {
    match check_access(db, resource_type, resource_id, user_id, user_role) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                resource_type,
                resource_id,
                user_id,
                "ACL check failed — fail-open: {}",
                e
            );
            true
        }
    }
}

/// Egzekucja ACL dla /v1 (Tier 2) — fail-CLOSED. Semantyka default DENY
/// (patrz `resource_permissions::check_subject_default_deny`). Przy bledzie DB
/// zwraca `false` (deny) i loguje warn — odwrotnosc `check_access_safe`.
pub fn check_v1_access(
    db: &DbPool,
    resource_type: &str,
    resource_id: &str,
    principal: &Principal,
) -> bool {
    match crate::db::repository::resource_permissions::check_subject_default_deny(
        db,
        resource_type,
        resource_id,
        principal,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                resource_type,
                resource_id,
                ?principal,
                "v1 ACL check failed — fail-closed (deny): {}",
                e
            );
            false
        }
    }
}
