// =============================================================================
// File: dispatch/app_gate.rs — server-side availability + permission gate for
//       NATIVE applications (app-platform). Hiding a tile is not a gate: every
//       request family of a native app funnels through here, so a disabled or
//       uninstalled app rejects on the wire, and access levels come from the
//       addon permission matrix (PermissionChecker), not org-RBAC.
// =============================================================================

use tentaflow_protocol::{ProtocolError, ProtocolErrorCode};

use super::{HandlerContext, SessionAuthKind};

/// Resolves the SINGLETON app's instance, then applies the same instance gate
/// as [`require_app_instance_permission`]. Returns the instance `addon_id`,
/// which handlers use for instance-scoped state.
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
    check_instance_grant(ctx, package_id, &addon_id, enabled, permission_id)?;
    Ok(addon_id)
}

/// Gate for MULTI-instance apps, where the request names the instance it means
/// (a TentaQuant lab, not "the" lab): the instance comes from the request, and
/// the gate proves it is an ENABLED instance of `package_id` before evaluating
/// that instance's permission matrix. Resolving by package alone is wrong here
/// — `get_package_instance` returns an arbitrary one of several.
///
/// An `addon_id` belonging to another package is refused exactly like a
/// missing one, so a caller cannot probe the instance table of other apps.
pub fn require_app_instance_permission(
    ctx: &HandlerContext,
    package_id: &str,
    addon_id: &str,
    permission_id: &str,
) -> Result<(), ProtocolError> {
    let instance =
        crate::db::repository::get_app_instance_state(&ctx.state.db, addon_id).map_err(|e| {
            tracing::warn!(package_id, addon_id, error = %e, "app gate: instance lookup failed");
            ProtocolError::internal("application registry error")
        })?;
    let Some((owner_package, enabled)) = instance else {
        return Err(unavailable(ctx, package_id, "not installed"));
    };
    if owner_package != package_id {
        return Err(unavailable(
            ctx,
            package_id,
            "not an instance of this application",
        ));
    }
    check_instance_grant(ctx, package_id, addon_id, enabled, permission_id)
}

/// Shared core of both gates: enablement plus the caller's grant for
/// `permission_id` on THIS instance (admin bypass included — the checker
/// hierarchy handles it).
///
/// Non-admin callers get one uniform "unavailable" message for both
/// not-installed and disabled, so the gate never leaks install state; admins
/// see the actual reason.
fn check_instance_grant(
    ctx: &HandlerContext,
    package_id: &str,
    addon_id: &str,
    enabled: bool,
    permission_id: &str,
) -> Result<(), ProtocolError> {
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
        .check(addon_id, user_id, permission_id, None)
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
    pub(crate) fn install_app(
        state: &Arc<AppState>,
        package_id: &str,
        defaults_allow: &[&str],
    ) -> String {
        install_app_instance(
            state,
            package_id,
            &format!("{package_id}-testinst"),
            defaults_allow,
        )
    }

    /// Installs ONE named instance of a package — the multi-instance form of
    /// [`install_app`], which is this with the canonical single-instance id.
    /// `package_id` must be a bundled native package: the row carries the
    /// package manifest rewritten for the instance, exactly what
    /// `lifecycle::install_instance` persists, so code that reads instance
    /// manifests (e.g. `app_db::open` for `native.db_file`) sees the real one.
    pub(crate) fn install_app_instance(
        state: &Arc<AppState>,
        package_id: &str,
        addon_id: &str,
        defaults_allow: &[&str],
    ) -> String {
        let manifest = crate::addon::bundled::native_manifest(package_id)
            .unwrap_or_else(|| panic!("'{package_id}' is not a bundled native package"));
        let manifest = crate::addon::lifecycle::rewrite_manifest_for_instance(
            manifest,
            addon_id,
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
        refresh(state, addon_id);
        addon_id.to_string()
    }

    /// Grants one permission to one user on the instance (matrix row).
    pub(crate) fn grant(state: &Arc<AppState>, addon_id: &str, user_id: &str, perm: &str) {
        set_permission(state, addon_id, "user", user_id, perm, "allow");
    }

    /// Writes one matrix row for any subject kind and grant mode
    /// (`"user"`/`"group"`, `"allow"`/`"deny"`) — the hierarchy tests need
    /// group subjects and explicit denies, which [`grant`] cannot express.
    pub(crate) fn set_permission(
        state: &Arc<AppState>,
        addon_id: &str,
        subject_type: &str,
        subject_id: &str,
        perm: &str,
        grant_mode: &str,
    ) {
        crate::db::repository::upsert_permission(
            &state.db,
            addon_id,
            subject_type,
            subject_id,
            perm,
            grant_mode,
            None,
        )
        .expect("test permission row");
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

/// The refusal that reveals nothing to a non-admin: identical for a package
/// that is not installed, one that is disabled and an instance the caller may
/// not see. `pub(crate)` because an app whose access model adds a condition of
/// its own (TentaQuant intersects the matrix with the instance's Visibility)
/// must answer with exactly this error, not one that can be told apart.
pub(crate) fn unavailable(ctx: &HandlerContext, package_id: &str, reason: &str) -> ProtocolError {
    if SessionAuthKind::Admin.session_satisfies(&ctx.session) {
        ProtocolError::new(
            ProtocolErrorCode::AppUnavailable,
            format!("application '{package_id}' is {reason}"),
        )
    } else {
        ProtocolError::new(ProtocolErrorCode::AppUnavailable, "application unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository;
    use crate::dispatch::state::AppState;
    use tentaflow_protocol::SessionAuth;

    const PACKAGE: &str = "benchmark-studio";
    const OTHER_PACKAGE: &str = "ml-studio";
    const PERM: &str = "benchmark.write";

    /// A caller with an org context but no admin role — the matrix is the only
    /// thing that can grant, exactly like a real dashboard session.
    fn ctx(state: &std::sync::Arc<AppState>, user_id: &str) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0x33u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(crate::services::rbac::OrgContext {
                user_id: user_id.to_string(),
                org_id: "org-t".to_string(),
                role_id: "role-x".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    fn user(state: &std::sync::Arc<AppState>, name: &str) -> String {
        repository::create_user_account(&state.db, name, "$test$hash", name, "").expect("test user")
    }

    /// The whole point of the instance gate: a grant is per instance, so the
    /// same group is admitted in lab A and refused in lab B.
    #[test]
    fn group_grant_is_scoped_to_one_instance() {
        let state = AppState::for_test();
        let lab_a = test_support::install_app_instance(&state, PACKAGE, "lab-a", &[]);
        let lab_b = test_support::install_app_instance(&state, PACKAGE, "lab-b", &[]);

        let student = user(&state, "student-a");
        let group = repository::create_group(&state.db, "lab-a-students", "").expect("group");
        repository::add_user_to_group(&state.db, &group, &student).expect("membership");
        test_support::set_permission(&state, &lab_a, "group", &group, PERM, "allow");

        let c = ctx(&state, &student);
        require_app_instance_permission(&c, PACKAGE, &lab_a, PERM).expect("granted in lab A");
        let denied = require_app_instance_permission(&c, PACKAGE, &lab_b, PERM)
            .expect_err("no entry in lab B");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// Hierarchy: an explicit per-user deny beats an allow inherited from a
    /// group, on the very same instance.
    #[test]
    fn user_deny_beats_group_allow() {
        let state = AppState::for_test();
        let lab = test_support::install_app_instance(&state, PACKAGE, "lab-deny", &[]);

        let student = user(&state, "student-deny");
        let group = repository::create_group(&state.db, "lab-deny-students", "").expect("group");
        repository::add_user_to_group(&state.db, &group, &student).expect("membership");
        test_support::set_permission(&state, &lab, "group", &group, PERM, "allow");

        let c = ctx(&state, &student);
        require_app_instance_permission(&c, PACKAGE, &lab, PERM).expect("group allow admits");

        test_support::set_permission(&state, &lab, "user", &student, PERM, "deny");
        let denied =
            require_app_instance_permission(&c, PACKAGE, &lab, PERM).expect_err("user deny wins");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// An instance of ANOTHER package is refused even when the caller holds the
    /// permission there — and the refusal is the uniform unavailable answer, so
    /// the caller cannot use the gate to probe other apps' instance ids.
    #[test]
    fn instance_of_another_package_is_refused() {
        let state = AppState::for_test();
        test_support::install_app_instance(&state, PACKAGE, "lab-own", &[]);
        let foreign = test_support::install_app_instance(&state, OTHER_PACKAGE, "ml-inst", &[]);

        let student = user(&state, "student-foreign");
        test_support::set_permission(&state, &foreign, "user", &student, PERM, "allow");

        let c = ctx(&state, &student);
        let denied = require_app_instance_permission(&c, PACKAGE, &foreign, PERM)
            .expect_err("foreign instance");
        assert_eq!(denied.code, ProtocolErrorCode::AppUnavailable);
        assert_eq!(denied.message, "application unavailable");

        let missing = require_app_instance_permission(&c, PACKAGE, "no-such-instance", PERM)
            .expect_err("unknown instance");
        assert_eq!(missing.message, denied.message);
    }

    /// A disabled instance refuses regardless of the matrix, and the singleton
    /// gate keeps behaving exactly like the instance gate it now delegates to.
    #[test]
    fn disabled_instance_refuses_both_gates() {
        let state = AppState::for_test();
        let lab = test_support::install_app_instance(&state, PACKAGE, "lab-off", &[PERM]);
        let student = user(&state, "student-off");
        {
            let conn = state.db.write().unwrap();
            conn.execute(
                "UPDATE addons SET is_enabled = 0 WHERE addon_id = ?1",
                rusqlite::params![lab],
            )
            .expect("disable instance");
        }

        let c = ctx(&state, &student);
        let denied = require_app_instance_permission(&c, PACKAGE, &lab, PERM)
            .expect_err("disabled instance");
        assert_eq!(denied.code, ProtocolErrorCode::AppUnavailable);
        let singleton = require_app_permission(&c, PACKAGE, PERM).expect_err("disabled singleton");
        assert_eq!(singleton.code, ProtocolErrorCode::AppUnavailable);
    }
}
