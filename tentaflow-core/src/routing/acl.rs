// =============================================================================
// Plik: routing/acl.rs
// Opis: Helper do sprawdzania ACL (resource_permissions) przed wykonaniem
//       operacji routingowej. Wolany przez chat/embedding/tts handlery gdy
//       maja user context (z HandlerContext). Priorytet:
//         user_deny > user_allow > group_deny > group_allow > default_allow.
// =============================================================================

use crate::db::DbPool;
use anyhow::Result;

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
