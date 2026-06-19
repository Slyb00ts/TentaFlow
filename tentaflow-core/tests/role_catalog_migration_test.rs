// =============================================================================
// File: tests/role_catalog_migration_test.rs
// Purpose: Weryfikuje migracje v40 (platform_locales) i v41 (role_catalog) —
//          poprawnosc schematu, seed pl+en oraz seed 14 rol z kompletnymi
//          tlumaczeniami i pracujace UNIQUE constraints.
// =============================================================================

use std::collections::BTreeSet;
use tempfile::TempDir;

fn open() -> (TempDir, tentaflow_core::db::DbPool) {
    let d = TempDir::new().expect("tempdir");
    let p = d.path().join("role_catalog.db");
    let pool = tentaflow_core::db::init(&p).expect("init");
    (d, pool)
}

#[test]
fn migrations_v40_v41_recorded() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();
    let v40: i64 = conn
        .query_row(
            "SELECT count(*) FROM _migrations WHERE version = 40",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v41: i64 = conn
        .query_row(
            "SELECT count(*) FROM _migrations WHERE version = 41",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v40, 1, "migration v40 must be recorded");
    assert_eq!(v41, 1, "migration v41 must be recorded");
}

#[test]
fn platform_locales_seed_pl_and_en() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM platform_locales WHERE org_id = 'org-default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 2,
        "platform_locales should seed pl + en for org-default"
    );

    let mut stmt = conn
        .prepare("SELECT code, is_default FROM platform_locales WHERE org_id = 'org-default' ORDER BY code")
        .unwrap();
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "en");
    assert_eq!(rows[0].1, 0, "english must not be default");
    assert_eq!(rows[1].0, "pl");
    assert_eq!(rows[1].1, 1, "polish must be default");
}

#[test]
fn role_catalog_seeds_14_roles_with_pl_en_translations() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM role_catalog WHERE org_id = 'org-default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 14, "role_catalog should seed exactly 14 roles");

    // Sprawdz ze kazda rola ma name_translations i description_translations
    // z dokladnie kluczami pl + en (zgodne z seedowanymi platform_locales).
    let mut stmt = conn
        .prepare(
            "SELECT slug, name_translations, description_translations \
             FROM role_catalog WHERE org_id = 'org-default'",
        )
        .unwrap();

    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let expected_keys: BTreeSet<String> =
        ["pl".to_string(), "en".to_string()].into_iter().collect();

    for (slug, name_json, desc_json) in &rows {
        let name: serde_json::Value =
            serde_json::from_str(name_json).expect("name_translations must be JSON");
        let desc: serde_json::Value =
            serde_json::from_str(desc_json).expect("description_translations must be JSON");

        let name_keys: BTreeSet<String> = name
            .as_object()
            .expect("name_translations is object")
            .keys()
            .cloned()
            .collect();
        let desc_keys: BTreeSet<String> = desc
            .as_object()
            .expect("description_translations is object")
            .keys()
            .cloned()
            .collect();

        assert_eq!(
            name_keys, expected_keys,
            "role '{slug}' name_translations must have keys pl + en"
        );
        assert_eq!(
            desc_keys, expected_keys,
            "role '{slug}' description_translations must have keys pl + en"
        );

        for locale in ["pl", "en"] {
            let n = name[locale].as_str().unwrap_or("");
            let d = desc[locale].as_str().unwrap_or("");
            assert!(!n.is_empty(), "role '{slug}' has empty name[{locale}]");
            assert!(
                !d.is_empty(),
                "role '{slug}' has empty description[{locale}]"
            );
        }
    }
}

#[test]
fn role_catalog_contains_expected_slugs() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT slug FROM role_catalog WHERE org_id = 'org-default' ORDER BY slug")
        .unwrap();
    let slugs: BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let expected: BTreeSet<String> = [
        "handlowiec_l1",
        "handlowiec_l2",
        "sales_lead",
        "pm_technical",
        "architect_senior",
        "consultant_technical",
        "developer",
        "qa",
        "section_director",
        "sales_director",
        "ceo",
        "decision_maker",
        "influencer",
        "power_user_sponsor",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        slugs, expected,
        "role_catalog slugs must match the spec 1:1"
    );
}

#[test]
fn role_catalog_unique_slug_per_org_enforced() {
    let (_d, pool) = open();
    let conn = pool.write().unwrap();

    // Probujemy zduplikowac slug — UNIQUE (org_id, slug) musi to zablokowac.
    let res = conn.execute(
        "INSERT INTO role_catalog (id, org_id, slug, kind, name_translations, description_translations) \
         VALUES ('11111111-1111-4111-8111-111111111111', 'org-default', 'developer', 'technical', \
                 '{\"pl\":\"Dup\",\"en\":\"Dup\"}', '{\"pl\":\"x\",\"en\":\"x\"}')",
        [],
    );
    assert!(
        res.is_err(),
        "duplicate slug per org must be rejected by UNIQUE"
    );
}

#[test]
fn platform_locales_one_default_per_org_enforced() {
    let (_d, pool) = open();
    let conn = pool.write().unwrap();

    // Proba dodania drugiego is_default = 1 dla tej samej organizacji.
    let res = conn.execute(
        "INSERT INTO platform_locales (id, org_id, code, display_name, is_default, is_active) \
         VALUES ('22222222-2222-4222-8222-222222222222', 'org-default', 'de', 'Deutsch', 1, 1)",
        [],
    );
    assert!(
        res.is_err(),
        "partial unique index must reject a second default locale per org"
    );
}

#[test]
fn role_catalog_check_kind_enforced() {
    let (_d, pool) = open();
    let conn = pool.write().unwrap();

    let res = conn.execute(
        "INSERT INTO role_catalog (id, org_id, slug, kind, name_translations, description_translations) \
         VALUES ('33333333-3333-4333-8333-333333333333', 'org-default', 'rogue_role', 'invalid_kind', \
                 '{\"pl\":\"x\",\"en\":\"x\"}', '{\"pl\":\"y\",\"en\":\"y\"}')",
        [],
    );
    assert!(res.is_err(), "invalid kind value must be rejected by CHECK");
}

#[test]
fn role_catalog_json_valid_enforced() {
    let (_d, pool) = open();
    let conn = pool.write().unwrap();

    let res = conn.execute(
        "INSERT INTO role_catalog (id, org_id, slug, kind, name_translations, description_translations) \
         VALUES ('44444444-4444-4444-8444-444444444444', 'org-default', 'broken_json', 'other', \
                 'not-json-at-all', '{}')",
        [],
    );
    assert!(res.is_err(), "non-JSON name_translations must be rejected");
}

#[test]
fn role_catalog_is_manager_flags() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();

    // Wg specyfikacji: sales_lead, section_director, sales_director, ceo
    // sa managerami; reszta nie.
    let managers: BTreeSet<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT slug FROM role_catalog WHERE org_id = 'org-default' AND is_manager = 1",
            )
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };

    let expected: BTreeSet<String> = ["sales_lead", "section_director", "sales_director", "ceo"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(managers, expected, "manager flag must match the spec");
}
