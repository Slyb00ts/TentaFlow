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

/// The same gate for a MULTI-INSTANCE app, where the caller names the instance
/// it is talking to (`instance_id` travels in every request of such a family).
/// `require_app_permission` cannot serve these: it resolves the instance from
/// the package id, and with two environments installed that lookup picks one
/// arbitrarily — the caller would be gated against an instance it never named
/// and granted access to data of another.
///
/// Three things are verified before the matrix, and all three fail the same way
/// for a non-admin: the instance exists, it belongs to `package_id`, and it is
/// enabled. The package check is what stops one app's instance id from being
/// used to enter another app's request family, and the uniform message is what
/// stops the gate from answering "which app is this id?" to someone who may not
/// know that the id exists at all.
pub fn require_app_instance_permission(
    ctx: &HandlerContext,
    package_id: &str,
    instance_id: &str,
    permission_id: &str,
) -> Result<(), ProtocolError> {
    let instance = crate::db::repository::get_addon(&ctx.state.db, instance_id).map_err(|e| {
        tracing::warn!(package_id, instance_id, error = %e, "app gate: instance lookup failed");
        ProtocolError::internal("application registry error")
    })?;
    let Some(instance) = instance else {
        return Err(unavailable(ctx, package_id, "not installed"));
    };
    if instance.package_id != package_id {
        return Err(unavailable(ctx, package_id, "not an instance of this app"));
    }
    if !instance.is_enabled {
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
        .check(instance_id, user_id, permission_id, None)
        .is_granted()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{permission_id} permission required"),
        ));
    }
    Ok(())
}

/// Availability checks for entry points OUTSIDE dispatch (a sidecar's reverse
/// stream has no user session — the platform contract still demands every
/// entry point verify the instance itself). Two tiers because of
/// `disable_semantics = "drain"`: a DISABLED app refuses new work but keeps
/// serving its running sessions, an UNINSTALLED app serves nothing.
pub fn package_instance_installed(db: &crate::db::DbPool, package_id: &str) -> bool {
    matches!(
        crate::db::repository::get_package_instance(db, package_id),
        Ok(Some(_))
    )
}

/// Test fixture for gated app families: registers an ENABLED app instance in
/// the test DB (instance row + manifest-style permission defaults) so the
/// gate passes, and refreshes the checker. Shared by every dispatch/stream
/// test that goes through `require_app_permission`.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::dispatch::state::AppState;
    use std::sync::Arc;

    /// Returns the instance addon_id (`{package_id}-testinst`). Idempotent.
    /// `package_id` must be a bundled native package: the row carries the
    /// package manifest rewritten for the instance, exactly what
    /// `lifecycle::install_instance` persists, so code that reads instance
    /// manifests (e.g. `app_db::open` for `native.db_file`) sees the real one.
    pub(crate) fn install_app(
        state: &Arc<AppState>,
        package_id: &str,
        defaults_allow: &[&str],
    ) -> String {
        let manifest = crate::addon::bundled::native_manifest(package_id)
            .unwrap_or_else(|| panic!("'{package_id}' is not a bundled native package"));
        install_app_instance(
            state,
            package_id,
            &format!("{package_id}-testinst"),
            manifest,
            defaults_allow,
        )
    }

    /// The same, with the instance id and the package manifest spelled out.
    /// A MULTI-INSTANCE app needs the id: the whole point of its gate is that
    /// two environments of one package are separate subjects, and a fixture
    /// that can only build one of them cannot show that. It needs the manifest
    /// because an app can be built before its package is listed in the
    /// catalog — TentaVM is, deliberately, until its tile has a route to open.
    pub(crate) fn install_app_instance(
        state: &Arc<AppState>,
        package_id: &str,
        addon_id: &str,
        manifest: &str,
        defaults_allow: &[&str],
    ) -> String {
        let addon_id = addon_id.to_string();
        let manifest = crate::addon::lifecycle::rewrite_manifest_for_instance(
            manifest,
            &addon_id,
            package_id,
            &std::collections::BTreeMap::new(),
        )
        .expect("instance manifest");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO addons \
                 (addon_id, name, version, package_id, package_version, runtime, is_enabled, \
                  manifest_json) \
                 VALUES (?1, ?2, '1.0.0', ?3, '1.0.0', 'native', 1, ?4)",
                rusqlite::params![addon_id, package_id, package_id, manifest],
            )
            .expect("test instance row");
            for perm in defaults_allow {
                conn.execute(
                    "INSERT OR IGNORE INTO addon_permission_defaults \
                     (addon_id, permission_id, grant_mode) VALUES (?1, ?2, 'allow')",
                    rusqlite::params![addon_id, perm],
                )
                .expect("test default row");
            }
        }
        refresh(state, &addon_id);
        addon_id
    }

    /// Grants one permission to one user on the instance (matrix row).
    pub(crate) fn grant(state: &Arc<AppState>, addon_id: &str, user_id: &str, perm: &str) {
        crate::db::repository::upsert_permission(
            &state.db, addon_id, "user", user_id, perm, "allow", None,
        )
        .expect("test grant");
        refresh(state, addon_id);
    }

    fn refresh(state: &Arc<AppState>, addon_id: &str) {
        state
            .permission_checker
            .as_ref()
            .expect("test state has a checker")
            .refresh_addon(addon_id);
    }
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
