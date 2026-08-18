// =============================================================================
// Plik: tests/v1_authorize.rs
// Opis: Unit-level tests for the /v1 default-DENY authorization gate
//       (`authorize_model`) covering user / group / general (api_key) keys,
//       flow/alias resource kinds and per-Principal /v1/models filtering.
// =============================================================================

use std::sync::Arc;

use tentaflow_core::api::openai::server::{authorize_model, AuthDecision};
use tentaflow_core::auth::acl::Principal;
use tentaflow_core::db::DbPool;
use tentaflow_core::services::catalog::{
    CatalogEntry, CatalogEntryKind, CatalogSnapshot, Strategy,
};

fn open_pool() -> (tempfile::TempDir, DbPool) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("v1_authorize.db");
    let pool = tentaflow_core::db::init(&path).expect("db init");
    (dir, pool)
}

fn service_model(id: &str) -> CatalogEntry {
    CatalogEntry {
        reasoning_levels: Vec::new(),
        id: id.to_string(),
        kind: CatalogEntryKind::ServiceModel { instances: vec![] },
        service_surfaces: vec![],
        input_modalities: vec![],
        output_modalities: vec![],
        diagnostic: None,
    }
}

fn flow_entry(id: &str) -> CatalogEntry {
    CatalogEntry {
        reasoning_levels: Vec::new(),
        id: id.to_string(),
        kind: CatalogEntryKind::Flow {
            flow_id: id.to_string(),
            published_name: id.to_string(),
        },
        service_surfaces: vec![],
        input_modalities: vec![],
        output_modalities: vec![],
        diagnostic: None,
    }
}

fn alias_entry(id: &str, target: &str, fallbacks: &[&str]) -> CatalogEntry {
    CatalogEntry {
        reasoning_levels: Vec::new(),
        id: id.to_string(),
        kind: CatalogEntryKind::Alias {
            target: target.to_string(),
            fallback_targets: fallbacks.iter().map(|s| s.to_string()).collect(),
            strategy: Strategy::FirstAvailable,
        },
        service_surfaces: vec![],
        input_modalities: vec![],
        output_modalities: vec![],
        diagnostic: None,
    }
}

fn snapshot(entries: Vec<CatalogEntry>) -> CatalogSnapshot {
    CatalogSnapshot {
        entries: Arc::from(entries.into_boxed_slice()),
        version: 1,
    }
}

fn set_perm(
    pool: &DbPool,
    resource_type: &str,
    resource_id: &str,
    subject_type: &str,
    subject_id: &str,
    level: &str,
) {
    tentaflow_core::db::repository::resource_permissions::set(
        pool,
        resource_type,
        resource_id,
        subject_type,
        subject_id,
        level,
    )
    .expect("set perm");
}

fn user(uid: &str) -> Principal {
    Principal::User {
        user_id: uid.to_string(),
        role: "user".to_string(),
    }
}

// ---------------------------------------------------------------------------
// user-key
// ---------------------------------------------------------------------------

#[test]
fn user_key_model_allow_ok() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "m1"),
        AuthDecision::Allow
    );
}

#[test]
fn user_key_model_deny_denied() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    set_perm(&db, "model", "m1", "user", "u1", "deny");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "m1"),
        AuthDecision::Denied
    );
}

#[test]
fn user_key_model_no_rule_default_deny() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    // No rule at all → default-DENY on /v1.
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "m1"),
        AuthDecision::Denied
    );
}

#[test]
fn model_absent_from_catalog_is_not_in_catalog() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    set_perm(&db, "model", "ghost", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "ghost"),
        AuthDecision::ModelNotInCatalog
    );
}

#[test]
fn user_key_flow_gated() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![flow_entry("f1")]);
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "f1"),
        AuthDecision::Denied
    );
    set_perm(&db, "flow", "f1", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "f1"),
        AuthDecision::Allow
    );
}

#[test]
fn user_key_alias_allow_but_target_deny_denied() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![alias_entry("a1", "m1", &[]), service_model("m1")]);
    set_perm(&db, "alias", "a1", "user", "u1", "allow");
    // Target m1 has no allow → alias must not bypass target ACL.
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Denied
    );
}

#[test]
fn user_key_alias_and_target_allow_ok() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![alias_entry("a1", "m1", &[]), service_model("m1")]);
    set_perm(&db, "alias", "a1", "user", "u1", "allow");
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Allow
    );
}

#[test]
fn user_key_alias_fallback_target_deny_denied() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![
        alias_entry("a1", "m1", &["m2"]),
        service_model("m1"),
        service_model("m2"),
    ]);
    set_perm(&db, "alias", "a1", "user", "u1", "allow");
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    // m2 (fallback) not allowed → deny (conservative).
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Denied
    );
    set_perm(&db, "model", "m2", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Allow
    );
}

#[test]
fn user_key_alias_missing_target_denied() {
    let (_d, db) = open_pool();
    // Target not present in catalog → conservative deny even with alias allow.
    let snap = snapshot(vec![alias_entry("a1", "ghost", &[])]);
    set_perm(&db, "alias", "a1", "user", "u1", "allow");
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Denied
    );
}

// ---------------------------------------------------------------------------
// general-key (ApiKey) — own allowlist only, no admin-bypass
// ---------------------------------------------------------------------------

#[test]
fn general_key_only_allowlisted() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1"), service_model("m2")]);
    let p = Principal::ApiKey {
        uid: "k1".to_string(),
    };
    set_perm(&db, "model", "m1", "api_key", "k1", "allow");
    assert_eq!(authorize_model(&snap, &db, &p, "m1"), AuthDecision::Allow);
    // m2 not in this key's allowlist → deny.
    assert_eq!(authorize_model(&snap, &db, &p, "m2"), AuthDecision::Denied);
}

#[test]
fn general_key_no_admin_bypass() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    // An allow rule for a *user* must not leak to a general key.
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    let p = Principal::ApiKey {
        uid: "k1".to_string(),
    };
    assert_eq!(authorize_model(&snap, &db, &p, "m1"), AuthDecision::Denied);
}

// ---------------------------------------------------------------------------
// group-key
// ---------------------------------------------------------------------------

#[test]
fn group_key_rules_apply() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![service_model("m1")]);
    let p = Principal::Group {
        group_id: "g1".to_string(),
    };
    // No rule → deny.
    assert_eq!(authorize_model(&snap, &db, &p, "m1"), AuthDecision::Denied);
    set_perm(&db, "model", "m1", "group", "g1", "allow");
    assert_eq!(authorize_model(&snap, &db, &p, "m1"), AuthDecision::Allow);
    set_perm(&db, "model", "m1", "group", "g1", "deny");
    assert_eq!(authorize_model(&snap, &db, &p, "m1"), AuthDecision::Denied);
}

// ---------------------------------------------------------------------------
// /v1/models filtering via authorize_model (the same gate the handler uses, so
// an entry shown on the list is exactly one the principal can actually reach)
// ---------------------------------------------------------------------------

fn visible_ids(snap: &CatalogSnapshot, db: &DbPool, p: &Principal) -> Vec<String> {
    let mut ids: Vec<String> = snap
        .advertised_entries()
        .filter(|e| matches!(authorize_model(snap, db, p, &e.id), AuthDecision::Allow))
        .map(|e| e.id.clone())
        .collect();
    ids.sort();
    ids
}

#[test]
fn models_list_filters_per_principal() {
    let (_d, db) = open_pool();
    let snap = snapshot(vec![
        service_model("m1"),
        service_model("m2"),
        flow_entry("f1"),
    ]);

    // user u1 sees m1 + f1.
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    set_perm(&db, "flow", "f1", "user", "u1", "allow");
    assert_eq!(visible_ids(&snap, &db, &user("u1")), vec!["f1", "m1"]);

    // general key k1 sees only m2 — a different set.
    let k = Principal::ApiKey {
        uid: "k1".to_string(),
    };
    set_perm(&db, "model", "m2", "api_key", "k1", "allow");
    assert_eq!(visible_ids(&snap, &db, &k), vec!["m2"]);

    // unknown user with no rules sees nothing (fail-CLOSED default-DENY).
    assert!(visible_ids(&snap, &db, &user("nobody")).is_empty());
}

#[test]
fn models_list_hides_alias_when_target_denied() {
    // Consistency with the /v1 gate: an alias the caller is allowed on but
    // whose resolved target they are NOT allowed on must NOT appear on the
    // list — using it would 404, so advertising it is misleading and leaks
    // catalog structure. `authorize_model` (the gate) denies it, so the
    // models-list filter (same function) must hide it too. The plain target
    // `m1` itself, being denied, is also absent.
    let (_d, db) = open_pool();
    let snap = snapshot(vec![alias_entry("a1", "m1", &[]), service_model("m1")]);
    set_perm(&db, "alias", "a1", "user", "u1", "allow");
    // No allow on target m1 → alias denied at the gate.
    assert_eq!(
        authorize_model(&snap, &db, &user("u1"), "a1"),
        AuthDecision::Denied
    );
    assert!(visible_ids(&snap, &db, &user("u1")).is_empty());

    // Allowing the target too makes BOTH the alias and the target visible.
    set_perm(&db, "model", "m1", "user", "u1", "allow");
    assert_eq!(visible_ids(&snap, &db, &user("u1")), vec!["a1", "m1"]);
}
