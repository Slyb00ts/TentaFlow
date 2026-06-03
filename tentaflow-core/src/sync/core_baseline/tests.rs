// =============================================================================
// Plik: sync/core_baseline/tests.rs
// Opis: Testy in-process rdzenia baseline-adopt: deterministyczna elekcja,
//       round-trip snapshotu (serialize/chunk/reassemble), oraz atomowy import
//       joinera na dwoch osobnych DbPool (dawca + joiner).
// =============================================================================

use super::*;
use crate::db::{self, DbPool};
use rusqlite::params;

/// Tworzy goly pool i czysci seedowane domyslne dane platformowe, by fixture
/// reprezentowal pojedynczy, kontrolowany single-org nod (realny stan przed
/// pierwszym parowaniem to dokladnie jedna organizacja).
fn new_pool() -> DbPool {
    let pool = db::init(std::path::Path::new(":memory:")).expect("init test DB");
    {
        let conn = pool.lock().unwrap();
        for table in [
            "org_memberships",
            "sync_user_org_profiles",
            "sync_resource_acl",
            "sync_policies",
            "group_members",
            "flow_model_bindings",
            "flows",
            "user_groups",
            "user_accounts",
            "roles",
            "organizations",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), []).unwrap();
        }
    }
    pool
}

fn epoch(counter: u64, origin: &str) -> BaselineEpoch {
    BaselineEpoch {
        counter,
        origin_node: origin.to_string(),
    }
}

/// Wstawia organizacje + role + usera + flow tak, by snapshot byl reprezenta-
/// tywny. Czysci najpierw seedowane wiersze nieuzywane w tescie, by asercje byly
/// jednoznaczne.
fn seed_org(pool: &DbPool, org_id: &str, slug: &str) {
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO organizations \
            (org_id, name, slug, status, created_at) \
         VALUES (?1, ?2, ?3, 'active', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![org_id, format!("Org {org_id}"), slug],
    )
    .unwrap();
}

fn seed_role(pool: &DbPool, role_id: &str, name: &str) {
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES (?1, ?2, '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![role_id, name],
    )
    .unwrap();
}

fn seed_user(pool: &DbPool, id: &str, username: &str, email: Option<&str>) {
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_accounts \
            (id, username, password_hash, display_name, email, is_active, is_admin, role) \
         VALUES (?1, ?2, 'hash', ?2, ?3, 1, 0, 'user')",
        params![id, username, email],
    )
    .unwrap();
}

fn seed_membership(pool: &DbPool, org_id: &str, user_id: &str, role_id: &str) {
    let conn = pool.lock().unwrap();
    // org_memberships.role_id ma FK na roles(role_id) — zapewniamy istnienie roli.
    conn.execute(
        "INSERT OR IGNORE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES (?1, ?1, '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![role_id],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO org_memberships \
            (org_id, user_id, role_id, granted_at, granted_by) \
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'seed')",
        params![org_id, user_id, role_id],
    )
    .unwrap();
}

fn seed_flow(pool: &DbPool, id: &str, name: &str, model: Option<&str>) {
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO flows \
            (id, name, flow_json, status, is_default, published_model_name) \
         VALUES (?1, ?2, '{}', 'active', 0, ?3)",
        params![id, name, model],
    )
    .unwrap();
}

fn user_exists(pool: &DbPool, id: &str) -> bool {
    let conn = pool.lock().unwrap();
    conn.query_row(
        "SELECT 1 FROM user_accounts WHERE id = ?1",
        params![id],
        |_| Ok(()),
    )
    .optional()
    .unwrap()
    .is_some()
}

fn username_of(pool: &DbPool, id: &str) -> Option<String> {
    let conn = pool.lock().unwrap();
    conn.query_row(
        "SELECT username FROM user_accounts WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn membership_org(pool: &DbPool, user_id: &str) -> Option<String> {
    let conn = pool.lock().unwrap();
    conn.query_row(
        "SELECT org_id FROM org_memberships WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn count_flows_named(pool: &DbPool, name: &str) -> i64 {
    let conn = pool.lock().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM flows WHERE name = ?1",
        params![name],
        |r| r.get(0),
    )
    .unwrap()
}

// =============================================================================
// 1. Elekcja dawcy
// =============================================================================

#[test]
fn decide_roles_lower_node_id_wins_donor() {
    let (donor, joiner) = decide_roles("aaaa", "bbbb", None);
    assert_eq!(donor, "aaaa");
    assert_eq!(joiner, "bbbb");
    // Symetria: kolejnosc argumentow nie zmienia wyniku.
    let (donor2, joiner2) = decide_roles("bbbb", "aaaa", None);
    assert_eq!(donor2, "aaaa");
    assert_eq!(joiner2, "bbbb");
}

#[test]
fn decide_roles_explicit_donor_respected() {
    let (donor, joiner) = decide_roles("aaaa", "bbbb", Some("bbbb"));
    assert_eq!(donor, "bbbb");
    assert_eq!(joiner, "aaaa");
}

#[test]
fn dual_initiate_converges_to_single_donor() {
    // A i B oba "inicjuja": kazdy liczy decide_roles z wlasnej perspektywy.
    let a = "node-a-1111";
    let b = "node-b-2222";
    let (donor_from_a, joiner_from_a) = decide_roles(a, b, None);
    let (donor_from_b, joiner_from_b) = decide_roles(b, a, None);

    // Obie strony zgadzaja sie kto jest dawca i kto joinerem — brak
    // A-joins-B && B-joins-A.
    assert_eq!(donor_from_a, donor_from_b);
    assert_eq!(joiner_from_a, joiner_from_b);
    assert_eq!(donor_from_a, a);
    assert_eq!(joiner_from_a, b);
}

#[test]
fn validate_ack_agreement_rejects_mismatch() {
    let ok = BaselineAck {
        accepted: true,
        donor: "d".into(),
        joiner: "j".into(),
        epoch: 7,
    };
    assert!(validate_ack_agreement(&ok, "d", "j", 7).is_ok());

    let wrong_epoch = BaselineAck {
        epoch: 8,
        ..ok.clone()
    };
    assert!(validate_ack_agreement(&wrong_epoch, "d", "j", 7).is_err());

    let rejected = BaselineAck {
        accepted: false,
        ..ok.clone()
    };
    assert!(validate_ack_agreement(&rejected, "d", "j", 7).is_err());

    let role_swap = BaselineAck {
        donor: "j".into(),
        joiner: "d".into(),
        ..ok
    };
    assert!(validate_ack_agreement(&role_swap, "d", "j", 7).is_err());
}

#[test]
fn guard_role_blocks_opposite_in_progress() {
    let pool = new_pool();
    store_adopt_state(
        &pool,
        &BaselineAdoptState {
            role: BaselineRole::Donor,
            peer: "peer".into(),
            epoch: epoch(1, "x"),
            phase: BaselinePhase::Receiving,
        },
    )
    .unwrap();
    // Nod jest dawca w trakcie — nie moze wejsc jako joiner.
    assert!(guard_role(&pool, BaselineRole::Joiner).is_err());
    assert!(guard_role(&pool, BaselineRole::Donor).is_ok());

    // Po zakonczeniu poprzedniej adopcji przeciwna rola jest dozwolona.
    store_adopt_state(
        &pool,
        &BaselineAdoptState {
            role: BaselineRole::Donor,
            peer: "peer".into(),
            epoch: epoch(1, "x"),
            phase: BaselinePhase::Completed,
        },
    )
    .unwrap();
    assert!(guard_role(&pool, BaselineRole::Joiner).is_ok());
}

// =============================================================================
// 2. Snapshot round-trip
// =============================================================================

#[test]
fn snapshot_serialize_chunk_reassemble_roundtrip() {
    let pool = new_pool();
    seed_org(&pool, "org-donor", "donor");
    seed_role(&pool, "role-user", "user");
    seed_user(&pool, "u-donor-1", "alice", Some("alice@example.com"));
    seed_membership(&pool, "org-donor", "u-donor-1", "role-user");
    seed_flow(&pool, "f-1", "donor-flow", Some("donor/model"));

    let snap = capture_baseline_snapshot(&pool, epoch(3, "org-donor")).unwrap();
    let bytes = serialize_snapshot(&snap).unwrap();
    let chunks = chunk_snapshot(&bytes);
    let rebuilt_bytes = reassemble_chunks(&chunks).unwrap();
    assert_eq!(rebuilt_bytes, bytes);

    let rebuilt = deserialize_snapshot(&rebuilt_bytes).unwrap();
    assert_eq!(rebuilt, snap);
    assert!(rebuilt.user_accounts.iter().any(|u| u.id == "u-donor-1"));
    assert!(rebuilt.flows.iter().any(|f| f.id == "f-1"));
}

#[test]
fn corrupted_chunk_is_detected() {
    let pool = new_pool();
    seed_org(&pool, "org-donor", "donor");
    let snap = capture_baseline_snapshot(&pool, epoch(1, "org-donor")).unwrap();
    let bytes = serialize_snapshot(&snap).unwrap();
    let mut chunks = chunk_snapshot(&bytes);
    assert!(!chunks.is_empty());
    // Uszkadzamy bajt w pierwszym chunku, hash zostaje stary.
    chunks[0].bytes[0] ^= 0xFF;
    let err = reassemble_chunks(&chunks).expect_err("corruption must be detected");
    assert!(format!("{err}").contains("content hash mismatch"));
}

#[test]
fn sequence_gap_is_detected() {
    let pool = new_pool();
    seed_org(&pool, "org-donor", "donor");
    let snap = capture_baseline_snapshot(&pool, epoch(1, "org-donor")).unwrap();
    let bytes = vec![0u8; BASELINE_CHUNK_BYTES * 3];
    let _ = (&snap, &bytes);
    let mut chunks = chunk_snapshot(&bytes);
    assert!(chunks.len() >= 3);
    chunks.remove(1); // luka w seq
    let err = reassemble_chunks(&chunks).expect_err("gap must be detected");
    assert!(format!("{err}").contains("sequence gap"));
}

// =============================================================================
// 3. Atomowy import (dwa osobne pule)
// =============================================================================

/// Buduje snapshot dawcy z osobnej puli i importuje go do puli joinera.
fn donor_snapshot(donor: &DbPool, epoch_counter: u64) -> BaselineSnapshot {
    capture_baseline_snapshot(donor, epoch(epoch_counter, "donor-node")).unwrap()
}

#[test]
fn import_merges_roles_without_duplicates() {
    let donor = new_pool();
    let joiner = new_pool();
    // Deterministyczny seed: ta sama rola po tym samym role_id u obu.
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-shared", "shared");
    seed_org(&joiner, "org-joiner", "joiner");
    seed_role(&joiner, "role-shared", "shared");

    let snap = donor_snapshot(&donor, 5);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    let conn = joiner.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM roles WHERE role_id = 'role-shared'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "deterministyczna rola scalona po id");
}

#[test]
fn import_brings_donor_user_created_row() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor-only", "donoruser", Some("donor@x.io"));
    seed_membership(&donor, "org-donor", "u-donor-only", "role-user");
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 2);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    assert!(user_exists(&joiner, "u-donor-only"));
    assert_eq!(membership_org(&joiner, "u-donor-only").as_deref(), Some("org-donor"));
}

#[test]
fn import_username_collision_donor_wins_joiner_suffixed() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "shared_name", Some("donor@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    // Inny UUID, ta sama nazwa, INNY email -> nie ten sam czlowiek (joiner
    // dolaczy jako nowy czlonek org dawcy, dlatego dawca musi miec role).
    seed_user(&joiner, "u-joiner", "shared_name", Some("joiner@x.io"));

    let snap = donor_snapshot(&donor, 1);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    // Dawca zachowuje czysta nazwe.
    assert_eq!(username_of(&joiner, "u-donor").as_deref(), Some("shared_name"));
    // Joiner dostal suffix.
    let joiner_name = username_of(&joiner, "u-joiner").expect("joiner user present");
    assert!(
        joiner_name.starts_with("shared_name-"),
        "joiner username powinien byc suffixowany, jest: {joiner_name}"
    );
}

#[test]
fn import_joiner_user_other_email_becomes_member_of_donor_org() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("alice@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("bob@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");

    let snap = donor_snapshot(&donor, 1);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    // Bob zostaje wlasnym userem, ale w org dawcy.
    assert!(user_exists(&joiner, "u-joiner"));
    assert_eq!(membership_org(&joiner, "u-joiner").as_deref(), Some("org-donor"));
}

#[test]
fn import_same_email_maps_to_donor_user() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("same@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "alice_local", Some("same@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");

    let snap = donor_snapshot(&donor, 1);
    let report = import_baseline(&joiner, &snap, "donor-node").unwrap();

    // Lokalny user joinera znika (zmapowany na usera dawcy po emailu).
    assert!(!user_exists(&joiner, "u-joiner"));
    assert!(user_exists(&joiner, "u-donor"));
    assert_eq!(report.users_merged_by_email, 1);
}

#[test]
fn import_joiner_belongs_to_donor_org_after() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_membership(&donor, "org-donor", "u-donor", "role-user");

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("b@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");

    let snap = donor_snapshot(&donor, 1);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    // Zadnego czlonkostwa w org joinera nie powinno juz byc.
    let conn = joiner.lock().unwrap();
    let foreign: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM org_memberships WHERE org_id <> 'org-donor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(foreign, 0, "joiner nie ma juz wlasnej org");
}

#[test]
fn import_both_have_version_2_flow_no_unique_error_both_kept() {
    // Scenariusz z briefu: dwa nody, kazdy z flow "version 2" o tym samym
    // published_model_name ale roznych UUID. Po imporcie brak UNIQUE error,
    // oba flow zachowane.
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_flow(&donor, "f-donor", "version 2", Some("shared/model"));

    seed_org(&joiner, "org-joiner", "joiner");
    seed_flow(&joiner, "f-joiner", "version 2", Some("shared/model"));

    let snap = donor_snapshot(&donor, 1);
    import_baseline(&joiner, &snap, "donor-node").unwrap();

    // Oba flow zachowane (rozne UUID).
    assert_eq!(count_flows_named(&joiner, "version 2"), 2);

    // Dawca zachowuje published_model_name, joiner ma unpublish (NULL).
    let conn = joiner.lock().unwrap();
    let donor_model: Option<String> = conn
        .query_row(
            "SELECT published_model_name FROM flows WHERE id = 'f-donor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let joiner_model: Option<String> = conn
        .query_row(
            "SELECT published_model_name FROM flows WHERE id = 'f-joiner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(donor_model.as_deref(), Some("shared/model"));
    assert_eq!(joiner_model, None, "joiner flow unpublished przy kolizji modelu");
}

#[test]
fn import_is_idempotent_on_repair() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 4);
    import_baseline(&joiner, &snap, "donor-node").unwrap();
    // Drugi import tego samego dawcy+epoch jest no-opem (faza Completed).
    let report2 = import_baseline(&joiner, &snap, "donor-node").unwrap();
    assert_eq!(report2.users_merged_by_email, 0);
    assert_eq!(report2.users_joined_donor_org, 0);
    assert!(user_exists(&joiner, "u-donor"));
}

#[test]
fn import_rollback_leaves_joiner_untouched_on_injected_error() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("b@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");

    // Wstrzykniety blad: org_membership dawcy wskazuje na nieistniejaca role
    // (FK references roles(role_id)) — INSERT w transakcji importu padnie i cala
    // transakcja sie cofnie.
    let mut bad = donor_snapshot(&donor, 1);
    bad.org_memberships.push(OrgMembershipRow {
        org_id: "org-donor".into(),
        user_id: "u-donor".into(),
        role_id: "role-DOES-NOT-EXIST".into(),
        granted_by: "seed".into(),
    });

    let result = import_baseline(&joiner, &bad, "donor-node");
    assert!(result.is_err(), "import z bledem FK musi sie nie powiesc");

    // Joiner nietkniety: jego user i org dalej istnieja, dawcy nie wniknal.
    assert!(user_exists(&joiner, "u-joiner"));
    assert!(!user_exists(&joiner, "u-donor"));
    assert_eq!(membership_org(&joiner, "u-joiner").as_deref(), Some("org-joiner"));
}

#[test]
fn run_baseline_adopt_drives_full_path_from_bytes() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_membership(&donor, "org-donor", "u-donor", "role-user");
    seed_org(&joiner, "org-joiner", "joiner");

    // Pelna sciezka transportu-agnostyczna: snapshot -> chunks -> reassemble ->
    // run_baseline_adopt (deserializacja + import).
    let snap = donor_snapshot(&donor, 9);
    let bytes = serialize_snapshot(&snap).unwrap();
    let chunks = chunk_snapshot(&bytes);
    let rebuilt = reassemble_chunks(&chunks).unwrap();

    run_baseline_adopt(&joiner, "donor-node", &rebuilt).unwrap();
    assert!(user_exists(&joiner, "u-donor"));
    assert_eq!(membership_org(&joiner, "u-donor").as_deref(), Some("org-donor"));
}
