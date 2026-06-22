// =============================================================================
// Plik: sync/core_baseline/tests.rs
// Opis: Testy in-process rdzenia baseline-adopt: deterministyczna elekcja,
//       round-trip snapshotu (serialize/chunk/reassemble), oraz atomowy import
//       joinera na dwoch osobnych DbPool (dawca + joiner).
// =============================================================================

use super::*;
use crate::crypto::SettingsCipher;
use crate::db::{self, DbPool};
use rusqlite::params;

/// Deterministyczny cipher testowy. Dawca i joiner moga miec ROZNE klucze —
/// sekrety jada plaintextem w snapshocie i joiner re-encryptuje swoim.
fn test_cipher() -> SettingsCipher {
    SettingsCipher::new(&[7u8; 32])
}

/// Cipher z innym kluczem niz `test_cipher` — symuluje fakt, ze kazdy nod ma
/// wlasny master key.
fn other_cipher() -> SettingsCipher {
    SettingsCipher::new(&[42u8; 32])
}

/// Lokalny node_id joinera w testach — uzywany do zachowania wlasnego wpisu
/// `sync_nodes` przy imporcie.
const JOINER_LOCAL_NODE: &str = "joiner-local-node";

/// Tworzy goly pool i czysci seedowane domyslne dane platformowe, by fixture
/// reprezentowal pojedynczy, kontrolowany single-org nod (realny stan przed
/// pierwszym parowaniem to dokladnie jedna organizacja).
fn new_pool() -> DbPool {
    let pool = db::init(std::path::Path::new(":memory:")).expect("init test DB");
    {
        let conn = pool.write().unwrap();
        for table in [
            "node_user_assignments",
            "user_identity_keys",
            "sync_explicit_shares",
            "org_memberships",
            "sync_user_org_profiles",
            "sync_resource_acl",
            "sync_policies",
            "group_members",
            "flow_model_bindings",
            "flows",
            "user_groups",
            "sync_nodes",
            "user_accounts",
            "roles",
            "organizations",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), []).unwrap();
        }
    }
    pool
}

/// Wrapper na import z domyslnymi parametrami testowymi (lokalny node + cipher).
fn import(
    joiner: &DbPool,
    snap: &BaselineSnapshot,
    donor: &str,
) -> LedgerResult<BaselineImportReport> {
    import_baseline(joiner, snap, donor, JOINER_LOCAL_NODE, &test_cipher())
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
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO organizations \
            (org_id, name, slug, status, created_at) \
         VALUES (?1, ?2, ?3, 'active', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![org_id, format!("Org {org_id}"), slug],
    )
    .unwrap();
}

fn seed_role(pool: &DbPool, role_id: &str, name: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES (?1, ?2, '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![role_id, name],
    )
    .unwrap();
}

fn seed_user(pool: &DbPool, id: &str, username: &str, email: Option<&str>) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_accounts \
            (id, username, password_hash, display_name, email, is_active, is_admin, role) \
         VALUES (?1, ?2, 'hash', ?2, ?3, 1, 0, 'user')",
        params![id, username, email],
    )
    .unwrap();
}

fn seed_membership(pool: &DbPool, org_id: &str, user_id: &str, role_id: &str) {
    let conn = pool.write().unwrap();
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
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO flows \
            (id, name, flow_json, status, is_default, published_model_name) \
         VALUES (?1, ?2, '{}', 'active', 0, ?3)",
        params![id, name, model],
    )
    .unwrap();
}

fn seed_admin_user(pool: &DbPool, id: &str, username: &str, email: Option<&str>) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_accounts \
            (id, username, password_hash, display_name, email, is_active, is_admin, role) \
         VALUES (?1, ?2, 'hash', ?2, ?3, 1, 1, 'admin')",
        params![id, username, email],
    )
    .unwrap();
}

/// Seeduje role z jawnym permissions_json (np. `["org.admin"]` dla
/// uprzywilejowanej, `[]` dla zwyklej).
fn seed_role_perms(pool: &DbPool, role_id: &str, name: &str, perms_json: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        params![role_id, name, perms_json],
    )
    .unwrap();
}

fn seed_sync_node(pool: &DbPool, node_id: &str, display: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO sync_nodes (node_id, public_key, display_name, trust_status) \
         VALUES (?1, ?1, ?2, 'trusted')",
        params![node_id, display],
    )
    .unwrap();
}

fn seed_explicit_share(pool: &DbPool, org_id: &str, subject_id: &str, granted_by: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO sync_explicit_shares \
            (org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action, granted_by) \
         VALUES (?1, 'addon-x', 'rt', 'rid', 'user', ?2, 'read', ?3)",
        params![org_id, subject_id, granted_by],
    )
    .unwrap();
}

fn seed_identity_key(pool: &DbPool, key_id: &str, user_id: &str, public_key: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_identity_keys \
            (key_id, user_id, key_type, public_key, purpose, status) \
         VALUES (?1, ?2, 'ed25519', ?3, 'sync', 'active')",
        params![key_id, user_id, public_key],
    )
    .unwrap();
}

fn seed_node_owner(pool: &DbPool, node_id: &str, owner_user_id: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO sync_nodes (node_id, public_key, display_name, trust_status, owner_user_id) \
         VALUES (?1, ?1, ?1, 'trusted', ?2)",
        params![node_id, owner_user_id],
    )
    .unwrap();
}

fn identity_key_count(pool: &DbPool, user_id: &str) -> i64 {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM user_identity_keys WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn node_owner_of(pool: &DbPool, node_id: &str) -> Option<String> {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT owner_user_id FROM sync_nodes WHERE node_id = ?1",
        params![node_id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
    .flatten()
}

fn share_granted_by(pool: &DbPool, subject_id: &str) -> Option<String> {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT granted_by FROM sync_explicit_shares WHERE subject_id = ?1 LIMIT 1",
        params![subject_id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
    .flatten()
}

fn foreign_key_violations(pool: &DbPool) -> i64 {
    let conn = pool.read().unwrap();
    let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
    let rows = stmt.query_map([], |_| Ok(())).unwrap();
    rows.count() as i64
}

fn set_secret(pool: &DbPool, cipher: &SettingsCipher, key: &str, value: &str) {
    let conn = pool.write().unwrap();
    let enc = cipher.encrypt(value).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, enc],
    )
    .unwrap();
}

fn get_secret(pool: &DbPool, cipher: &SettingsCipher, key: &str) -> Option<String> {
    let conn = pool.read().unwrap();
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    raw.filter(|v| !v.is_empty())
        .map(|v| cipher.decrypt(&v).unwrap())
}

fn node_exists(pool: &DbPool, node_id: &str) -> bool {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT 1 FROM sync_nodes WHERE node_id = ?1",
        params![node_id],
        |_| Ok(()),
    )
    .optional()
    .unwrap()
    .is_some()
}

fn explicit_share_count(pool: &DbPool, org_id: &str, subject_id: &str) -> i64 {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM sync_explicit_shares WHERE org_id = ?1 AND subject_id = ?2",
        params![org_id, subject_id],
        |r| r.get(0),
    )
    .unwrap()
}

fn user_exists(pool: &DbPool, id: &str) -> bool {
    let conn = pool.read().unwrap();
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
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT username FROM user_accounts WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn membership_org(pool: &DbPool, user_id: &str) -> Option<String> {
    let conn = pool.read().unwrap();
    conn.query_row(
        "SELECT org_id FROM org_memberships WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn count_flows_named(pool: &DbPool, name: &str) -> i64 {
    let conn = pool.read().unwrap();
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
fn content_election_data_holder_is_donor_regardless_of_node_id() {
    // Empty node has the LOWER id but the data-holder must still be donor, so the
    // empty node adopts the content instead of wiping the peer.
    let empty = "aaaa";
    let holder = "bbbb";
    let (donor, joiner) = decide_roles_by_content(empty, 0, holder, 500);
    assert_eq!(donor, holder);
    assert_eq!(joiner, empty);
    // Same inputs from the holder's perspective converge on the same donor.
    let (donor2, joiner2) = decide_roles_by_content(holder, 500, empty, 0);
    assert_eq!(donor2, holder);
    assert_eq!(joiner2, empty);
}

#[test]
fn content_election_ties_break_on_lower_node_id() {
    let (donor, joiner) = decide_roles_by_content("aaaa", 10, "bbbb", 10);
    assert_eq!(donor, "aaaa");
    assert_eq!(joiner, "bbbb");
    let (donor2, joiner2) = decide_roles_by_content("bbbb", 10, "aaaa", 10);
    assert_eq!(donor2, "aaaa");
    assert_eq!(joiner2, "bbbb");
}

#[test]
fn joiner_supersedes_armed_donor_slot_for_same_peer_epoch() {
    let epoch = epoch(3, "peer");
    let armed_donor = BaselineAdoptState {
        role: BaselineRole::Donor,
        peer: "peer".into(),
        epoch: epoch.clone(),
        phase: BaselinePhase::Elected,
    };
    // The local node legitimately decides to PULL from the same peer: the armed
    // donor slot must yield, not wedge the single-flight gate.
    assert!(!conflicts_with(
        &armed_donor,
        BaselineRole::Joiner,
        "peer",
        &epoch
    ));
    // But an ACTIVE donor transfer (past Elected) for the same peer still conflicts.
    let active_donor = BaselineAdoptState {
        phase: BaselinePhase::Receiving,
        ..armed_donor.clone()
    };
    assert!(conflicts_with(
        &active_donor,
        BaselineRole::Joiner,
        "peer",
        &epoch
    ));
    // And an armed donor toward a DIFFERENT peer still conflicts.
    assert!(conflicts_with(
        &armed_donor,
        BaselineRole::Joiner,
        "other-peer",
        &epoch
    ));
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
fn begin_adopt_atomic_blocks_opposite_in_progress() {
    let pool = new_pool();
    // Nod startuje jako dawca w trakcie (faza != Completed).
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Donor,
            "peer",
            &epoch(1, "x"),
            BaselinePhase::Receiving,
        )
        .unwrap(),
        BeginOutcome::Started
    ));

    // Przeciwna rola (joiner) w trakcie — twardy blad single-flight.
    assert!(begin_adopt_atomic(
        &pool,
        BaselineRole::Joiner,
        "peer",
        &epoch(1, "x"),
        BaselinePhase::Importing,
    )
    .is_err());

    // Ta sama rola (dawca) — dozwolona (aktualizuje stan).
    assert!(begin_adopt_atomic(
        &pool,
        BaselineRole::Donor,
        "peer",
        &epoch(1, "x"),
        BaselinePhase::Receiving,
    )
    .is_ok());

    // Po `Completed` przeciwna rola jest dozwolona (nowa adopcja).
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
    assert!(begin_adopt_atomic(
        &pool,
        BaselineRole::Joiner,
        "peer2",
        &epoch(2, "y"),
        BaselinePhase::Importing,
    )
    .is_ok());
}

#[test]
fn begin_adopt_atomic_single_flight_only_one_winner() {
    // Symulacja dwoch rownoleglych startow: pierwszy wygrywa role, drugi (rola
    // przeciwna) dostaje odmowe. Atomowy check+write w jednej transakcji na
    // wspoldzielonym polaczeniu serializuje oba wywolania.
    let pool = new_pool();
    let r1 = begin_adopt_atomic(
        &pool,
        BaselineRole::Joiner,
        "peer-a",
        &epoch(1, "a"),
        BaselinePhase::Importing,
    );
    let r2 = begin_adopt_atomic(
        &pool,
        BaselineRole::Donor,
        "peer-b",
        &epoch(1, "b"),
        BaselinePhase::Receiving,
    );
    assert!(r1.is_ok(), "pierwszy start wygrywa role");
    assert!(r2.is_err(), "drugi start (przeciwna rola) dostaje odmowe");
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

    let snap = capture_baseline_snapshot(&pool, epoch(3, "org-donor"), &test_cipher()).unwrap();
    let bytes = serialize_snapshot(&snap).unwrap();
    let header = build_baseline_header(&snap, &bytes);
    let chunks = chunk_snapshot(&bytes);
    let rebuilt_bytes = reassemble_chunks(&chunks, &header).unwrap();
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
    let snap = capture_baseline_snapshot(&pool, epoch(1, "org-donor"), &test_cipher()).unwrap();
    let bytes = serialize_snapshot(&snap).unwrap();
    let header = build_baseline_header(&snap, &bytes);
    let mut chunks = chunk_snapshot(&bytes);
    assert!(!chunks.is_empty());
    // Uszkadzamy bajt w pierwszym chunku, hash zostaje stary.
    chunks[0].bytes[0] ^= 0xFF;
    let err = reassemble_chunks(&chunks, &header).expect_err("corruption must be detected");
    assert!(format!("{err}").contains("content hash mismatch"));
}

#[test]
fn sequence_gap_is_detected() {
    let bytes = vec![0u8; BASELINE_CHUNK_BYTES * 3];
    let header = BaselineHeader {
        schema_version: 1,
        epoch: 1,
        tables: vec![],
        row_counts: vec![],
        total_bytes: bytes.len() as u64,
        max_bytes: BASELINE_MAX_TOTAL_BYTES,
        content_hash: *blake3::hash(&bytes).as_bytes(),
    };
    let mut chunks = chunk_snapshot(&bytes);
    assert!(chunks.len() >= 3);
    chunks.remove(1); // luka w seq
    let err = reassemble_chunks(&chunks, &header).expect_err("gap must be detected");
    assert!(format!("{err}").contains("sequence gap"));
}

#[test]
fn oversize_snapshot_is_rejected() {
    let bytes = vec![0u8; BASELINE_CHUNK_BYTES];
    let header = BaselineHeader {
        schema_version: 1,
        epoch: 1,
        tables: vec![],
        row_counts: vec![],
        total_bytes: bytes.len() as u64,
        // Limit ponizej rzeczywistego rozmiaru -> odmowa.
        max_bytes: 16,
        content_hash: *blake3::hash(&bytes).as_bytes(),
    };
    let chunks = chunk_snapshot(&bytes);
    let err = reassemble_chunks(&chunks, &header).expect_err("oversize must be rejected");
    assert!(format!("{err}").contains("too large"));
}

#[test]
fn whole_snapshot_hash_mismatch_rejected() {
    // Przepisany seq + przeniesiony chunk: per-chunk hashe sa OK (przeliczone),
    // ale hash CALOSCI z naglowka juz nie pasuje -> odmowa.
    let bytes = vec![1u8; BASELINE_CHUNK_BYTES * 2 + 10];
    let header = BaselineHeader {
        schema_version: 1,
        epoch: 1,
        tables: vec![],
        row_counts: vec![],
        total_bytes: bytes.len() as u64,
        max_bytes: BASELINE_MAX_TOTAL_BYTES,
        // Naglowek deklaruje hash INNEJ zawartosci (manipulacja calego strumienia).
        content_hash: *blake3::hash(b"different content").as_bytes(),
    };
    let chunks = chunk_snapshot(&bytes);
    let err =
        reassemble_chunks(&chunks, &header).expect_err("whole-snapshot tamper must be caught");
    assert!(format!("{err}").contains("whole-snapshot hash mismatch"));
}

// =============================================================================
// 3. Atomowy import (dwa osobne pule)
// =============================================================================

/// Buduje snapshot dawcy z osobnej puli i importuje go do puli joinera.
fn donor_snapshot(donor: &DbPool, epoch_counter: u64) -> BaselineSnapshot {
    capture_baseline_snapshot(donor, epoch(epoch_counter, "donor-node"), &test_cipher()).unwrap()
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
    import(&joiner, &snap, "donor-node").unwrap();

    let conn = joiner.read().unwrap();
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
    import(&joiner, &snap, "donor-node").unwrap();

    assert!(user_exists(&joiner, "u-donor-only"));
    assert_eq!(
        membership_org(&joiner, "u-donor-only").as_deref(),
        Some("org-donor")
    );
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
    import(&joiner, &snap, "donor-node").unwrap();

    // Dawca zachowuje czysta nazwe.
    assert_eq!(
        username_of(&joiner, "u-donor").as_deref(),
        Some("shared_name")
    );
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
    import(&joiner, &snap, "donor-node").unwrap();

    // Bob zostaje wlasnym userem, ale w org dawcy.
    assert!(user_exists(&joiner, "u-joiner"));
    assert_eq!(
        membership_org(&joiner, "u-joiner").as_deref(),
        Some("org-donor")
    );
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
    let report = import(&joiner, &snap, "donor-node").unwrap();

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
    import(&joiner, &snap, "donor-node").unwrap();

    // Zadnego czlonkostwa w org joinera nie powinno juz byc.
    let conn = joiner.read().unwrap();
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
    import(&joiner, &snap, "donor-node").unwrap();

    // Oba flow zachowane (rozne UUID).
    assert_eq!(count_flows_named(&joiner, "version 2"), 2);

    // Dawca zachowuje published_model_name, joiner ma unpublish (NULL).
    let conn = joiner.read().unwrap();
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
    assert_eq!(
        joiner_model, None,
        "joiner flow unpublished przy kolizji modelu"
    );
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
    import(&joiner, &snap, "donor-node").unwrap();
    // Drugi import tego samego dawcy+epoch jest no-opem (faza Completed).
    let report2 = import(&joiner, &snap, "donor-node").unwrap();
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

    let result = import(&joiner, &bad, "donor-node");
    assert!(result.is_err(), "import z bledem FK musi sie nie powiesc");

    // Joiner nietkniety: jego user i org dalej istnieja, dawcy nie wniknal.
    assert!(user_exists(&joiner, "u-joiner"));
    assert!(!user_exists(&joiner, "u-donor"));
    assert_eq!(
        membership_org(&joiner, "u-joiner").as_deref(),
        Some("org-joiner")
    );
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
    let header = build_baseline_header(&snap, &bytes);
    let chunks = chunk_snapshot(&bytes);
    let rebuilt = reassemble_chunks(&chunks, &header).unwrap();

    run_baseline_adopt(
        &joiner,
        "donor-node",
        JOINER_LOCAL_NODE,
        &rebuilt,
        &test_cipher(),
    )
    .unwrap();
    assert!(user_exists(&joiner, "u-donor"));
    assert_eq!(
        membership_org(&joiner, "u-donor").as_deref(),
        Some("org-donor")
    );
}

// =============================================================================
// 4. Blocker fixes (Faza C codex review)
// =============================================================================

#[test]
fn multi_org_donor_snapshot_is_rejected() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-a", "a");
    seed_org(&donor, "org-b", "b"); // drugi aktywny org -> niedozwolony
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 1);
    let err = import(&joiner, &snap, "donor-node").expect_err("multi-org donor must be rejected");
    assert!(format!("{err}").contains("single-org donor"));
    // Joiner nietkniety — jego org dalej istnieje.
    let conn = joiner.read().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM organizations WHERE org_id = 'org-joiner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn import_brings_donor_sync_nodes_keeps_local_node() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_sync_node(&donor, "donor-cluster-node", "Donor Node");

    seed_org(&joiner, "org-joiner", "joiner");
    seed_sync_node(&joiner, JOINER_LOCAL_NODE, "Joiner Local");

    let snap = donor_snapshot(&donor, 1);
    import(&joiner, &snap, "donor-node").unwrap();

    // Wezel dawcy zaimportowany (joiner poznaje klaster).
    assert!(node_exists(&joiner, "donor-cluster-node"));
    // Lokalny wpis joinera ZACHOWANY.
    assert!(node_exists(&joiner, JOINER_LOCAL_NODE));
}

#[test]
fn import_brings_donor_explicit_shares_and_node_assignments() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_sync_node(&donor, "donor-node-1", "D1");
    seed_explicit_share(&donor, "org-donor", "u-donor", "u-donor");
    {
        let conn = donor.write().unwrap();
        conn.execute(
            "INSERT INTO node_user_assignments (node_id, user_id, assignment_mode, created_by) \
             VALUES ('donor-node-1', 'u-donor', 'primary', 'u-donor')",
            [],
        )
        .unwrap();
    }
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 1);
    import(&joiner, &snap, "donor-node").unwrap();

    // Explicit share dawcy obecny po imporcie i zremapowany do org dawcy.
    assert_eq!(explicit_share_count(&joiner, "org-donor", "u-donor"), 1);
    // node_user_assignment dawcy obecny.
    let conn = joiner.read().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM node_user_assignments WHERE node_id = 'donor-node-1' AND user_id = 'u-donor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn import_donor_secret_wins_and_is_reencrypted() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_org(&joiner, "org-joiner", "joiner");

    // Dawca i joiner maja ROZNE ciphery i rozne wartosci tego samego sekretu.
    let donor_cipher = other_cipher();
    let joiner_cipher = test_cipher();
    set_secret(&donor, &donor_cipher, "hf_token", "DONOR-TOKEN");
    set_secret(&joiner, &joiner_cipher, "hf_token", "JOINER-TOKEN");

    // Snapshot dawcy odszyfrowuje sekret dawcy do plaintextu (jego cipher).
    let snap = capture_baseline_snapshot(&donor, epoch(1, "donor-node"), &donor_cipher).unwrap();
    // Joiner importuje swoim cipherem (re-encrypt).
    import_baseline(
        &joiner,
        &snap,
        "donor-node",
        JOINER_LOCAL_NODE,
        &joiner_cipher,
    )
    .unwrap();

    // Donor-wins: wartosc dawcy, odczytywalna joinerowym cipherem.
    assert_eq!(
        get_secret(&joiner, &joiner_cipher, "hf_token").as_deref(),
        Some("DONOR-TOKEN")
    );
}

#[test]
fn import_email_match_to_admin_donor_does_not_merge() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user"); // nieuprzywilejowana rola dla nowego czlonka
                                            // Donor admin (is_admin=1) z emailem X.
    seed_admin_user(&donor, "u-donor-admin", "admin", Some("shared@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    // Joiner user z TYM SAMYM emailem co donor-admin.
    seed_user(&joiner, "u-joiner", "bob", Some("shared@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");

    let snap = donor_snapshot(&donor, 1);
    let report = import(&joiner, &snap, "donor-node").unwrap();

    // NIE scalony — joiner user zostaje osobny, dolacza jako zwykly czlonek.
    assert!(
        user_exists(&joiner, "u-joiner"),
        "joiner user musi przetrwac (brak przejecia admina)"
    );
    assert_eq!(report.users_merged_by_email, 0);
    assert_eq!(report.users_joined_donor_org, 1);
    assert_eq!(
        membership_org(&joiner, "u-joiner").as_deref(),
        Some("org-donor")
    );
}

#[test]
fn import_email_match_to_admin_via_role_does_not_merge() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role_perms(&donor, "role-admin", "admin", r#"["org.admin"]"#);
    seed_role(&donor, "role-user", "user");
    // Donor user NIE is_admin, ale ma membership z rola org.admin.
    seed_user(&donor, "u-donor-priv", "priv", Some("shared2@x.io"));
    seed_membership(&donor, "org-donor", "u-donor-priv", "role-admin");

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("shared2@x.io"));

    let snap = donor_snapshot(&donor, 1);
    let report = import(&joiner, &snap, "donor-node").unwrap();

    assert!(user_exists(&joiner, "u-joiner"));
    assert_eq!(
        report.users_merged_by_email, 0,
        "match na admina-przez-role nie scala"
    );
}

#[test]
fn import_new_user_never_gets_privileged_role() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    // TYLKO uprzywilejowana rola u dawcy + zwykla.
    seed_role_perms(&donor, "role-admin", "admin", r#"["org.admin"]"#);
    seed_role_perms(&donor, "role-viewer", "viewer", r#"["org.read"]"#);

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("b@x.io"));

    let snap = donor_snapshot(&donor, 1);
    import(&joiner, &snap, "donor-node").unwrap();

    // Nowy user dostal NIEUPRZYWILEJOWANA role (viewer), nigdy admina.
    let conn = joiner.read().unwrap();
    let role_id: String = conn
        .query_row(
            "SELECT role_id FROM org_memberships WHERE user_id = 'u-joiner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(role_id, "role-viewer");
}

#[test]
fn import_no_nonprivileged_role_refuses_membership() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    // JEDYNA rola u dawcy jest uprzywilejowana.
    seed_role_perms(&donor, "role-admin", "admin", r#"["org.admin"]"#);

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "bob", Some("b@x.io"));

    let snap = donor_snapshot(&donor, 1);
    let err = import(&joiner, &snap, "donor-node")
        .expect_err("must refuse rather than grant a privileged role");
    assert!(format!("{err}").contains("non-privileged role"));
}

#[test]
fn import_sync_policy_same_logical_key_donor_wins() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    // Joiner zna te sama org po id (realny global default-org wspoldzielony przez
    // seed migracji) — FK sync_policies.org_id -> organizations wymaga istnienia.
    seed_org(&joiner, "org-donor", "donor");
    {
        let c = donor.write().unwrap();
        c.execute(
            "INSERT INTO sync_policies (policy_id, org_id, addon_id, resource_type, resource_id, mode) \
             VALUES ('pol-donor', 'org-donor', 'core', 'rt', 'rid', 'local_only')",
            [],
        )
        .unwrap();
    }
    {
        let c = joiner.write().unwrap();
        c.execute(
            "INSERT INTO sync_policies (policy_id, org_id, addon_id, resource_type, resource_id, mode) \
             VALUES ('pol-joiner', 'org-donor', 'core', 'rt', 'rid', 'ephemeral')",
            [],
        )
        .unwrap();
    }

    let snap = donor_snapshot(&donor, 1);
    // Brak abort mimo kolizji UNIQUE(org,addon,type,id).
    import(&joiner, &snap, "donor-node").unwrap();

    let conn = joiner.read().unwrap();
    // Wiersz joinera o tym samym kluczu logicznym usuniety (donor-wins).
    let joiner_pol: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_policies WHERE policy_id = 'pol-joiner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(joiner_pol, 0, "wiersz joinera ustapil dawcy");
    let donor_pol: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_policies WHERE policy_id = 'pol-donor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(donor_pol, 1, "wiersz dawcy obecny");
}

#[test]
fn import_username_collision_probes_next_free_variant() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user"); // nieuprzywilejowana rola dla nowego czlonka
    seed_user(&donor, "u-donor", "shared_name", Some("donor@x.io"));

    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", "shared_name", Some("joiner@x.io"));
    // Wstaw wiersz, ktory ZAJMUJE pierwszy wariant suffixu `shared_name-<short>`,
    // by sondowanie musialo wybrac kolejny wariant.
    let short = &"u-joiner"[..8.min("u-joiner".len())];
    let first_variant = format!("shared_name-{short}");
    seed_user(&joiner, "u-occupant", &first_variant, Some("occ@x.io"));

    let snap = donor_snapshot(&donor, 1);
    import(&joiner, &snap, "donor-node").unwrap();

    // Dawca czysty.
    assert_eq!(
        username_of(&joiner, "u-donor").as_deref(),
        Some("shared_name")
    );
    // Joiner dostal kolejny wariant (nie kolidujacy z occupantem) — brak abort.
    let jn = username_of(&joiner, "u-joiner").unwrap();
    assert!(jn.starts_with("shared_name-"));
    assert_ne!(
        jn, first_variant,
        "musi sondowac dalej, occupant zajmuje pierwszy wariant"
    );
    assert!(user_exists(&joiner, "u-occupant"));
}

#[test]
fn import_resumes_after_epoch_adopt_failure_without_double_import() {
    // Symulacja awarii post-commit: recznie ustawiamy faze `Imported` (DB juz
    // scalony), potem re-pair. Wznawiacz NIE importuje drugi raz — tylko
    // dokancza do `Completed`.
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 7);
    // Pierwszy pelny import (in-process: post-commit epoch-adopt jest no-opem,
    // konczy w `Completed`). Symulujemy awarie cofajac faze do `Imported`.
    import(&joiner, &snap, "donor-node").unwrap();
    store_adopt_state(
        &joiner,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: "donor-node".into(),
            epoch: snap.epoch.clone(),
            phase: BaselinePhase::Imported,
        },
    )
    .unwrap();

    // Re-pair: wznawia tylko post-commit, NIE importuje drugi raz.
    let report = import(&joiner, &snap, "donor-node").unwrap();
    assert_eq!(report.users_merged_by_email, 0);
    assert_eq!(report.users_joined_donor_org, 0);
    assert!(user_exists(&joiner, "u-donor"));

    // Stan koncowy: Completed.
    let st = load_adopt_state(&joiner).unwrap().unwrap();
    assert_eq!(st.phase, BaselinePhase::Completed);
}

// =============================================================================
// CRITICAL 1: single-flight blokuje KAZDY rozny cel (nie tylko przeciwna role)
// =============================================================================

#[test]
fn begin_adopt_atomic_same_role_different_peer_in_progress_rejected() {
    let pool = new_pool();
    // Pierwszy start: joiner wobec peer-a, w toku (faza != Completed).
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Importing,
        )
        .unwrap(),
        BeginOutcome::Started
    ));

    // Drugi start TEJ SAMEJ roli (joiner) ale wobec INNEGO peera — single-flight
    // musi odmowic (dawniej przechodzil i nadpisywal stan).
    assert!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-b",
            &epoch(1, "b"),
            BaselinePhase::Importing,
        )
        .is_err(),
        "ten sam role lecz inny peer w toku musi byc odrzucony"
    );

    // Ta sama rola, ten sam peer, INNY epoch — tez konflikt (rozny cel).
    assert!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(2, "a"),
            BaselinePhase::Importing,
        )
        .is_err(),
        "ten sam role+peer lecz inny epoch w toku musi byc odrzucony"
    );

    // Stan single-flight nietkniety: dalej peer-a/epoch 1.
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.peer, "peer-a");
    assert_eq!(st.epoch.counter, 1);
}

#[test]
fn begin_adopt_atomic_identical_start_is_allowed_resumption() {
    let pool = new_pool();
    // Start: joiner/peer-a/epoch 1 w fazie Elected.
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));

    // Identyczny cel (role+peer+epoch) we wczesnej fazie — wznowienie, nie konflikt.
    // Posuwamy faze (Elected -> Importing): brak bledu, stan zaktualizowany.
    assert!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Importing,
        )
        .is_ok(),
        "identyczny cel musi byc dozwolony (wznowienie)"
    );
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.phase, BaselinePhase::Importing);
}

// =============================================================================
// CRITICAL 2: faza Imported zapisana ATOMOWO z merge'em (jeden commit)
// =============================================================================

#[test]
fn import_commits_imported_phase_atomically_never_leaves_importing() {
    // Po udanym imporcie DB jest scalony i faza NIGDY nie jest `Importing`:
    // `Imported` jest zapisana w tej samej transakcji co merge, a in-process
    // post-commit (no-op) dociaga do `Completed`. Gdyby zapis fazy byl po
    // commicie i padl, faza utknelaby na `Importing` przy scalonym DB.
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_org(&joiner, "org-joiner", "joiner");

    let snap = donor_snapshot(&donor, 11);
    import(&joiner, &snap, "donor-node").unwrap();

    // DB scalony.
    assert!(user_exists(&joiner, "u-donor"));
    // Faza nigdy nie zostaje na `Importing` po scaleniu.
    let st = load_adopt_state(&joiner).unwrap().unwrap();
    assert_ne!(st.phase, BaselinePhase::Importing);
    assert!(matches!(
        st.phase,
        BaselinePhase::Imported | BaselinePhase::Completed
    ));
}

#[test]
fn import_rollback_does_not_persist_imported_phase() {
    // Gdy merge cofa sie przed commitem (FK violation w trakcie upsertu dawcy),
    // ANI DB ANI faza `Imported` nie sa utrwalone — faza zostaje na `Importing`
    // (z `begin_adopt_atomic`), a re-pair zrobi pelny import. To dowodzi, ze
    // zapis `Imported` dzieli transakcje z merge'em (rollback cofa oba).
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("a@x.io"));
    seed_membership(&donor, "org-donor", "u-donor", "role-user");
    seed_org(&joiner, "org-joiner", "joiner");

    let mut snap = donor_snapshot(&donor, 12);
    // Psujemy snapshot: membership wskazuje role, ktorej snapshot NIE niesie ->
    // INSERT membership lamie FK roles(role_id) wewnatrz transakcji importu.
    for m in &mut snap.org_memberships {
        m.role_id = "role-does-not-exist".to_string();
    }

    let res = import(&joiner, &snap, "donor-node");
    assert!(res.is_err(), "import z naruszeniem FK musi zwrocic blad");

    // DB joinera nietkniety: user dawcy NIE zostal scalony (rollback).
    assert!(!user_exists(&joiner, "u-donor"));
    // Faza utknela na `Importing` (zapis `Imported` byl w cofnietej transakcji).
    let st = load_adopt_state(&joiner).unwrap().unwrap();
    assert_eq!(st.phase, BaselinePhase::Importing);
}

// =============================================================================
// HIGH 3: remap tabel security przy merge tozsamosci (brak dangling refs)
// =============================================================================

#[test]
fn merge_identity_remaps_security_tables_no_dangling_refs() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");
    seed_user(&donor, "u-donor", "alice", Some("same@x.io"));
    seed_identity_key(&donor, "k-donor", "u-donor", "DONOR_PUBKEY");

    seed_org(&joiner, "org-joiner", "joiner");
    // Joiner user z TYM SAMYM emailem (zostanie scalony z u-donor, lokalny znika).
    seed_user(&joiner, "u-joiner", "alice_local", Some("same@x.io"));
    seed_membership(&joiner, "org-joiner", "u-joiner", "role-user");
    // Lokalne wiersze security wskazujace na usera joinera (FK na user_accounts).
    seed_node_owner(&joiner, "joiner-owned-node", "u-joiner");
    seed_identity_key(&joiner, "k-joiner", "u-joiner", "JOINER_PUBKEY");
    seed_explicit_share(&joiner, "org-joiner", "u-joiner", "u-joiner");

    let snap = donor_snapshot(&donor, 1);
    let report = import(&joiner, &snap, "donor-node").unwrap();
    assert_eq!(report.users_merged_by_email, 1);

    // Lokalny user joinera usuniety; user dawcy obecny.
    assert!(!user_exists(&joiner, "u-joiner"));
    assert!(user_exists(&joiner, "u-donor"));

    // sync_nodes.owner_user_id przepiete na dawce (nie wyzerowane przez FK).
    assert_eq!(
        node_owner_of(&joiner, "joiner-owned-node").as_deref(),
        Some("u-donor")
    );

    // sync_explicit_shares: subject_id i granted_by przepiete na dawce (org_id
    // share'a nie jest remapowany — to zasob w org joinera, ktora wciaz istnieje).
    assert_eq!(explicit_share_count(&joiner, "org-joiner", "u-donor"), 1);
    assert_eq!(explicit_share_count(&joiner, "org-joiner", "u-joiner"), 0);
    assert_eq!(
        share_granted_by(&joiner, "u-donor").as_deref(),
        Some("u-donor")
    );

    // user_identity_keys: donor-wins. Klucz joinera usuniety, klucz dawcy zostaje.
    assert_eq!(
        identity_key_count(&joiner, "u-joiner"),
        0,
        "klucz joinera usuniety"
    );
    assert_eq!(
        identity_key_count(&joiner, "u-donor"),
        1,
        "klucz dawcy zachowany"
    );

    // Brak zadnych dangling refs (FK check czysty).
    assert_eq!(
        foreign_key_violations(&joiner),
        0,
        "brak naruszen FK po merge tozsamosci"
    );
}

// =============================================================================
// HIGH 5: suffix-probe terminuje przy max-dlugiej bazie (nie petli do u32::MAX)
// =============================================================================

#[test]
fn import_max_length_base_collision_terminates_with_unique_value() {
    let donor = new_pool();
    let joiner = new_pool();
    seed_org(&donor, "org-donor", "donor");
    seed_role(&donor, "role-user", "user");

    // Username dawcy o MAKSYMALNEJ dlugosci (>= limit kolizji). Joiner ma user o
    // tej samej (max) nazwie -> kolizja UNIQUE wymaga suffixu mieszczacego sie w
    // limicie. Stary kod ucinalby zmienny licznik i petlil do u32::MAX.
    let long_name: String = std::iter::repeat('a').take(300).collect();
    seed_user(&donor, "u-donor", &long_name, Some("donor@x.io"));
    seed_org(&joiner, "org-joiner", "joiner");
    seed_user(&joiner, "u-joiner", &long_name, Some("joiner@x.io"));

    let snap = donor_snapshot(&donor, 1);
    // Nie petli, nie panikuje: import konczy sie sukcesem.
    import(&joiner, &snap, "donor-node").unwrap();

    // Dawca zachowuje (obcieta do limitu) nazwe, joiner dostaje INNA, unikalna,
    // mieszczaca sie w limicie dlugosci wartosc.
    let donor_name = username_of(&joiner, "u-donor").unwrap();
    let joiner_name = username_of(&joiner, "u-joiner").unwrap();
    assert_ne!(donor_name, joiner_name, "wartosci musza byc rozne");
    assert!(
        joiner_name.chars().count() <= 200,
        "suffixowana wartosc joinera musi miescic sie w limicie: {} znakow",
        joiner_name.chars().count()
    );
}

// =============================================================================
// Staleness-aware single-flight: abandoned adopt states free the slot via TTL
// =============================================================================

/// Rewrites `settings.updated_at` of the persisted adopt state with an SQLite
/// datetime modifier (e.g. "-11 minutes") or a raw literal, to simulate an
/// adopt state armed long ago by a counterpart that never came back.
fn set_adopt_state_updated_at(pool: &DbPool, value_sql: &str) {
    let conn = pool.write().unwrap();
    let changed = conn
        .execute(
            &format!("UPDATE settings SET updated_at = {value_sql} WHERE key = ?1"),
            params![BASELINE_ADOPT_STATE_KEY],
        )
        .unwrap();
    assert_eq!(changed, 1, "adopt state row must exist to backdate");
}

#[test]
fn begin_adopt_atomic_evicts_stale_elected_state() {
    let pool = new_pool();
    // Armed-only state (Elected) from an old pairing whose joiner never pulled.
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Donor,
            "ghost-peer",
            &epoch(0, "g"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    set_adopt_state_updated_at(&pool, "datetime('now','-11 minutes')");

    // A different target may now take over the slot instead of being refused.
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "live-peer",
            &epoch(3, "l"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.role, BaselineRole::Joiner);
    assert_eq!(st.peer, "live-peer");
    assert_eq!(st.epoch.counter, 3);
}

#[test]
fn begin_adopt_atomic_fresh_elected_still_refuses_different_target() {
    let pool = new_pool();
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Donor,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    // Within the armed TTL the slot still refuses any different target.
    set_adopt_state_updated_at(&pool, "datetime('now','-5 minutes')");
    assert!(begin_adopt_atomic(
        &pool,
        BaselineRole::Joiner,
        "peer-b",
        &epoch(2, "b"),
        BaselinePhase::Elected,
    )
    .is_err());
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.peer, "peer-a", "fresh state must not be evicted");
}

#[test]
fn begin_adopt_atomic_evicts_stale_active_phase() {
    let pool = new_pool();
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Receiving,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    // An active phase gets the longer TTL: 61 minutes is past it.
    set_adopt_state_updated_at(&pool, "datetime('now','-61 minutes')");
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Donor,
            "peer-b",
            &epoch(2, "b"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.peer, "peer-b");
}

#[test]
fn begin_adopt_atomic_fresh_active_phase_still_refuses() {
    let pool = new_pool();
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Receiving,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    // 30 minutes is stale for an armed slot but NOT for an active transfer.
    set_adopt_state_updated_at(&pool, "datetime('now','-30 minutes')");
    assert!(begin_adopt_atomic(
        &pool,
        BaselineRole::Donor,
        "peer-b",
        &epoch(2, "b"),
        BaselinePhase::Elected,
    )
    .is_err());
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.peer, "peer-a");
    assert_eq!(st.phase, BaselinePhase::Receiving);
}

#[test]
fn begin_adopt_atomic_same_target_resume_unaffected_by_age() {
    let pool = new_pool();
    store_adopt_state(
        &pool,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: "donor-node".into(),
            epoch: epoch(4, "d"),
            phase: BaselinePhase::Imported,
        },
    )
    .unwrap();
    // Even a very old same-target Imported state must resume, never restart:
    // the DB is already merged, only post-commit work remains.
    set_adopt_state_updated_at(&pool, "datetime('now','-2 hours')");
    match begin_adopt_atomic(
        &pool,
        BaselineRole::Joiner,
        "donor-node",
        &epoch(4, "d"),
        BaselinePhase::Receiving,
    )
    .unwrap()
    {
        BeginOutcome::Resume(st) => assert_eq!(st.phase, BaselinePhase::Imported),
        BeginOutcome::Started => panic!("same-target Imported must resume, not restart"),
    }
}

#[test]
fn begin_adopt_atomic_garbage_updated_at_treated_as_stale() {
    let pool = new_pool();
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Donor,
            "peer-a",
            &epoch(1, "a"),
            BaselinePhase::Receiving,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    // A corrupt timestamp must not wedge the slot forever -> treated as stale.
    set_adopt_state_updated_at(&pool, "'not-a-datetime'");
    assert!(matches!(
        begin_adopt_atomic(
            &pool,
            BaselineRole::Joiner,
            "peer-b",
            &epoch(2, "b"),
            BaselinePhase::Elected,
        )
        .unwrap(),
        BeginOutcome::Started
    ));
    let st = load_adopt_state(&pool).unwrap().unwrap();
    assert_eq!(st.peer, "peer-b");
}
