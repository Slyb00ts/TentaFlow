// =============================================================================
// Plik: routing/acl.rs
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
    pub user_id: i64,
    pub role: String,
}

impl UserContext {
    pub fn new(user_id: i64, role: impl Into<String>) -> Self {
        Self { user_id, role: role.into() }
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// Sprawdza czy user moze uzyc zasobu `(resource_type, resource_id)`.
/// user_role pobrane z HandlerContext (JWT claims) — admin omija ACL.
pub fn check_access(
    db: &DbPool,
    resource_type: &str,
    resource_id: &str,
    user_id: i64,
    user_role: &str,
) -> Result<bool> {
    crate::db::repository::resource_permissions::check(
        db,
        resource_type,
        resource_id,
        user_id,
        user_role,
    )
}

/// Bezpieczna wersja — zwraca true przy bledzie DB (fail-open) zeby pojedyncza
/// awaria DB nie blokowala calego mesha. Bledy logowane do warn.
pub fn check_access_safe(
    db: &DbPool,
    resource_type: &str,
    resource_id: &str,
    user_id: i64,
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
