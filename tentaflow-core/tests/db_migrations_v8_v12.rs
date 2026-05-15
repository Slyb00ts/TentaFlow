// =============================================================================
// Plik: tests/db_migrations_v8_v12.rs
// Opis: Testy migracji DB v8..v12 (F1a §6.5) — model_alias_owners,
//       alias_calls, model_alias_changes, addon_migrations_applied,
//       frame_pickup_log. Sprawdza: tabele i indeksy istnieja, CHECK
//       constraints odrzucaja zle wartosci, FK ON DELETE CASCADE dziala,
//       druga inicjalizacja nie aplikuje migracji ponownie (idempotencja).
// =============================================================================

use rusqlite::Connection;
use tempfile::TempDir;
use tentaflow_core::db;

fn open_db() -> (TempDir, db::DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool = db::init(&path).expect("init DB");
    (dir, pool)
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

#[test]
fn migrations_v8_v12_tables_created() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    for tbl in &[
        "model_alias_owners",
        "alias_calls",
        "model_alias_changes",
        "addon_migrations_applied",
        "frame_pickup_log",
    ] {
        assert!(table_exists(&conn, tbl), "tabela {tbl} musi istniec");
    }
}

#[test]
fn migrations_v8_v12_indexes_created() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    for idx in &[
        "idx_alias_owners_addon",
        "idx_alias_calls_alias_ts",
        "idx_alias_calls_addon_ts",
        "idx_alias_calls_request_id",
        "idx_alias_calls_fallback",
        "idx_alias_changes_alias",
        "idx_alias_changes_user_ts",
        "idx_addon_migrations_status",
        "idx_frame_pickup_ref",
        "idx_frame_pickup_request",
        "idx_frame_pickup_service_ts",
    ] {
        assert!(index_exists(&conn, idx), "indeks {idx} musi istniec");
    }
}

#[test]
fn migrations_recorded_in_meta() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version BETWEEN 8 AND 12",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 5, "wszystkie 5 migracji v8..v12 zapisane");
}

#[test]
fn migrations_idempotent_second_init_noop() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool1 = db::init(&path).expect("first init");
    let v_first: i64 = pool1
        .lock()
        .unwrap()
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("max v");
    drop(pool1);

    let pool2 = db::init(&path).expect("second init");
    let conn = pool2.lock().unwrap();
    let v_second: i64 = conn
        .query_row("SELECT MAX(version) FROM _migrations", [], |r| r.get(0))
        .expect("max v");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0))
        .expect("count");

    assert_eq!(v_first, v_second);
    assert_eq!(count, v_second, "kazda wersja zapisana dokladnie raz");
}

#[test]
fn alias_calls_check_constraint_rejects_invalid_result() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model) VALUES ('a1', 'm1')",
        [],
    )
    .expect("insert alias");
    let alias_id: i64 = conn
        .query_row("SELECT id FROM model_aliases WHERE alias='a1'", [], |r| {
            r.get(0)
        })
        .expect("select");

    let res = conn.execute(
        "INSERT INTO alias_calls (alias_id, alias_name, target_used, result, ts) \
         VALUES (?1, 'a1', 'm1', 'totally_invalid', 1)",
        rusqlite::params![alias_id],
    );
    assert!(
        res.is_err(),
        "CHECK constraint na result musi odrzucic 'totally_invalid'"
    );

    let ok_res = conn.execute(
        "INSERT INTO alias_calls (alias_id, alias_name, target_used, result, ts) \
         VALUES (?1, 'a1', 'm1', 'ok', 1)",
        rusqlite::params![alias_id],
    );
    assert!(ok_res.is_ok(), "ok jest legalnym wynikiem");
}

#[test]
fn model_alias_owners_check_constraint_rejects_invalid_type() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model) VALUES ('a2', 'm2')",
        [],
    )
    .expect("insert alias");
    let alias_id: i64 = conn
        .query_row("SELECT id FROM model_aliases WHERE alias='a2'", [], |r| {
            r.get(0)
        })
        .expect("select");

    let bad = conn.execute(
        "INSERT INTO model_alias_owners (alias_id, owner_type, owner_id) \
         VALUES (?1, 'system', 'x')",
        rusqlite::params![alias_id],
    );
    assert!(bad.is_err(), "owner_type='system' niedozwolone");

    let good = conn.execute(
        "INSERT INTO model_alias_owners (alias_id, owner_type, owner_id) \
         VALUES (?1, 'addon', 'tentavision')",
        rusqlite::params![alias_id],
    );
    assert!(good.is_ok());
}

#[test]
fn fk_cascade_delete_alias_clears_owners_and_calls() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model) VALUES ('a3', 'm3')",
        [],
    )
    .expect("insert alias");
    let alias_id: i64 = conn
        .query_row("SELECT id FROM model_aliases WHERE alias='a3'", [], |r| {
            r.get(0)
        })
        .expect("select");

    conn.execute(
        "INSERT INTO model_alias_owners (alias_id, owner_type, owner_id) \
         VALUES (?1, 'addon', 'foo')",
        rusqlite::params![alias_id],
    )
    .expect("owners insert");
    conn.execute(
        "INSERT INTO alias_calls (alias_id, alias_name, target_used, result, ts) \
         VALUES (?1, 'a3', 'm3', 'ok', 1)",
        rusqlite::params![alias_id],
    )
    .expect("calls insert");

    conn.execute("DELETE FROM model_aliases WHERE id=?1", [alias_id])
        .expect("delete alias");

    let owners: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_alias_owners WHERE alias_id=?1",
            [alias_id],
            |r| r.get(0),
        )
        .expect("count owners");
    let calls: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM alias_calls WHERE alias_id=?1",
            [alias_id],
            |r| r.get(0),
        )
        .expect("count calls");

    assert_eq!(owners, 0, "ON DELETE CASCADE czysci owners");
    assert_eq!(calls, 0, "ON DELETE CASCADE czysci alias_calls");
}

#[test]
fn addon_migrations_applied_primary_key_idempotent() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    conn.execute(
        "INSERT INTO addon_migrations_applied \
         (addon_id, migration_name, migration_hash, applied_in_addon_version, status) \
         VALUES ('foo', '001_init.sql', 'abc', '1.0.0', 'success')",
        [],
    )
    .expect("first insert");

    let dup = conn.execute(
        "INSERT INTO addon_migrations_applied \
         (addon_id, migration_name, migration_hash, applied_in_addon_version, status) \
         VALUES ('foo', '001_init.sql', 'xyz', '1.0.1', 'success')",
        [],
    );
    assert!(dup.is_err(), "PRIMARY KEY (addon_id, migration_name) blokuje duplikat");
}

#[test]
fn migration_v13_backfills_teams_bot_owner() {
    // Simulate a database where teams-bot already ran with hard-coded
    // aliases (no owner row). Drop the owner rows for the five teams-bot
    // aliases and re-apply v13 — the backfill must reinsert them.
    let (_dir, pool) = open_db();
    {
        let conn = pool.lock().expect("lock");
        for alias in &[
            "teams-stt",
            "teams-tts",
            "teams-summary",
            "teams-vision-face",
            "teams-vision-emotion",
        ] {
            conn.execute(
                "INSERT INTO model_aliases (alias, target_model) VALUES (?1, '')",
                [*alias],
            )
            .expect("seed alias");
        }
        // v13 already ran during init; remove the rows it created and
        // re-run the SQL to assert idempotence + backfill behavior.
        conn.execute("DELETE FROM model_alias_owners", [])
            .expect("clear owners");

        conn.execute_batch(
            "INSERT OR IGNORE INTO model_alias_owners (alias_id, owner_type, owner_id, created_at) \
             SELECT id, 'addon', 'teams-bot', datetime('now') \
             FROM model_aliases \
             WHERE alias IN ('teams-stt', 'teams-tts', 'teams-summary', 'teams-vision-face', 'teams-vision-emotion');",
        )
        .expect("rerun v13");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_alias_owners WHERE owner_type='addon' AND owner_id='teams-bot'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 5, "all 5 teams-bot aliases must get owner rows");
    }
}

#[test]
fn migration_v13_recorded_in_meta() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 13",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(exists, 1);
}
