// =============================================================================
// File: tests/directory_host_functions.rs
// Purpose: Integration tests for the read-only directory host functions
//          (directory_users_v1 / directory_groups_v1 / directory_roles_v1 /
//          directory_org_v1). Drives the same query layer as the WASM ABI
//          shells via `host_functions::directory::test_api`, covering: org
//          scoping of the user list, inactive-user exclusion, org-scoped
//          group member counts, role listing, org lookup, the CBOR wire
//          shape, and the `directory.read` permission gate (declared /
//          undeclared / system-call).
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex as PlMutex;

use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::check_permission;
use tentaflow_core::addon::host_functions::directory::{test_api, PERM_DIRECTORY_READ};
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::db::{init as db_init, repository, DbPool};
use tentaflow_sdk_spec::{DirectoryOrgOutput, DirectoryUsersOutput};

// =============================================================================
// Test helpers
// =============================================================================

fn make_core_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("core db init")
}

fn create_user(db: &DbPool, username: &str, email: &str) -> String {
    repository::create_user_account(db, username, "hash", username, email).expect("create user")
}

fn viewer_role_id(db: &DbPool) -> String {
    tentaflow_core::services::org::repo::list_roles(db)
        .expect("list roles")
        .into_iter()
        .find(|r| r.name == "org_viewer")
        .expect("org_viewer preseed role")
        .role_id
}

fn add_org_member(db: &DbPool, org_id: &str, user_id: &str) {
    let role = viewer_role_id(db);
    tentaflow_core::services::org::repo::add_membership(db, org_id, user_id, &role, "test")
        .expect("add membership");
}

fn create_org(db: &DbPool, name: &str, slug: &str) -> String {
    tentaflow_core::services::org::repo::create_organization(db, name, slug, None, None, None, None)
        .expect("create org")
        .org_id
}

/// Mirrors how a pooled worker's `AddonState` is configured for a call —
/// only the fields relevant to `check_permission` matter here.
fn make_state(
    db: DbPool,
    declared: Vec<String>,
    user_id: Option<String>,
    is_system_call: bool,
) -> AddonState {
    AddonState {
        addon_id: "directory-test-addon".to_string(),
        instance_id: "directory-test-instance".to_string(),
        user_id,
        org_id: None,
        db: db.clone(),
        permissions: declared,
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db)),
        fuel_consumed: 0,
        is_system_call,
        rate_limiter: None,
        net_manager: Arc::new(PlMutex::new(
            tentaflow_core::addon::host_functions::network::NetworkConnectionManager::new(),
        )),
        settings_cipher: Arc::new(tentaflow_core::crypto::SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 64 * 1024 * 1024,
        oauth_refresh_guard: Arc::new(
            tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
        ),
        router: None,
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

// =============================================================================
// directory_users — org scoping
// =============================================================================

#[test]
fn users_are_scoped_to_the_caller_org() {
    let db = make_core_db();
    let org_a = create_org(&db, "Org A", "org-a");
    let org_b = create_org(&db, "Org B", "org-b");

    let u1 = create_user(&db, "alice", "alice@x");
    let u2 = create_user(&db, "bob", "bob@x");
    let u3 = create_user(&db, "carol", "carol@x");
    add_org_member(&db, &org_a, &u1);
    add_org_member(&db, &org_a, &u2);
    add_org_member(&db, &org_b, &u3);

    let out = test_api::users(&db, &org_a).unwrap();
    let usernames: Vec<&str> = out.users.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(usernames, vec!["alice", "bob"]);

    let out_b = test_api::users(&db, &org_b).unwrap();
    let usernames_b: Vec<&str> = out_b.users.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(usernames_b, vec!["carol"]);
}

#[test]
fn inactive_users_are_excluded() {
    let db = make_core_db();
    let org = create_org(&db, "Org", "org");
    let u1 = create_user(&db, "active-user", "a@x");
    let u2 = create_user(&db, "disabled-user", "d@x");
    add_org_member(&db, &org, &u1);
    add_org_member(&db, &org, &u2);
    repository::update_user_account(&db, &u2, "disabled-user", "d@x", false).unwrap();

    let out = test_api::users(&db, &org).unwrap();
    assert_eq!(out.users.len(), 1);
    assert_eq!(out.users[0].username, "active-user");
    assert!(out.users[0].is_active);
}

#[test]
fn users_carry_rbac_role() {
    let db = make_core_db();
    let org = create_org(&db, "Org", "org");
    let admin = create_user(&db, "boss", "boss@x");
    let plain = create_user(&db, "member", "member@x");
    add_org_member(&db, &org, &admin);
    add_org_member(&db, &org, &plain);
    repository::set_user_role(&db, &admin, "admin").expect("set admin role");

    let out = test_api::users(&db, &org).unwrap();
    let boss = out.users.iter().find(|u| u.username == "boss").unwrap();
    assert_eq!(boss.role, "admin");
    // Fresh accounts default to the `user` role.
    let member = out.users.iter().find(|u| u.username == "member").unwrap();
    assert_eq!(member.role, "user");
}

#[test]
fn users_carry_group_ids_and_no_credentials() {
    let db = make_core_db();
    let org = create_org(&db, "Org", "org");
    let u1 = create_user(&db, "grouped", "g@x");
    add_org_member(&db, &org, &u1);
    let g1 = repository::create_group(&db, "developers", "Dev team").unwrap();
    let g2 = repository::create_group(&db, "testers", "QA").unwrap();
    repository::add_user_to_group(&db, &g1, &u1).unwrap();
    repository::add_user_to_group(&db, &g2, &u1).unwrap();

    let out = test_api::users(&db, &org).unwrap();
    assert_eq!(out.users.len(), 1);
    let mut groups = out.users[0].groups.clone();
    groups.sort();
    let mut expected = vec![g1, g2];
    expected.sort();
    assert_eq!(groups, expected);
    assert_eq!(out.users[0].email.as_deref(), Some("g@x"));
}

// =============================================================================
// directory_groups — org-scoped member counts
// =============================================================================

#[test]
fn group_member_count_is_scoped_to_org_and_active_users() {
    let db = make_core_db();
    let org_a = create_org(&db, "Org A", "org-a");
    let org_b = create_org(&db, "Org B", "org-b");

    let u1 = create_user(&db, "a1", "a1@x");
    let u2 = create_user(&db, "a2", "a2@x");
    let u3 = create_user(&db, "b1", "b1@x");
    add_org_member(&db, &org_a, &u1);
    add_org_member(&db, &org_a, &u2);
    add_org_member(&db, &org_b, &u3);

    let g = repository::create_group(&db, "mixed", "Cross-org group").unwrap();
    repository::add_user_to_group(&db, &g, &u1).unwrap();
    repository::add_user_to_group(&db, &g, &u2).unwrap();
    repository::add_user_to_group(&db, &g, &u3).unwrap();
    // Deactivating u2 must drop it from org A's count.
    repository::update_user_account(&db, &u2, "a2", "a2@x", false).unwrap();

    let out_a = test_api::groups(&db, &org_a).unwrap();
    let mixed_a = out_a.groups.iter().find(|x| x.id == g).unwrap();
    assert_eq!(mixed_a.member_count, 1);
    assert_eq!(mixed_a.name, "mixed");
    assert_eq!(mixed_a.description, "Cross-org group");

    let out_b = test_api::groups(&db, &org_b).unwrap();
    let mixed_b = out_b.groups.iter().find(|x| x.id == g).unwrap();
    assert_eq!(mixed_b.member_count, 1);
}

#[test]
fn groups_without_members_in_caller_org_are_not_returned() {
    let db = make_core_db();
    let org_a = create_org(&db, "Org A", "org-a");
    let org_b = create_org(&db, "Org B", "org-b");

    let u_a = create_user(&db, "a-only", "a@x");
    add_org_member(&db, &org_a, &u_a);
    let g_a = repository::create_group(&db, "org-a-secret", "Org A internal").unwrap();
    repository::add_user_to_group(&db, &g_a, &u_a).unwrap();
    let g_empty = repository::create_group(&db, "empty-group", "No members at all").unwrap();

    // Org A sees its own group, never the empty one.
    let out_a = test_api::groups(&db, &org_a).unwrap();
    let ids_a: Vec<&str> = out_a.groups.iter().map(|x| x.id.as_str()).collect();
    assert!(ids_a.contains(&g_a.as_str()));
    assert!(!ids_a.contains(&g_empty.as_str()));

    // Org B has no members in either group — neither name/description may
    // leak cross-tenant (not even with a zero count).
    let out_b = test_api::groups(&db, &org_b).unwrap();
    let ids_b: Vec<&str> = out_b.groups.iter().map(|x| x.id.as_str()).collect();
    assert!(!ids_b.contains(&g_a.as_str()));
    assert!(!ids_b.contains(&g_empty.as_str()));
}

// =============================================================================
// directory_roles / directory_org
// =============================================================================

#[test]
fn roles_lists_preseed_roles_without_permissions() {
    let db = make_core_db();
    let out = test_api::roles(&db).unwrap();
    let names: Vec<&str> = out.roles.iter().map(|r| r.name.as_str()).collect();
    for expected in [
        "org_admin",
        "org_operator",
        "org_viewer",
        "dpo",
        "supervisor",
    ] {
        assert!(names.contains(&expected), "missing role {expected}");
    }
}

#[test]
fn org_returns_default_org_and_not_found_for_ghost() {
    let db = make_core_db();
    let out = test_api::org(&db, tentaflow_core::services::org::DEFAULT_ORG_ID).unwrap();
    assert_eq!(out.org_id, "org-default");
    assert_eq!(out.slug, "default");
    assert!(!out.name.is_empty());

    let err = test_api::org(&db, "ghost-org").unwrap_err();
    assert_eq!(err, AbiError::NotFound);
}

// =============================================================================
// Permission gate — directory.read
// =============================================================================

#[test]
fn undeclared_permission_is_denied_even_for_system_calls() {
    let db = make_core_db();
    let state = make_state(db, vec![], None, true);
    assert!(
        !check_permission(&state, PERM_DIRECTORY_READ, None),
        "an addon that never declared directory.read must be denied"
    );
}

#[test]
fn declared_permission_is_granted_for_system_calls() {
    let db = make_core_db();
    let state = make_state(db, vec![PERM_DIRECTORY_READ.to_string()], None, true);
    assert!(
        check_permission(&state, PERM_DIRECTORY_READ, None),
        "a system call with the declared permission must pass (CR-006)"
    );
}

#[test]
fn declared_permission_without_user_grant_is_denied() {
    let db = make_core_db();
    let state = make_state(
        db,
        vec![PERM_DIRECTORY_READ.to_string()],
        Some("user-without-grant".to_string()),
        false,
    );
    assert!(
        !check_permission(&state, PERM_DIRECTORY_READ, None),
        "a real principal still needs a per-user grant"
    );
}

#[test]
fn missing_user_without_system_flag_is_denied() {
    let db = make_core_db();
    let state = make_state(db, vec![PERM_DIRECTORY_READ.to_string()], None, false);
    assert!(
        !check_permission(&state, PERM_DIRECTORY_READ, None),
        "user_id=None without is_system_call must not pass"
    );
}

// =============================================================================
// CBOR wire shape
// =============================================================================

#[test]
fn outputs_roundtrip_through_the_spec_cbor_shapes() {
    let db = make_core_db();
    let org = create_org(&db, "Wire", "wire");
    let u = create_user(&db, "wire-user", "w@x");
    add_org_member(&db, &org, &u);

    let users = test_api::users(&db, &org).unwrap();
    let bytes = minicbor::to_vec(&users).unwrap();
    let decoded: DirectoryUsersOutput = minicbor::decode(&bytes).unwrap();
    assert_eq!(decoded, users);
    assert_eq!(decoded.users[0].username, "wire-user");

    let org_out = test_api::org(&db, &org).unwrap();
    let bytes = minicbor::to_vec(&org_out).unwrap();
    let decoded: DirectoryOrgOutput = minicbor::decode(&bytes).unwrap();
    assert_eq!(decoded, org_out);
    assert_eq!(decoded.slug, "wire");
}
