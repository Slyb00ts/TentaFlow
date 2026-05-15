// =============================================================================
// File: tests/alias_host_functions.rs
// Purpose: Integration tests for readonly alias host functions (F1a M1.W5
//          Chunk C — alias_get_v1, alias_list_owned_v1). Drives the same
//          internal logic as the WASM host function wrappers via
//          `host_functions::aliases::test_api`, covering: get-with-stats,
//          list-owned filtering, payload limits, alias_id format validation,
//          cross-addon stat visibility.
//
//          Alias lifecycle (create/deactivate) is no longer addon-callable
//          — it runs at install/uninstall time via
//          `addon::install_manifest_aliases`. Setup here writes alias rows
//          directly through `repository::create_or_reactivate_model_alias_with_active`.
// =============================================================================

use serde_json::{json, Value};
use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::host_functions::aliases::test_api;
use tentaflow_core::db::repository::{
    create_or_reactivate_model_alias, create_or_reactivate_model_alias_with_active,
};
use tentaflow_core::db::DbPool;

// =============================================================================
// Test helpers
// =============================================================================

fn make_core_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("core db init")
}

/// Installs an addon-owned alias in the active state. Used by tests that
/// need a fixture before exercising the readonly host functions.
fn install_addon_alias(db: &DbPool, addon_id: &str, alias: &str, target: &str) {
    create_or_reactivate_model_alias_with_active(
        db,
        alias,
        target,
        "first_available",
        "addon",
        Some(addon_id),
        true,
    )
    .expect("install_addon_alias");
}

fn fetch_alias_row(db: &DbPool, alias: &str) -> Option<(i64, String, i64)> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, target_model, is_active FROM model_aliases WHERE alias = ?1",
        rusqlite::params![alias],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
    )
    .ok()
}

fn insert_alias_call(
    db: &DbPool,
    alias_id: i64,
    alias_name: &str,
    target_used: &str,
    fallback_used: bool,
    ts: i64,
) {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO alias_calls \
         (alias_id, alias_name, target_used, fallback_used, result, ts) \
         VALUES (?1, ?2, ?3, ?4, 'ok', ?5)",
        rusqlite::params![alias_id, alias_name, target_used, fallback_used as i64, ts],
    )
    .unwrap();
}

// =============================================================================
// alias_get
// =============================================================================

#[test]
fn alias_get_returns_full_info_with_stats() {
    let db = make_core_db();
    install_addon_alias(&db, "stat-addon", "stats-alias", "model-s");

    let alias_db_id = fetch_alias_row(&db, "stats-alias").unwrap().0;
    let now = chrono::Utc::now().timestamp();
    insert_alias_call(&db, alias_db_id, "stats-alias", "model-s", false, now - 10);
    insert_alias_call(&db, alias_db_id, "stats-alias", "model-s", false, now - 20);
    insert_alias_call(&db, alias_db_id, "stats-alias", "model-s", false, now - 30);
    insert_alias_call(&db, alias_db_id, "stats-alias", "model-s", true, now - 40);
    insert_alias_call(&db, alias_db_id, "stats-alias", "model-s", false, now - 5);

    let out = test_api::alias_get_internal(&db, "stats-alias", "stat-addon").unwrap();
    assert_eq!(out["id"], json!("stats-alias"));
    assert_eq!(out["owner"], json!("addon:stat-addon"));
    assert_eq!(out["current_target"], json!("model-s"));
    assert_eq!(out["strategy"], json!("first_available"));
    assert_eq!(out["is_active"], json!(true));
    assert_eq!(out["calls_24h"], json!(5));
    assert_eq!(out["fallback_calls_24h"], json!(1));
    assert_eq!(out["last_used_target"], json!("model-s"));
    assert!(out["last_used_at"].as_i64().is_some());
}

#[test]
fn alias_get_returns_empty_stats_when_no_calls() {
    let db = make_core_db();
    install_addon_alias(&db, "q-addon", "no-calls-alias", "model-q");

    let out = test_api::alias_get_internal(&db, "no-calls-alias", "q-addon").unwrap();
    assert_eq!(out["calls_24h"], json!(0));
    assert_eq!(out["fallback_calls_24h"], json!(0));
    assert_eq!(out["last_used_target"], Value::Null);
    assert_eq!(out["last_used_at"], Value::Null);
}

#[test]
fn alias_get_nonexistent_returns_not_found() {
    let db = make_core_db();
    let err = test_api::alias_get_internal(&db, "ghost-alias", "any-addon").unwrap_err();
    assert_eq!(err, AbiError::NotFound);
}

#[test]
fn alias_get_returns_owner_manual_for_manual_owned() {
    let db = make_core_db();
    create_or_reactivate_model_alias(&db, "admin-alias", "model-a", "first_available", "manual", None)
        .unwrap();

    let out = test_api::alias_get_internal(&db, "admin-alias", "any-addon").unwrap();
    assert_eq!(out["owner"], json!("manual"));
}

#[test]
fn alias_get_excludes_old_calls_from_24h_window() {
    let db = make_core_db();
    install_addon_alias(&db, "w-addon", "old-calls", "model-w");
    let alias_db_id = fetch_alias_row(&db, "old-calls").unwrap().0;

    let now = chrono::Utc::now().timestamp();
    insert_alias_call(&db, alias_db_id, "old-calls", "model-w", false, now - 60);
    insert_alias_call(&db, alias_db_id, "old-calls", "model-w", false, now - 90_000);
    insert_alias_call(&db, alias_db_id, "old-calls", "model-w", true, now - 90_000);

    let out = test_api::alias_get_internal(&db, "old-calls", "w-addon").unwrap();
    assert_eq!(out["calls_24h"], json!(1));
    assert_eq!(out["fallback_calls_24h"], json!(0));
}

// =============================================================================
// alias_list_owned
// =============================================================================

#[test]
fn alias_list_owned_returns_only_caller_aliases() {
    let db = make_core_db();
    install_addon_alias(&db, "addon-a", "a-one", "model-a1");
    install_addon_alias(&db, "addon-a", "a-two", "model-a2");
    install_addon_alias(&db, "addon-b", "b-one", "model-b1");

    let out_a = test_api::alias_list_owned_internal(&db, "addon-a").unwrap();
    let arr_a = out_a["aliases"].as_array().expect("array");
    assert_eq!(arr_a.len(), 2);
    let ids_a: Vec<&str> = arr_a
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(ids_a.contains(&"a-one"));
    assert!(ids_a.contains(&"a-two"));
    assert!(!ids_a.contains(&"b-one"));

    let out_b = test_api::alias_list_owned_internal(&db, "addon-b").unwrap();
    assert_eq!(out_b["aliases"].as_array().unwrap().len(), 1);
}

#[test]
fn alias_list_owned_empty_when_no_aliases() {
    let db = make_core_db();
    let out = test_api::alias_list_owned_internal(&db, "lonely-addon").unwrap();
    assert_eq!(out["aliases"].as_array().unwrap().len(), 0);
}

#[test]
fn alias_list_owned_excludes_manual_aliases() {
    let db = make_core_db();
    create_or_reactivate_model_alias(&db, "manual-x", "model-m", "first_available", "manual", None)
        .unwrap();
    install_addon_alias(&db, "addon-m", "addon-y", "model-y");

    let out = test_api::alias_list_owned_internal(&db, "addon-m").unwrap();
    let arr = out["aliases"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], json!("addon-y"));
}

// =============================================================================
// CR-002: stat visibility — addon stats are private to the owner
// =============================================================================

#[test]
fn alias_get_strips_stats_for_cross_addon_caller() {
    let db = make_core_db();
    install_addon_alias(&db, "owner-addon", "private-stats", "model-p");

    let alias_db_id = fetch_alias_row(&db, "private-stats").unwrap().0;
    let now = chrono::Utc::now().timestamp();
    insert_alias_call(&db, alias_db_id, "private-stats", "model-p", false, now - 10);
    insert_alias_call(&db, alias_db_id, "private-stats", "model-p", true, now - 20);

    let owner_view =
        test_api::alias_get_internal(&db, "private-stats", "owner-addon").unwrap();
    assert_eq!(owner_view["calls_24h"], json!(2));
    assert_eq!(owner_view["fallback_calls_24h"], json!(1));
    assert_eq!(owner_view["last_used_target"], json!("model-p"));
    assert!(owner_view["last_used_at"].as_i64().is_some());

    let stranger_view =
        test_api::alias_get_internal(&db, "private-stats", "snooper-addon").unwrap();
    assert_eq!(stranger_view["id"], json!("private-stats"));
    assert_eq!(stranger_view["owner"], json!("addon:owner-addon"));
    assert_eq!(stranger_view["current_target"], json!("model-p"));
    assert_eq!(stranger_view["is_active"], json!(true));
    assert_eq!(stranger_view["calls_24h"], json!(0));
    assert_eq!(stranger_view["fallback_calls_24h"], json!(0));
    assert_eq!(stranger_view["last_used_target"], Value::Null);
    assert_eq!(stranger_view["last_used_at"], Value::Null);
}

#[test]
fn alias_get_manual_alias_stats_visible_to_any_caller() {
    let db = make_core_db();
    create_or_reactivate_model_alias(
        &db,
        "manual-stats",
        "model-ms",
        "first_available",
        "manual",
        None,
    )
    .unwrap();

    let alias_db_id = fetch_alias_row(&db, "manual-stats").unwrap().0;
    let now = chrono::Utc::now().timestamp();
    insert_alias_call(&db, alias_db_id, "manual-stats", "model-ms", false, now - 5);
    insert_alias_call(&db, alias_db_id, "manual-stats", "model-ms", true, now - 15);

    let view = test_api::alias_get_internal(&db, "manual-stats", "random-addon").unwrap();
    assert_eq!(view["owner"], json!("manual"));
    assert_eq!(view["calls_24h"], json!(2));
    assert_eq!(view["fallback_calls_24h"], json!(1));
    assert_eq!(view["last_used_target"], json!("model-ms"));
}

// =============================================================================
// CR-007: payload size enforcement reaches do_alias_* (defense in depth)
// =============================================================================

#[test]
fn alias_get_payload_too_large_rejected() {
    let db = make_core_db();
    let huge = "a".repeat(5 * 1024 * 1024);
    let err = test_api::alias_get_internal(&db, &huge, "addon-x").unwrap_err();
    assert_eq!(err, AbiError::PayloadTooLarge);
}
