// =============================================================================
// File: tests/install_flow_e2e.rs — F1a M2.W11 install + resolver integration
// =============================================================================
//
// Scope (precise — earlier revisions over-claimed):
//   * Covers `install_manifest_aliases()` end-to-end: the manifest-driven
//     transactional path that writes model_aliases / model_alias_owners /
//     model_alias_visibility / model_alias_consumers / addon_uses_alias /
//     addon_uses_model plus the reconciliation audit (model_alias_changes /
//     audit_log).
//   * Covers two TOML round-trip cases via `lifecycle::parse_manifest_toml`
//     -> `install_manifest_aliases` so reviewers see the exact entry point
//     a real `install_addon()` invocation takes (minus WASM load).
//   * Covers the resolver gate `repository::resolve_model_alias_for_addon`
//     with `caller_addon_id = Some(...)`, asserting both the granted and
//     denied paths produce the right outcome (Ok(target) vs
//     AliasPermissionDenied + `alias_calls` row with
//     `result='permission_denied'`).
//
// Out of scope (deliberately not claimed): full `install_addon(path)` —
// that requires a WASM artifact on disk plus wasmtime instantiation, which
// is exercised by the camera/recording WASM e2e suites instead.

use std::sync::Arc;

use tentaflow_core::addon::lifecycle::parse_manifest_toml;
use tentaflow_core::addon::manifest::{AliasSpec, AliasVisibility, UsesAliasSpec, UsesModelSpec};
use tentaflow_core::addon::{AddonManager, AddonManifest};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::repository::resolve_model_alias_for_addon;
use tentaflow_core::db::DbPool;

// =============================================================================
// Fixtures
// =============================================================================

fn make_manager() -> (AddonManager, DbPool) {
    let db: DbPool =
        tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("init core db");
    let cipher = Arc::new(SettingsCipher::new(&[0u8; 32]));
    let mgr = AddonManager::new(db.clone(), cipher).expect("AddonManager::new");
    (mgr, db)
}

fn manifest_with(
    addon_id: &str,
    aliases: Vec<AliasSpec>,
    uses_aliases: Vec<UsesAliasSpec>,
    uses_models: Vec<UsesModelSpec>,
) -> AddonManifest {
    AddonManifest {
        addon_id: addon_id.to_string(),
        version: "1.0.0".to_string(),
        display_name: addon_id.to_string(),
        wasm_file: "addon.wasm".to_string(),
        aliases,
        uses_aliases,
        uses_models,
        ..AddonManifest::default()
    }
}

fn alias(id: &str, target: &str, visibility: AliasVisibility, consumers: Vec<String>) -> AliasSpec {
    AliasSpec {
        id: id.to_string(),
        display_name: id.to_string(),
        methods: vec![],
        suggested_default: target.to_string(),
        gate: None,
        visibility,
        allowed_consumers: consumers,
    }
}

fn uses_alias(id: &str, required: bool, reason: &str) -> UsesAliasSpec {
    UsesAliasSpec {
        id: id.to_string(),
        required,
        reason: reason.to_string(),
    }
}

fn uses_model(id: &str, required: bool, reason: &str) -> UsesModelSpec {
    UsesModelSpec {
        id: id.to_string(),
        required,
        reason: reason.to_string(),
    }
}

fn count_no_params(db: &DbPool, sql: &str) -> i64 {
    let conn = db.lock().unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

fn grant_status(db: &DbPool, addon_id: &str, alias_target_name: &str) -> Option<String> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT grant_status FROM addon_uses_alias \
         WHERE addon_id = ?1 AND alias_target_name = ?2",
        rusqlite::params![addon_id, alias_target_name],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

// =============================================================================
// 1. Owner install registers alias + ownership + visibility rows.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn owner_install_writes_alias_owner_and_visibility_rows() {
    let (mgr, db) = make_manager();

    let manifest = manifest_with(
        "owner-addon",
        vec![
            alias("public-one", "model-pub", AliasVisibility::Public, vec![]),
            alias(
                "restricted-one",
                "model-restricted",
                AliasVisibility::Restricted,
                vec!["consumer-x".to_string(), "consumer-y".to_string()],
            ),
        ],
        vec![],
        vec![],
    );

    mgr.install_manifest_aliases(&manifest)
        .expect("install ok");

    // model_aliases: dwa wpisy z poprawnym targetem.
    let n_aliases = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_aliases \
         WHERE alias IN ('public-one','restricted-one')",
    );
    assert_eq!(n_aliases, 2);

    // model_alias_owners: oba przypisane do owner-addon.
    let n_owners = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_alias_owners \
         WHERE owner_type='addon' AND owner_id='owner-addon'",
    );
    assert_eq!(n_owners, 2);

    // model_alias_visibility: public + restricted.
    let visibilities: Vec<(String, String)> = {
        let conn = db.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ma.alias, mv.visibility \
                 FROM model_aliases ma \
                 JOIN model_alias_visibility mv ON mv.alias_id = ma.id \
                 WHERE ma.alias IN ('public-one','restricted-one') \
                 ORDER BY ma.alias",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    };
    assert_eq!(
        visibilities,
        vec![
            ("public-one".to_string(), "public".to_string()),
            ("restricted-one".to_string(), "restricted".to_string())
        ]
    );

    // model_alias_consumers: dwie whitelisty dla restricted, zero dla public.
    let n_consumers = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_alias_consumers c \
         JOIN model_aliases a ON a.id = c.alias_id \
         WHERE a.alias = 'restricted-one'",
    );
    assert_eq!(n_consumers, 2);
}

// =============================================================================
// 2. uses_alias against an unknown alias stays pending.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn uses_alias_unknown_target_is_pending() {
    let (mgr, db) = make_manager();

    let manifest = manifest_with(
        "consumer-addon",
        vec![],
        vec![uses_alias("not-yet-installed", false, "reason")],
        vec![],
    );
    mgr.install_manifest_aliases(&manifest).expect("install ok");

    assert_eq!(
        grant_status(&db, "consumer-addon", "not-yet-installed").as_deref(),
        Some("pending")
    );
}

// =============================================================================
// 3. Required uses_alias against unknown target rejects install.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn required_uses_alias_unknown_target_rejects_install() {
    let (mgr, _db) = make_manager();

    let manifest = manifest_with(
        "needy-addon",
        vec![],
        vec![uses_alias(
            "missing-alias",
            true,
            "blocks_start_without_it",
        )],
        vec![],
    );

    let err = mgr.install_manifest_aliases(&manifest).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing-alias") && msg.contains("install rejected"),
        "expected rejection error, got: {msg}"
    );
}

// =============================================================================
// 4. Public alias + consumer uses_alias → auto_granted after reconcile.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn public_alias_consumer_reconciles_to_auto_granted() {
    let (mgr, db) = make_manager();

    // Step 1: consumer installs first — alias does not exist yet so the
    // row lands as 'pending'.
    let consumer_manifest = manifest_with(
        "consumer-public",
        vec![],
        vec![uses_alias("provider-alias", false, "needs it")],
        vec![],
    );
    mgr.install_manifest_aliases(&consumer_manifest)
        .expect("consumer install");
    assert_eq!(
        grant_status(&db, "consumer-public", "provider-alias").as_deref(),
        Some("pending"),
        "before provider installs, status must be pending"
    );

    // Step 2: provider installs its public alias — the reconcile step in
    // install_manifest_aliases flips every consumer pointing at this alias.
    let provider_manifest = manifest_with(
        "provider-public",
        vec![alias(
            "provider-alias",
            "model-p",
            AliasVisibility::Public,
            vec![],
        )],
        vec![],
        vec![],
    );
    mgr.install_manifest_aliases(&provider_manifest)
        .expect("provider install");

    assert_eq!(
        grant_status(&db, "consumer-public", "provider-alias").as_deref(),
        Some("auto_granted")
    );
}

// =============================================================================
// 5. Restricted alias + whitelisted consumer → granted; non-whitelisted → pending.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn restricted_alias_only_whitelisted_consumer_becomes_granted() {
    let (mgr, db) = make_manager();

    // Two consumers pre-declare uses on the same alias name.
    mgr.install_manifest_aliases(&manifest_with(
        "whitelisted-consumer",
        vec![],
        vec![uses_alias("restricted-foo", false, "ok")],
        vec![],
    ))
    .unwrap();
    mgr.install_manifest_aliases(&manifest_with(
        "outsider-consumer",
        vec![],
        vec![uses_alias("restricted-foo", false, "ok")],
        vec![],
    ))
    .unwrap();

    // Provider installs restricted alias with only whitelisted-consumer
    // on the allowed_consumers list.
    mgr.install_manifest_aliases(&manifest_with(
        "restricted-provider",
        vec![alias(
            "restricted-foo",
            "model-r",
            AliasVisibility::Restricted,
            vec!["whitelisted-consumer".to_string()],
        )],
        vec![],
        vec![],
    ))
    .unwrap();

    assert_eq!(
        grant_status(&db, "whitelisted-consumer", "restricted-foo").as_deref(),
        Some("granted")
    );
    assert_eq!(
        grant_status(&db, "outsider-consumer", "restricted-foo").as_deref(),
        Some("pending")
    );
}

// =============================================================================
// 6. After install, runtime alias resolve respects the grant.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn runtime_resolve_for_granted_consumer_succeeds() {
    let (mgr, db) = make_manager();

    // Provider with a public alias.
    mgr.install_manifest_aliases(&manifest_with(
        "owner-r",
        vec![alias(
            "shared-alias",
            "model-shared",
            AliasVisibility::Public,
            vec![],
        )],
        vec![],
        vec![],
    ))
    .unwrap();

    // Consumer declares uses; reconcile flips it to auto_granted.
    mgr.install_manifest_aliases(&manifest_with(
        "consumer-r",
        vec![],
        vec![uses_alias("shared-alias", false, "need")],
        vec![],
    ))
    .unwrap();

    // Owner can always resolve its own alias.
    let owner_target =
        resolve_model_alias_for_addon(&db, "shared-alias", Some("owner-r"), None, None).expect("owner ok");
    assert!(
        owner_target.is_some(),
        "owner must be able to resolve its own alias"
    );

    // Consumer can resolve because grant_status is auto_granted.
    let consumer_target =
        resolve_model_alias_for_addon(&db, "shared-alias", Some("consumer-r"), None, None)
            .expect("consumer ok");
    assert!(
        consumer_target.is_some(),
        "auto_granted consumer must be able to resolve"
    );

    // Unrelated addon without a uses_alias row must NOT resolve.
    let stranger =
        resolve_model_alias_for_addon(&db, "shared-alias", Some("stranger"), None, None);
    assert!(
        stranger.is_err() || stranger.unwrap().is_none(),
        "stranger addon without uses_alias row must not resolve"
    );
}

// =============================================================================
// 7. Reinstall with a smaller allowed_consumers list revokes the dropped one.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn reinstall_with_shrunk_whitelist_revokes_obsolete_consumer() {
    let (mgr, db) = make_manager();

    mgr.install_manifest_aliases(&manifest_with(
        "consumer-a",
        vec![],
        vec![uses_alias("evolving-alias", false, "x")],
        vec![],
    ))
    .unwrap();
    mgr.install_manifest_aliases(&manifest_with(
        "consumer-b",
        vec![],
        vec![uses_alias("evolving-alias", false, "x")],
        vec![],
    ))
    .unwrap();

    // First install: both consumers whitelisted.
    mgr.install_manifest_aliases(&manifest_with(
        "evolver",
        vec![alias(
            "evolving-alias",
            "model-e",
            AliasVisibility::Restricted,
            vec!["consumer-a".to_string(), "consumer-b".to_string()],
        )],
        vec![],
        vec![],
    ))
    .unwrap();
    assert_eq!(
        grant_status(&db, "consumer-a", "evolving-alias").as_deref(),
        Some("granted")
    );
    assert_eq!(
        grant_status(&db, "consumer-b", "evolving-alias").as_deref(),
        Some("granted")
    );

    // Reinstall: consumer-b dropped from whitelist.
    mgr.install_manifest_aliases(&manifest_with(
        "evolver",
        vec![alias(
            "evolving-alias",
            "model-e",
            AliasVisibility::Restricted,
            vec!["consumer-a".to_string()],
        )],
        vec![],
        vec![],
    ))
    .unwrap();

    // consumer-a still granted; consumer-b revoked back to pending.
    assert_eq!(
        grant_status(&db, "consumer-a", "evolving-alias").as_deref(),
        Some("granted")
    );
    assert_eq!(
        grant_status(&db, "consumer-b", "evolving-alias").as_deref(),
        Some("pending"),
        "dropping consumer-b from the whitelist must revoke its grant"
    );
}

// =============================================================================
// 8. uses_model declarations are persisted with the right status.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn uses_model_pending_for_unknown_model() {
    let (mgr, db) = make_manager();

    mgr.install_manifest_aliases(&manifest_with(
        "model-consumer",
        vec![],
        vec![],
        vec![uses_model("unknown-model-xyz", false, "want it")],
    ))
    .unwrap();

    let status: String = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT grant_status FROM addon_uses_model \
             WHERE addon_id = 'model-consumer' AND model_target_name = 'unknown-model-xyz'",
            [],
            |r| r.get(0),
        )
        .expect("addon_uses_model row missing")
    };
    assert_eq!(status, "pending");
}

// =============================================================================
// 9. Reconciliation transitions are audited in model_alias_changes.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn reconciliation_writes_audit_row() {
    let (mgr, db) = make_manager();

    mgr.install_manifest_aliases(&manifest_with(
        "consumer-aud",
        vec![],
        vec![uses_alias("audited-alias", false, "x")],
        vec![],
    ))
    .unwrap();

    mgr.install_manifest_aliases(&manifest_with(
        "owner-aud",
        vec![alias(
            "audited-alias",
            "model-a",
            AliasVisibility::Public,
            vec![],
        )],
        vec![],
        vec![],
    ))
    .unwrap();

    // model_alias_changes (or audit_log) should contain a row tied to the
    // reconcile transition. Repository helper writes either; we accept any
    // observable trace.
    let changes = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_alias_changes \
         WHERE alias_name = 'audited-alias'",
    );
    assert!(
        changes >= 1,
        "expected >=1 audit entries in model_alias_changes, got {changes}"
    );
}

// =============================================================================
// 10. TOML round-trip — owner manifest parses, then installs cleanly.
// =============================================================================
//
// Proves the TOML format documented in addon-author-facing docs lines up with
// the in-memory `AddonManifest` shape consumed by `install_manifest_aliases`.

#[tokio::test(flavor = "current_thread")]
async fn toml_round_trip_owner_manifest_installs() {
    let (mgr, db) = make_manager();

    let manifest_toml = r#"
[addon]
id = "toml-owner"
name = "TOML Owner"
version = "1.0.0"
wasm_file = "addon.wasm"

[[alias]]
id = "toml-public"
display_name = "TOML Public Alias"
suggested_default = "model-toml-pub"
visibility = "public"

[[alias]]
id = "toml-restricted"
display_name = "TOML Restricted Alias"
suggested_default = "model-toml-r"
visibility = "restricted"
allowed_consumers = ["whitelisted-toml"]
"#;

    let manifest = parse_manifest_toml(manifest_toml).expect("parse TOML manifest");
    assert_eq!(manifest.addon_id, "toml-owner");
    assert_eq!(manifest.aliases.len(), 2);

    mgr.install_manifest_aliases(&manifest)
        .expect("install parsed manifest");

    let n_aliases = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_aliases \
         WHERE alias IN ('toml-public','toml-restricted')",
    );
    assert_eq!(n_aliases, 2, "both parsed aliases must land in DB");

    let n_consumers = count_no_params(
        &db,
        "SELECT COUNT(*) FROM model_alias_consumers c \
         JOIN model_aliases a ON a.id = c.alias_id \
         WHERE a.alias = 'toml-restricted'",
    );
    assert_eq!(n_consumers, 1, "restricted whitelist must persist");
}

// =============================================================================
// 11. TOML round-trip — consumer manifest with uses_alias.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn toml_round_trip_consumer_manifest_installs_pending() {
    let (mgr, db) = make_manager();

    let manifest_toml = r#"
[addon]
id = "toml-consumer"
name = "TOML Consumer"
version = "1.0.0"
wasm_file = "addon.wasm"

[[uses_alias]]
id = "future-alias"
required = false
reason = "needs upstream alias"
"#;

    let manifest = parse_manifest_toml(manifest_toml).expect("parse consumer TOML");
    assert_eq!(manifest.uses_aliases.len(), 1);
    assert_eq!(manifest.uses_aliases[0].id, "future-alias");

    mgr.install_manifest_aliases(&manifest)
        .expect("install consumer-only TOML manifest");

    assert_eq!(
        grant_status(&db, "toml-consumer", "future-alias").as_deref(),
        Some("pending"),
        "uses_alias against missing target must land as 'pending'"
    );
}

// =============================================================================
// 12. Resolver gate — after install + reconcile, granted consumer resolves.
// =============================================================================
//
// Closes the loop: install owner + consumer through install_manifest_aliases,
// then exercise `resolve_model_alias_for_addon` with the consumer's id and
// assert the gate returns the target. This is the production codepath every
// runtime alias resolution takes.

#[tokio::test(flavor = "current_thread")]
async fn full_flow_resolver_grants_access_after_install_and_reconcile() {
    let (mgr, db) = make_manager();

    // Owner publishes a public alias.
    mgr.install_manifest_aliases(&manifest_with(
        "addon-a",
        vec![alias(
            "shared-stt",
            "model-stt",
            AliasVisibility::Public,
            vec![],
        )],
        vec![],
        vec![],
    ))
    .expect("owner install");

    // Consumer declares uses_alias against the public alias; reconcile flips
    // grant_status to 'auto_granted' inside the same install call.
    mgr.install_manifest_aliases(&manifest_with(
        "addon-b",
        vec![],
        vec![uses_alias("shared-stt", true, "needs STT")],
        vec![],
    ))
    .expect("consumer install");

    assert_eq!(
        grant_status(&db, "addon-b", "shared-stt").as_deref(),
        Some("auto_granted"),
        "consumer must be auto_granted after reconcile"
    );

    // Resolver path: consumer is allowed through the gate.
    let resolved = resolve_model_alias_for_addon(
        &db,
        "shared-stt",
        Some("addon-b"),
        Some("infer"),
        Some("req-grant-1"),
    )
    .expect("resolver must return Ok for granted consumer");
    let target = resolved.expect("alias row must exist");
    assert_eq!(target.alias, "shared-stt");
    assert_eq!(target.target_model, "model-stt");
}

// =============================================================================
// 13. Resolver gate — undeclared consumer is denied and logged.
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn full_flow_resolver_denies_access_without_uses_alias_declaration() {
    let (mgr, db) = make_manager();

    // Owner publishes a public alias.
    mgr.install_manifest_aliases(&manifest_with(
        "addon-a",
        vec![alias(
            "shared-stt2",
            "model-stt2",
            AliasVisibility::Public,
            vec![],
        )],
        vec![],
        vec![],
    ))
    .expect("owner install");

    // Addon-c never declares uses_alias for shared-stt2 — gate must reject.
    let err = resolve_model_alias_for_addon(
        &db,
        "shared-stt2",
        Some("addon-c"),
        Some("infer"),
        Some("req-deny-1"),
    )
    .expect_err("resolver must deny undeclared consumer");

    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("denied")
            || msg.contains("permission")
            || msg.contains("public_no_uses"),
        "expected permission-denied error, got: {msg}"
    );

    // `alias_calls` must contain a permission_denied row for this attempt.
    let denied_rows = count_no_params(
        &db,
        "SELECT COUNT(*) FROM alias_calls \
         WHERE alias_name = 'shared-stt2' \
           AND caller_addon_id = 'addon-c' \
           AND result = 'permission_denied'",
    );
    assert!(
        denied_rows >= 1,
        "expected >=1 permission_denied row in alias_calls, got {denied_rows}"
    );
}
