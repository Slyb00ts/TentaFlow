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

/// Multi-instance counterpart of `require_app_permission`: the caller names
/// the instance. Verifies (1) the row exists AND belongs to `package_id`
/// (membership — an instance id from another package is "unavailable", never
/// a cross-app hop), (2) it is enabled, (3) the caller's matrix grant ON THAT
/// INSTANCE. Same uniform `AppUnavailable` message for non-admins.
///
/// Returns the resolved `addon_id` (the DB row's, not simply the client-
/// supplied argument echoed back) — same contract as `require_app_permission`
/// — so a caller keeps using the gate-verified id for any instance-scoped
/// work that follows, instead of reaching back into the argument it passed in
/// (which is what it is, but the type keeps that guarantee explicit and
/// matches the sibling function's shape).
pub fn require_instance_permission(
    ctx: &HandlerContext,
    package_id: &str,
    addon_id: &str,
    permission_id: &str,
) -> Result<String, ProtocolError> {
    let instance = crate::db::repository::get_instance_of_package(
        &ctx.state.db,
        package_id,
        addon_id,
    )
    .map_err(|e| {
        tracing::warn!(package_id, addon_id, error = %e, "app gate: instance lookup failed");
        ProtocolError::internal("application registry error")
    })?;
    let Some((resolved_addon_id, enabled)) = instance else {
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
        .check(&resolved_addon_id, user_id, permission_id, None)
        .is_granted()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{permission_id} permission required"),
        ));
    }
    Ok(resolved_addon_id)
}

/// The owner's double lock (plan-app-platform §0.4, `dispatch/tentanas.rs::
/// gate_admin`): the instance's `permission_id` grant in the matrix AND the
/// caller's org Admin role. The matrix can delegate an app's admin
/// permission to a non-admin user; the org role cannot be delegated —
/// destructive/admin operations require both, so every native app does not
/// re-implement this pattern itself. Returns the resolved `addon_id`, same
/// as `require_instance_permission`.
pub fn require_instance_admin(
    ctx: &HandlerContext,
    package_id: &str,
    addon_id: &str,
    permission_id: &str,
) -> Result<String, ProtocolError> {
    let resolved_addon_id = require_instance_permission(ctx, package_id, addon_id, permission_id)?;
    let is_org_admin = ctx.org_context.as_ref().is_some_and(|o| o.has("org.admin"));
    if !is_org_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "org Admin role required",
        ));
    }
    Ok(resolved_addon_id)
}

/// Distinguishes "no instance at all", "installed but none enabled",
/// "more than one enabled" and "the lookup itself failed" — `Ok` alone
/// cannot tell a caller which fallback to apply (404 vs. a 409 naming the
/// candidates vs. a transient 500), and collapsing "disabled" into "none"
/// would make an installed-but-disabled app indistinguishable from one that
/// was never installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoleInstanceError {
    /// No instance of the package is installed.
    None,
    /// At least one instance is installed, but none is enabled.
    Disabled,
    /// More than one instance is enabled; the caller cannot pick one.
    Ambiguous(usize),
    /// The instance list query itself failed (DB/pool error) — kept distinct
    /// from `None` so a transient failure is never reported as "not
    /// installed".
    Lookup,
}

/// Resolves the ONE enabled instance of a non-singleton package for callers
/// that cannot name one (legacy SDK/REST default). `Ok(id)` only when exactly
/// one enabled instance exists; otherwise a distinguishable error so the
/// caller can tell "none installed" from "installed but disabled" from
/// "ambiguous" from "lookup failed".
pub fn sole_enabled_instance(
    db: &crate::db::DbPool,
    package_id: &str,
) -> Result<String, SoleInstanceError> {
    let instances = crate::db::repository::list_package_instances(db, package_id).map_err(|e| {
        tracing::warn!(package_id, error = %e, "app gate: instance list failed");
        SoleInstanceError::Lookup
    })?;
    let any_installed = !instances.is_empty();
    let mut enabled = instances
        .into_iter()
        .filter(|(_, is_enabled, _)| *is_enabled);
    let Some((addon_id, _, _)) = enabled.next() else {
        return Err(if any_installed {
            SoleInstanceError::Disabled
        } else {
            SoleInstanceError::None
        });
    };
    let remaining = enabled.count();
    if remaining > 0 {
        return Err(SoleInstanceError::Ambiguous(1 + remaining));
    }
    Ok(addon_id)
}

/// Non-dispatch entry points (REST, reactor, host functions): instance
/// exists, belongs to the package, and is enabled.
pub fn instance_enabled(db: &crate::db::DbPool, package_id: &str, addon_id: &str) -> bool {
    matches!(
        crate::db::repository::get_instance_of_package(db, package_id, addon_id),
        Ok(Some((_, true)))
    )
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

    /// Installs the Nth instance of a package (`{package_id}-{suffix}`) so a
    /// test can drive two isolated instances of the same package. Idempotent.
    /// `package_id` must be a bundled native package: the row carries the
    /// package manifest rewritten for the instance, exactly what
    /// `lifecycle::install_instance` persists, so code that reads instance
    /// manifests (e.g. `app_db::open` for `native.db_file`) sees the real one.
    ///
    /// Three ways this diverges from the real install path
    /// (`lifecycle::install_native_instance`), each a deliberate shortcut for
    /// gate tests specifically, not a general install fixture:
    /// 1. The row is inserted with `is_enabled = 1`. The real path always
    ///    inserts `0` (`lifecycle.rs:268-277`) — a freshly installed instance
    ///    starts DISABLED. Most gate tests want an available instance by
    ///    default; a test that needs the disabled case flips it explicitly
    ///    with `db::repository::set_addon_enabled`.
    /// 2. It bypasses `count_addon_instances`/the singleton check entirely —
    ///    it INSERTs a row directly, so it will happily create a second
    ///    instance of a `singleton = true` package. Real installs refuse that
    ///    (`lifecycle.rs:222-235`; regression-tested in
    ///    `tests/native_app_multi_instance.rs::
    ///    singleton_package_still_refuses_a_second_instance`).
    /// 3. The suffix is a caller-chosen string (`"a"`, `"b"`, `"testinst"`,
    ///    ...), not `unique_instance_id`'s 8 lowercase hex chars
    ///    (`lifecycle.rs:478-487`). `native_apps::package_of_instance`, which
    ///    parses `{package}-{8hex}` to recover a package id from an instance
    ///    id on the sync-remove path, does not recognize these ids as
    ///    instances of anything (see `package_of_instance_parses_instance_shape`
    ///    in `addon/native_apps.rs`) — a test that exercises that recovery
    ///    path needs a real `lifecycle::install_instance` id, not this one.
    pub(crate) fn install_app_instance(
        state: &Arc<AppState>,
        package_id: &str,
        suffix: &str,
        defaults_allow: &[&str],
    ) -> String {
        let addon_id = format!("{package_id}-{suffix}");
        let manifest = crate::addon::bundled::native_manifest(package_id)
            .unwrap_or_else(|| panic!("'{package_id}' is not a bundled native package"));
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

    /// Returns the instance addon_id (`{package_id}-testinst`). Idempotent.
    pub(crate) fn install_app(
        state: &Arc<AppState>,
        package_id: &str,
        defaults_allow: &[&str],
    ) -> String {
        install_app_instance(state, package_id, "testinst", defaults_allow)
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
    use crate::dispatch::state::AppState;
    use crate::services::rbac::OrgContext;
    use std::sync::Arc;
    use tentaflow_protocol::SessionAuth;

    fn ctx(state: &Arc<AppState>, user_id: &str) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [7u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: "org-1".to_string(),
                role_id: "role-1".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    #[test]
    fn require_instance_permission_rejects_a_disabled_instance() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(
            &state,
            "benchmark-studio",
            "a",
            &["benchmark.read"],
        );
        crate::db::repository::set_addon_enabled(&state.db, &addon_id, false).expect("disable");
        let c = ctx(&state, "user-1");
        let err = require_instance_permission(&c, "benchmark-studio", &addon_id, "benchmark.read")
            .unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::AppUnavailable);
    }

    #[test]
    fn require_instance_permission_rejects_an_instance_of_another_package() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(
            &state,
            "benchmark-studio",
            "a",
            &["benchmark.read"],
        );
        let c = ctx(&state, "user-1");
        // `addon_id` belongs to "benchmark-studio", not "code-studio": a
        // foreign instance id must be treated as unavailable, never as a
        // cross-app hop.
        let err = require_instance_permission(&c, "code-studio", &addon_id, "code_studio.read")
            .unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::AppUnavailable);
        // Indistinguishable from an id that exists nowhere, or the gate becomes
        // a probe for other apps' instance ids.
        let missing =
            require_instance_permission(&c, "code-studio", "no-such-instance", "code_studio.read")
                .unwrap_err();
        assert_eq!(missing.message, err.message);
    }

    fn user(state: &Arc<AppState>, name: &str) -> String {
        crate::db::repository::create_user_account(&state.db, name, "$test$hash", name, "")
            .expect("test user")
    }

    /// A grant is per instance even when it arrives through a group: the same
    /// group is admitted in lab A and refused in lab B.
    #[test]
    fn group_grant_is_scoped_to_one_instance() {
        let state = AppState::for_test();
        let lab_a = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        let lab_b = test_support::install_app_instance(&state, "benchmark-studio", "b", &[]);

        let student = user(&state, "student-a");
        let group =
            crate::db::repository::create_group(&state.db, "lab-a-students", "").expect("group");
        crate::db::repository::add_user_to_group(&state.db, &group, &student).expect("membership");
        test_support::set_permission(&state, &lab_a, "group", &group, "benchmark.write", "allow");

        let c = ctx(&state, &student);
        require_instance_permission(&c, "benchmark-studio", &lab_a, "benchmark.write")
            .expect("granted in lab A");
        let denied = require_instance_permission(&c, "benchmark-studio", &lab_b, "benchmark.write")
            .expect_err("no entry in lab B");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// Hierarchy: an explicit per-user deny beats an allow inherited from a
    /// group, on the very same instance.
    #[test]
    fn user_deny_beats_group_allow() {
        let state = AppState::for_test();
        let lab = test_support::install_app_instance(&state, "benchmark-studio", "deny", &[]);

        let student = user(&state, "student-deny");
        let group =
            crate::db::repository::create_group(&state.db, "lab-deny-students", "").expect("group");
        crate::db::repository::add_user_to_group(&state.db, &group, &student).expect("membership");
        test_support::set_permission(&state, &lab, "group", &group, "benchmark.write", "allow");

        let c = ctx(&state, &student);
        require_instance_permission(&c, "benchmark-studio", &lab, "benchmark.write")
            .expect("group allow admits");

        test_support::set_permission(&state, &lab, "user", &student, "benchmark.write", "deny");
        let denied = require_instance_permission(&c, "benchmark-studio", &lab, "benchmark.write")
            .expect_err("user deny wins");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// The singleton gate resolves its own instance, so it needs its own proof
    /// that a disabled one refuses regardless of the matrix.
    #[test]
    fn require_app_permission_refuses_a_disabled_singleton() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(
            &state,
            "benchmark-studio",
            "only",
            &["benchmark.write"],
        );
        crate::db::repository::set_addon_enabled(&state.db, &addon_id, false).expect("disable");
        let c = ctx(&state, "user-1");
        let err = require_app_permission(&c, "benchmark-studio", "benchmark.write").unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::AppUnavailable);
    }

    #[test]
    fn require_instance_permission_checks_the_matrix_of_that_instance_only() {
        let state = AppState::for_test();
        let addon_a = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        let addon_b = test_support::install_app_instance(&state, "benchmark-studio", "b", &[]);
        test_support::grant(&state, &addon_a, "user-1", "benchmark.write");

        let c = ctx(&state, "user-1");
        require_instance_permission(&c, "benchmark-studio", &addon_a, "benchmark.write")
            .expect("granted on instance A");
        let err = require_instance_permission(&c, "benchmark-studio", &addon_b, "benchmark.write")
            .unwrap_err();
        assert_eq!(
            err.code,
            ProtocolErrorCode::PolicyDenied,
            "a grant on instance A must not leak to instance B"
        );
    }

    #[test]
    fn sole_enabled_instance_distinguishes_none_from_disabled() {
        let state = AppState::for_test();
        // Nothing installed at all: "not installed", not "disabled".
        assert_eq!(
            sole_enabled_instance(&state.db, "benchmark-studio"),
            Err(SoleInstanceError::None)
        );

        let addon_a = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        // Installed but disabled must be reported as `Disabled`, not `None` —
        // an installed-but-off app is not "not installed".
        crate::db::repository::set_addon_enabled(&state.db, &addon_a, false).expect("disable a");
        assert_eq!(
            sole_enabled_instance(&state.db, "benchmark-studio"),
            Err(SoleInstanceError::Disabled)
        );
    }

    #[test]
    fn sole_enabled_instance_distinguishes_disabled_from_ambiguous() {
        let state = AppState::for_test();
        let addon_a = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        let addon_b = test_support::install_app_instance(&state, "benchmark-studio", "b", &[]);

        // The fixture installs both instances enabled; disable both first so
        // the "installed but none enabled" case is observable.
        crate::db::repository::set_addon_enabled(&state.db, &addon_a, false).expect("disable a");
        crate::db::repository::set_addon_enabled(&state.db, &addon_b, false).expect("disable b");
        assert_eq!(
            sole_enabled_instance(&state.db, "benchmark-studio"),
            Err(SoleInstanceError::Disabled)
        );

        crate::db::repository::set_addon_enabled(&state.db, &addon_a, true).expect("enable a");
        assert_eq!(
            sole_enabled_instance(&state.db, "benchmark-studio"),
            Ok(addon_a.clone())
        );

        crate::db::repository::set_addon_enabled(&state.db, &addon_b, true).expect("enable b");
        assert_eq!(
            sole_enabled_instance(&state.db, "benchmark-studio"),
            Err(SoleInstanceError::Ambiguous(2))
        );
    }

    fn ctx_with_org_role(
        state: &Arc<AppState>,
        user_id: &str,
        is_org_admin: bool,
    ) -> HandlerContext {
        let mut permissions = std::collections::HashSet::new();
        if is_org_admin {
            permissions.insert("org.admin".to_string());
        }
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [7u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: "org-1".to_string(),
                role_id: "role-1".to_string(),
                permissions,
            }),
        }
    }

    #[test]
    fn require_instance_admin_denied_without_org_admin_role_even_with_matrix_grant() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        test_support::grant(&state, &addon_id, "user-1", "benchmark.write");
        let c = ctx_with_org_role(&state, "user-1", false);
        let err = require_instance_admin(&c, "benchmark-studio", &addon_id, "benchmark.write")
            .unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[test]
    fn require_instance_admin_denied_with_org_admin_role_but_no_matrix_grant() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        let c = ctx_with_org_role(&state, "user-1", true);
        let err = require_instance_admin(&c, "benchmark-studio", &addon_id, "benchmark.write")
            .unwrap_err();
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[test]
    fn require_instance_admin_grants_with_both_matrix_grant_and_org_admin_role() {
        let state = AppState::for_test();
        let addon_id = test_support::install_app_instance(&state, "benchmark-studio", "a", &[]);
        test_support::grant(&state, &addon_id, "user-1", "benchmark.write");
        let c = ctx_with_org_role(&state, "user-1", true);
        let resolved = require_instance_admin(&c, "benchmark-studio", &addon_id, "benchmark.write")
            .expect("double lock satisfied");
        assert_eq!(resolved, addon_id);
    }
}
