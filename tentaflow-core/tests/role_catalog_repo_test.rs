// ============ File: tests/role_catalog_repo_test.rs ============
//
// Pelne pokrycie warstwy `services::role_catalog::repo` plus emiterow audit.
// Testy korzystaja z realnej bazy SQLite (tempfile) i pelnego stosu migracji,
// dzieki czemu seed v40/v41 jest aktywny i mozemy weryfikowac:
//   * read API na seedowanych 14 rolach,
//   * walidacje (slug, translacje, ikona, color_hint),
//   * patch logic + multi-tenant isolation,
//   * audit log + hash chain integrity.

use std::collections::BTreeMap;

use tempfile::TempDir;

use tentaflow_core::db::DbPool;
use tentaflow_core::services::role_catalog::{
    create_role, deactivate_role, get_role, list_active_locale_codes, list_roles, search_roles,
    update_role, Role, RoleCatalogError, RoleCreateInput, RoleKind, RoleListFilter,
    RoleUpdateInput, VisibilityScope,
};

const ORG_DEFAULT: &str = "org-default";
const ACTOR: &str = "u-admin";

fn open() -> (TempDir, DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("role_catalog_repo.db");
    let pool = tentaflow_core::db::init(&path).expect("init db");
    (dir, pool)
}

fn pl_en(name_pl: &str, name_en: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("pl".to_string(), name_pl.to_string());
    m.insert("en".to_string(), name_en.to_string());
    m
}

fn minimal_input(slug: &str) -> RoleCreateInput {
    RoleCreateInput {
        org_id: ORG_DEFAULT.to_string(),
        slug: slug.to_string(),
        kind: RoleKind::Other,
        name_translations: pl_en("Rola", "Role"),
        description_translations: BTreeMap::new(),
        icon: None,
        color_hint: None,
        is_manager: false,
        default_visibility_scope: VisibilityScope::Assigned,
    }
}

// -----------------------------------------------------------------------------
// 1. test_get_seeded_role
// -----------------------------------------------------------------------------
#[test]
fn test_get_seeded_role() {
    let (_d, pool) = open();
    let roles = list_roles(&pool, ORG_DEFAULT, RoleListFilter::default()).unwrap();
    let handlowiec = roles
        .iter()
        .find(|r| r.slug == "handlowiec_l1")
        .expect("seed must include handlowiec_l1");
    let by_id: Role = get_role(&pool, ORG_DEFAULT, &handlowiec.id).unwrap();
    assert_eq!(by_id.slug, "handlowiec_l1");
    assert_eq!(by_id.kind, RoleKind::Sales);
    assert_eq!(by_id.name_translations.len(), 2);
    assert!(by_id.name_translations.contains_key("pl"));
    assert!(by_id.name_translations.contains_key("en"));
}

// -----------------------------------------------------------------------------
// 2. test_list_all_seeded_roles
// -----------------------------------------------------------------------------
#[test]
fn test_list_all_seeded_roles() {
    let (_d, pool) = open();
    let roles = list_roles(&pool, ORG_DEFAULT, RoleListFilter::default()).unwrap();
    assert_eq!(roles.len(), 14, "v41 seeds exactly 14 roles");
}

// -----------------------------------------------------------------------------
// 3. test_list_filter_by_kind_sales
// -----------------------------------------------------------------------------
#[test]
fn test_list_filter_by_kind_sales() {
    let (_d, pool) = open();
    let filter = RoleListFilter {
        kind: Some(RoleKind::Sales),
        ..Default::default()
    };
    let roles = list_roles(&pool, ORG_DEFAULT, filter).unwrap();
    assert_eq!(roles.len(), 3, "seed contains 3 sales roles");
    for r in &roles {
        assert_eq!(r.kind, RoleKind::Sales);
    }
}

// -----------------------------------------------------------------------------
// 4. test_list_filter_by_kind_management
// -----------------------------------------------------------------------------
#[test]
fn test_list_filter_by_kind_management() {
    let (_d, pool) = open();
    let filter = RoleListFilter {
        kind: Some(RoleKind::Management),
        ..Default::default()
    };
    let roles = list_roles(&pool, ORG_DEFAULT, filter).unwrap();
    assert_eq!(roles.len(), 3, "seed contains 3 management roles");
}

// -----------------------------------------------------------------------------
// 5. test_search_by_name
// -----------------------------------------------------------------------------
#[test]
fn test_search_by_name() {
    let (_d, pool) = open();
    let roles = search_roles(&pool, ORG_DEFAULT, "Handlow", 50).unwrap();
    let slugs: Vec<&str> = roles.iter().map(|r| r.slug.as_str()).collect();
    assert!(slugs.contains(&"handlowiec_l1"));
    assert!(slugs.contains(&"handlowiec_l2"));
}

// -----------------------------------------------------------------------------
// 6. test_create_role_minimal_valid
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_minimal_valid() {
    let (_d, pool) = open();
    let role = create_role(&pool, ACTOR, minimal_input("custom_role")).unwrap();
    assert_eq!(role.slug, "custom_role");
    assert!(role.is_active);
    assert_eq!(role.created_by.as_deref(), Some(ACTOR));
    assert_eq!(role.created_at, role.updated_at);
}

// -----------------------------------------------------------------------------
// 7. test_create_role_missing_locale_translation
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_missing_locale_translation() {
    let (_d, pool) = open();
    let mut input = minimal_input("missing_locale");
    input.name_translations = {
        let mut m = BTreeMap::new();
        m.insert("pl".to_string(), "Tylko PL".to_string());
        m
    };
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    match err {
        RoleCatalogError::MissingTranslations { missing, required } => {
            assert!(required.contains(&"en".to_string()));
            assert!(required.contains(&"pl".to_string()));
            assert_eq!(missing, vec!["en".to_string()]);
        }
        other => panic!("expected MissingTranslations, got {other:?}"),
    }
}

// -----------------------------------------------------------------------------
// 8. test_create_role_empty_translation_value
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_empty_translation_value() {
    let (_d, pool) = open();
    let mut input = minimal_input("empty_trans");
    input.name_translations = pl_en("Rola", "   ");
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(
        err,
        RoleCatalogError::EmptyTranslation { ref locale, ref field }
            if locale == "en" && field == "name"
    ));
}

// -----------------------------------------------------------------------------
// 9. test_create_role_invalid_slug_chars
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_invalid_slug_chars() {
    let (_d, pool) = open();
    let mut input = minimal_input("bad-slug-dash");
    input.slug = "Bad Slug!".to_string();
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::InvalidSlug(_)));
}

// -----------------------------------------------------------------------------
// 10. test_create_role_invalid_slug_starts_with_digit
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_invalid_slug_starts_with_digit() {
    let (_d, pool) = open();
    let mut input = minimal_input("placeholder");
    input.slug = "1leading_digit".to_string();
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::InvalidSlug(_)));
}

// -----------------------------------------------------------------------------
// 11. test_create_role_too_long_slug
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_too_long_slug() {
    let (_d, pool) = open();
    let mut input = minimal_input("placeholder");
    input.slug = "a".repeat(51);
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::InvalidSlug(_)));
}

// -----------------------------------------------------------------------------
// 12. test_create_role_duplicate_slug
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_duplicate_slug() {
    let (_d, pool) = open();
    // `handlowiec_l1` jest seedowane przez v41.
    let err = create_role(&pool, ACTOR, minimal_input("handlowiec_l1")).unwrap_err();
    match err {
        RoleCatalogError::SlugConflict { org_id, slug } => {
            assert_eq!(org_id, ORG_DEFAULT);
            assert_eq!(slug, "handlowiec_l1");
        }
        other => panic!("expected SlugConflict, got {other:?}"),
    }
}

// -----------------------------------------------------------------------------
// 13. test_create_role_unknown_icon
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_unknown_icon() {
    let (_d, pool) = open();
    let mut input = minimal_input("bad_icon");
    input.icon = Some("i-nonexistent".to_string());
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::UnknownIcon(_)));
}

// -----------------------------------------------------------------------------
// 14. test_create_role_invalid_color_hex
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_invalid_color_hex() {
    let (_d, pool) = open();
    let mut input = minimal_input("bad_color_hex");
    input.color_hint = Some("#XYZ".to_string());
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::InvalidColorHint(_)));
}

// -----------------------------------------------------------------------------
// 15. test_create_role_invalid_color_var
// -----------------------------------------------------------------------------
#[test]
fn test_create_role_invalid_color_var() {
    let (_d, pool) = open();
    let mut input = minimal_input("bad_color_var");
    input.color_hint = Some("--BAD_UPPER".to_string());
    let err = create_role(&pool, ACTOR, input).unwrap_err();
    assert!(matches!(err, RoleCatalogError::InvalidColorHint(_)));
}

// -----------------------------------------------------------------------------
// 16. test_update_role_partial_field
// -----------------------------------------------------------------------------
#[test]
fn test_update_role_partial_field() {
    let (_d, pool) = open();
    let role = create_role(&pool, ACTOR, minimal_input("partial_update")).unwrap();
    assert!(!role.is_manager);
    let patch = RoleUpdateInput {
        is_manager: Some(true),
        ..Default::default()
    };
    let after = update_role(&pool, ACTOR, ORG_DEFAULT, &role.id, patch).unwrap();
    assert!(after.is_manager);
    // Slug + kind + translacje pozostaly niezmienione.
    assert_eq!(after.slug, role.slug);
    assert_eq!(after.kind, role.kind);
    assert_eq!(after.name_translations, role.name_translations);
}

// -----------------------------------------------------------------------------
// 17. test_update_role_change_translations_revalidates
// -----------------------------------------------------------------------------
#[test]
fn test_update_role_change_translations_revalidates() {
    let (_d, pool) = open();
    let role = create_role(&pool, ACTOR, minimal_input("trans_revalidate")).unwrap();
    let mut only_pl = BTreeMap::new();
    only_pl.insert("pl".to_string(), "Tylko PL".to_string());
    let patch = RoleUpdateInput {
        name_translations: Some(only_pl),
        ..Default::default()
    };
    let err = update_role(&pool, ACTOR, ORG_DEFAULT, &role.id, patch).unwrap_err();
    assert!(matches!(err, RoleCatalogError::MissingTranslations { .. }));
}

// -----------------------------------------------------------------------------
// 18. test_update_role_set_icon_to_null
// -----------------------------------------------------------------------------
#[test]
fn test_update_role_set_icon_to_null() {
    let (_d, pool) = open();
    let mut input = minimal_input("with_icon");
    input.icon = Some("i-shield".to_string());
    let role = create_role(&pool, ACTOR, input).unwrap();
    assert_eq!(role.icon.as_deref(), Some("i-shield"));

    let patch = RoleUpdateInput {
        icon: Some(None),
        ..Default::default()
    };
    let after = update_role(&pool, ACTOR, ORG_DEFAULT, &role.id, patch).unwrap();
    assert!(after.icon.is_none());
}

// -----------------------------------------------------------------------------
// 19. test_update_nonexistent_role
// -----------------------------------------------------------------------------
#[test]
fn test_update_nonexistent_role() {
    let (_d, pool) = open();
    let patch = RoleUpdateInput {
        is_manager: Some(true),
        ..Default::default()
    };
    let err = update_role(&pool, ACTOR, ORG_DEFAULT, "ghost-id", patch).unwrap_err();
    assert!(matches!(err, RoleCatalogError::NotFound(_)));
}

// -----------------------------------------------------------------------------
// 20. test_deactivate_role
// -----------------------------------------------------------------------------
#[test]
fn test_deactivate_role() {
    let (_d, pool) = open();
    let role = create_role(&pool, ACTOR, minimal_input("to_deactivate")).unwrap();
    deactivate_role(&pool, ACTOR, ORG_DEFAULT, &role.id).unwrap();
    let after = get_role(&pool, ORG_DEFAULT, &role.id).unwrap();
    assert!(!after.is_active);
    // Drugi deactivate na juz nieaktywnym rekordzie -> NotFound (filter is_active=1).
    let err = deactivate_role(&pool, ACTOR, ORG_DEFAULT, &role.id).unwrap_err();
    assert!(matches!(err, RoleCatalogError::NotFound(_)));
}

// -----------------------------------------------------------------------------
// 21. test_multi_tenant_isolation
// -----------------------------------------------------------------------------
#[test]
fn test_multi_tenant_isolation() {
    let (_d, pool) = open();
    // Tworzymy druga organizacje + jej locale rownolegle, omijajac warstwe
    // services::org (ktora wymagalaby szerszego scope'u). Multi-tenant
    // isolation tutaj weryfikujemy po stronie role_catalog.
    {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, status, created_at) \
             VALUES ('org-foo', 'Foo', 'foo', 'active', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO platform_locales (id, org_id, code, display_name, is_default, is_active) \
             VALUES ('loc-foo-pl', 'org-foo', 'pl', 'Polski', 1, 1)",
            [],
        )
        .unwrap();
    }

    let locales = list_active_locale_codes(&pool, "org-foo").unwrap();
    assert_eq!(locales, vec!["pl".to_string()]);

    // W org-foo locale to tylko `pl` — wystarczy jeden klucz w translacjach.
    let mut foo_input = RoleCreateInput {
        org_id: "org-foo".to_string(),
        slug: "foo_only_role".to_string(),
        kind: RoleKind::Other,
        name_translations: {
            let mut m = BTreeMap::new();
            m.insert("pl".to_string(), "Rola org-foo".to_string());
            m
        },
        description_translations: BTreeMap::new(),
        icon: None,
        color_hint: None,
        is_manager: false,
        default_visibility_scope: VisibilityScope::Assigned,
    };
    let foo_role = create_role(&pool, ACTOR, foo_input.clone()).unwrap();
    assert_eq!(foo_role.org_id, "org-foo");

    // org-default NIE powinno widziec roli z org-foo.
    let default_roles = list_roles(&pool, ORG_DEFAULT, RoleListFilter::default()).unwrap();
    assert!(default_roles.iter().all(|r| r.slug != "foo_only_role"));
    // get_role po org-default + foo_role.id zwraca NotFound.
    let err = get_role(&pool, ORG_DEFAULT, &foo_role.id).unwrap_err();
    assert!(matches!(err, RoleCatalogError::NotFound(_)));

    // Slug "foo_only_role" jest unikalny w obrebie org-foo, ale wolny dla
    // org-default — sprawdzmy ze unikalnosc jest scoped do org.
    foo_input.org_id = ORG_DEFAULT.to_string();
    foo_input.name_translations = pl_en("Rola default", "Default Role");
    let default_role = create_role(&pool, ACTOR, foo_input).unwrap();
    assert_eq!(default_role.org_id, ORG_DEFAULT);
    assert_eq!(default_role.slug, "foo_only_role");
}

// -----------------------------------------------------------------------------
// 22. test_audit_log_create
// -----------------------------------------------------------------------------
#[test]
fn test_audit_log_create() {
    let (_d, pool) = open();
    let before_count: i64 = {
        let conn = pool.read().unwrap();
        conn.query_row(
            "SELECT count(*) FROM audit_log WHERE action = 'role_catalog.created'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let role = create_role(&pool, ACTOR, minimal_input("audit_create")).unwrap();
    let conn = pool.read().unwrap();
    let after_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit_log WHERE action = 'role_catalog.created'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after_count, before_count + 1);

    let (resource_id, details, org_id, risk_class, result): (
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT resource_id, details, org_id, risk_class, result \
             FROM audit_log WHERE action = 'role_catalog.created' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(resource_id, role.id);
    assert_eq!(org_id, ORG_DEFAULT);
    assert_eq!(risk_class, "B");
    assert_eq!(result, "success");
    let parsed: serde_json::Value = serde_json::from_str(&details).unwrap();
    assert_eq!(parsed["user_id"], serde_json::Value::String(ACTOR.into()));
    assert_eq!(
        parsed["slug"],
        serde_json::Value::String("audit_create".into())
    );
}

// -----------------------------------------------------------------------------
// 23. test_audit_log_chain_integrity
// -----------------------------------------------------------------------------
#[test]
fn test_audit_log_chain_integrity() {
    let (_d, pool) = open();
    let _r1 = create_role(&pool, ACTOR, minimal_input("chain_1")).unwrap();
    let _r2 = create_role(&pool, ACTOR, minimal_input("chain_2")).unwrap();
    let _r3 = create_role(&pool, ACTOR, minimal_input("chain_3")).unwrap();

    let conn = pool.read().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, prev_hash, hash FROM audit_log \
             WHERE action = 'role_catalog.created' ORDER BY id ASC",
        )
        .unwrap();
    let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        rows.len() >= 3,
        "expected at least 3 audit rows for 3 creates"
    );
    // Lancuch: prev_hash row[i] == hash row[i-1] dla i >= 1. Sprawdz ostatnie 3.
    let tail = &rows[rows.len() - 3..];
    for i in 1..tail.len() {
        let prev_hash_of_current = &tail[i].1;
        let hash_of_previous = &tail[i - 1].2;
        assert_eq!(
            prev_hash_of_current,
            hash_of_previous,
            "audit hash chain broken between row {} and {}",
            tail[i - 1].0,
            tail[i].0
        );
        assert_eq!(prev_hash_of_current.len(), 32);
        assert_eq!(hash_of_previous.len(), 32);
    }
}
