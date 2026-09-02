// =============================================================================
// File: dispatch/app_gate.rs — server-side availability + permission gate for
//       NATIVE applications (app-platform). Hiding a tile is not a gate: every
//       request family of a native app funnels through here, so a disabled or
//       uninstalled app rejects on the wire, and access levels come from the
//       addon permission matrix (PermissionChecker), not org-RBAC.
// =============================================================================

use tentaflow_protocol::{ProtocolError, ProtocolErrorCode};

use super::{HandlerContext, SessionAuthKind};

/// Resolves the app's instance, verifies it is enabled and checks the caller's
/// grant for `permission_id` in the permission matrix (admin bypass included —
/// the checker hierarchy handles it). Returns the instance `addon_id`, which
/// handlers can use for instance-scoped state.
///
/// Non-admin callers get one uniform "unavailable" message for both
/// not-installed and disabled, so the gate never leaks install state; admins
/// see the actual reason.
pub fn require_app_permission(
    ctx: &HandlerContext,
    package_id: &str,
    permission_id: &str,
) -> Result<String, ProtocolError> {
    let instance =
        crate::db::repository::get_package_instance(&ctx.state.db, package_id).map_err(|e| {
            tracing::warn!(package_id, error = %e, "app gate: instance lookup failed");
            ProtocolError::internal("application registry error")
        })?;
    let Some((addon_id, enabled)) = instance else {
        return Err(unavailable(ctx, package_id, "not installed"));
    };
    if !enabled {
        return Err(unavailable(ctx, package_id, "disabled"));
    }

    let user_id = ctx
        .org_context
        .as_ref()
        .map(|o| o.user_id.as_str())
        .ok_or_else(|| {
            ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
        })?;
    let checker = ctx.state.permission_checker.as_ref().ok_or_else(|| {
        // Fail closed: a build without the checker wired must never grant.
        tracing::error!(package_id, "app gate: permission checker not wired");
        ProtocolError::internal("permission checker unavailable")
    })?;
    if !checker
        .check(&addon_id, user_id, permission_id, None)
        .is_granted()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{permission_id} permission required"),
        ));
    }
    Ok(addon_id)
}

fn unavailable(ctx: &HandlerContext, package_id: &str, reason: &str) -> ProtocolError {
    if SessionAuthKind::Admin.session_satisfies(&ctx.session) {
        ProtocolError::new(
            ProtocolErrorCode::AppUnavailable,
            format!("application '{package_id}' is {reason}"),
        )
    } else {
        ProtocolError::new(ProtocolErrorCode::AppUnavailable, "application unavailable")
    }
}
