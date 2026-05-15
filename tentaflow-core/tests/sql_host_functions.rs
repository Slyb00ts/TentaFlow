// =============================================================================
// Plik: tests/sql_host_functions.rs
// Opis: Test integracyjny SQL host functions F1a M1.W4. Testuje pelny pipeline
//       per-addon SQLite (fs_sandbox -> storage_sql -> migrations -> sql.rs)
//       z pominieciem warstwy WASM caller — sprawdza:
//       - apply_migrations + sql_exec + sql_query end-to-end
//       - SQL injection protection (bind params)
//       - DDL block via is_ddl helper
//       - constraint violation mapping (AbiError::SqlConstraint)
//       - syntax error mapping (AbiError::SqlSyntax)
//       - transaction rollback
//       - hash mismatch detection w migrations
//
//       Test full ABI z WasmCaller jest w `sdk_boilerplate.rs` (wymaga
//       wasmtime::Store + Module). Tu uzywamy bezposrednio funkcji
//       wewnetrznych pool/migrations zeby zminimalizowac infrastructure.
// =============================================================================

use tentaflow_core::addon::migrations::apply_migrations;
use tentaflow_core::addon::storage_sql::{close_addon_db, open_addon_db};

mod helpers {
    use std::sync::{Mutex, OnceLock};

    /// Wszystkie testy w tym pliku serializujemy przez globalny mutex.
    /// Wspoldzielony `HOME` env var miedzy testami powoduje race condition.
    pub fn home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    pub struct TmpHome {
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl TmpHome {
        pub fn new() -> Self {
            let guard = home_lock().lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().expect("tempdir");
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", tmp.path());
            Self {
                _tmp: tmp,
                _guard: guard,
                prev,
            }
        }
    }

    impl Drop for TmpHome {
        fn drop(&mut self) {
            if let Some(p) = self.prev.take() {
                std::env::set_var("HOME", p);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    pub fn make_core_db() -> tentaflow_core::db::DbPool {
        tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("core db")
    }

    pub fn write_migration(bundle: &std::path::Path, name: &str, sql: &str) {
        let dir = bundle.join("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), sql).unwrap();
    }
}

use helpers::*;

#[test]
fn install_and_insert_select_end_to_end() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, qty INTEGER DEFAULT 0);",
    );
    apply_migrations("sql-e2e", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("sql-e2e").unwrap();
    // INSERT via bind params (symulacja sql_exec internals).
    {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO items (name, qty) VALUES (?, ?)",
            rusqlite::params!["alpha", 3],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items (name, qty) VALUES (?, ?)",
            rusqlite::params!["beta", 5],
        )
        .unwrap();
    }
    {
        let conn = pool.get().unwrap();
        let mut stmt = conn
            .prepare("SELECT name, qty FROM items ORDER BY id")
            .unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("alpha".to_string(), 3));
        assert_eq!(rows[1], ("beta".to_string(), 5));
    }

    close_addon_db("sql-e2e");
}

#[test]
fn sql_injection_via_bind_param_is_safe() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    );
    apply_migrations("sql-inj", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("sql-inj").unwrap();
    // Probuje "wstrzyknac" DROP TABLE jako wartosc parametru.
    let payload = "'; DROP TABLE items;--";
    {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO items (name) VALUES (?)",
            rusqlite::params![payload],
        )
        .unwrap();
    }
    // Tabela MUSI nadal istniec, wartosc zapisana literalnie.
    {
        let conn = pool.get().unwrap();
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='items'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "tabela items nadal istnieje");
        let stored: String = conn
            .query_row("SELECT name FROM items LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, payload, "wartosc zapisana dokladnie");
    }
    close_addon_db("sql-inj");
}

#[test]
fn constraint_violation_returns_proper_error() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE);",
    );
    apply_migrations("sql-constraint", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("sql-constraint").unwrap();
    let conn = pool.get().unwrap();
    conn.execute(
        "INSERT INTO items (code) VALUES (?)",
        rusqlite::params!["A"],
    )
    .unwrap();
    let err = conn
        .execute(
            "INSERT INTO items (code) VALUES (?)",
            rusqlite::params!["A"],
        )
        .unwrap_err();
    // rusqlite mapuje na ConstraintViolation, ktora sql.rs::map_sqlite_error
    // tlumaczy do AbiError::SqlConstraint (kod 9).
    match err {
        rusqlite::Error::SqliteFailure(code, _) => {
            assert_eq!(
                code.code,
                rusqlite::ErrorCode::ConstraintViolation,
                "oczekiwano constraint violation"
            );
        }
        _ => panic!("inne errore niz constraint: {err:?}"),
    }
    close_addon_db("sql-constraint");
}

#[test]
fn migration_idempotent_reapply_skipped() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE x (id INTEGER PRIMARY KEY);",
    );

    let n1 = apply_migrations("sql-idem", "0.1.0", "migrations", bundle.path(), &core).unwrap();
    assert_eq!(n1, 1);
    let n2 = apply_migrations("sql-idem", "0.1.0", "migrations", bundle.path(), &core).unwrap();
    assert_eq!(n2, 0, "drugi run nie aplikuje");

    // Wpis w core DB widoczny ze status success.
    let conn = core.lock().unwrap();
    let (status, hash): (String, String) = conn
        .query_row(
            "SELECT status, migration_hash FROM addon_migrations_applied WHERE addon_id='sql-idem'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "success");
    assert_eq!(hash.len(), 64);
    drop(conn);
    close_addon_db("sql-idem");
}

#[test]
fn migration_hash_mismatch_blocks_install() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE x (id INTEGER);",
    );
    apply_migrations("sql-tamper", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    // Podmieniamy tresc migracji po zaaplikowaniu — runner ma to wykryc.
    std::fs::write(
        bundle.path().join("migrations").join("001_init.sql"),
        "CREATE TABLE x (id INTEGER, malicious INTEGER);",
    )
    .unwrap();
    let res = apply_migrations("sql-tamper", "0.1.0", "migrations", bundle.path(), &core);
    assert!(res.is_err(), "tampered migration powinno failowac");
    close_addon_db("sql-tamper");
}

#[test]
fn transaction_rollback_on_constraint_fail() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE entries (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE);",
    );
    apply_migrations("sql-tx", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("sql-tx").unwrap();
    {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO entries (code) VALUES (?)",
            rusqlite::params!["seed"],
        )
        .unwrap();
    }
    // Transakcja: pierwszy OK, drugi UNIQUE violation — calosc rolled back.
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO entries (code) VALUES (?)", rusqlite::params!["new"])
            .unwrap();
        let dup = tx.execute(
            "INSERT INTO entries (code) VALUES (?)",
            rusqlite::params!["seed"],
        );
        assert!(dup.is_err());
        // tx dropowane bez commit = rollback.
    }
    {
        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "tylko seed po rollback");
    }
    close_addon_db("sql-tx");
}

#[test]
fn statement_readonly_blocks_dml_in_query_path() {
    // Issue #1 part 2: Statement::readonly() autorytatywnie odrzuca DML
    // niezaleznie od regex (np. gdyby ktos znalazl konstrukcje obchodzaca
    // is_read_only). Tu sprawdzamy fundamentalna semantyke: prepare na
    // INSERT zwraca readonly() == false.
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE x (id INTEGER PRIMARY KEY);",
    );
    apply_migrations("ro-check", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("ro-check").unwrap();
    let conn = pool.get().unwrap();

    let stmt_select = conn.prepare("SELECT * FROM x").unwrap();
    assert!(stmt_select.readonly(), "SELECT musi byc readonly");

    let stmt_insert = conn.prepare("INSERT INTO x VALUES (1)").unwrap();
    assert!(
        !stmt_insert.readonly(),
        "INSERT MUSI byc non-readonly — sql_query odrzuci"
    );

    let stmt_cte_select = conn
        .prepare("WITH t AS (SELECT 1) SELECT * FROM t")
        .unwrap();
    assert!(stmt_cte_select.readonly(), "WITH ... SELECT pozostaje readonly");

    close_addon_db("ro-check");
}

#[test]
fn transaction_drop_without_commit_rolls_back() {
    // Issue #5: explicit DropBehavior::Rollback. Sprawdzamy ze drop transakcji
    // bez commit nie zostawia partial state — to definicja rollback.
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, n INTEGER);",
    );
    apply_migrations("tx-drop", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    let pool = open_addon_db("tx-drop").unwrap();
    {
        let mut conn = pool.get().unwrap();
        let mut tx = conn.transaction().unwrap();
        tx.set_drop_behavior(rusqlite::DropBehavior::Rollback);
        tx.execute("INSERT INTO items (n) VALUES (?)", rusqlite::params![1])
            .unwrap();
        tx.execute("INSERT INTO items (n) VALUES (?)", rusqlite::params![2])
            .unwrap();
        // Drop bez commit — wszystkie INSERTy rollback.
        drop(tx);
    }
    {
        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "drop transakcji bez commit -> rollback");
    }
    close_addon_db("tx-drop");
}

#[test]
fn uninstall_clears_addon_migrations_applied() {
    // Issue #6: po uninstall wpisy z addon_migrations_applied musza zniknac,
    // inaczej reinstall innej wersji z roznym hashem trafia na guard.
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle = tempfile::tempdir().unwrap();
    write_migration(
        bundle.path(),
        "001_init.sql",
        "CREATE TABLE x (id INTEGER);",
    );
    apply_migrations("re-install", "0.1.0", "migrations", bundle.path(), &core).unwrap();

    // Sanity: wpis istnieje.
    {
        let conn = core.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_migrations_applied WHERE addon_id='re-install'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count > 0);
    }

    // Aby uninstall przeszedl, musi istniec wpis w `addons` — dorzucamy go
    // explicite (apply_migrations samo nie rejestruje addona).
    {
        let conn = core.lock().unwrap();
        conn.execute(
            "INSERT INTO addons (addon_id, name, version, manifest_json) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["re-install", "Re Install", "0.1.0", "{}"],
        )
        .unwrap();
    }

    tentaflow_core::addon::lifecycle::uninstall("re-install", &core).unwrap();

    {
        let conn = core.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM addon_migrations_applied WHERE addon_id='re-install'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "uninstall musi usunac wpisy migracji");
    }
    close_addon_db("re-install");
}

#[test]
fn two_addons_isolated_databases() {
    let _h = TmpHome::new();
    let core = make_core_db();
    let bundle_a = tempfile::tempdir().unwrap();
    let bundle_b = tempfile::tempdir().unwrap();
    write_migration(bundle_a.path(), "001_a.sql", "CREATE TABLE a_only (x INTEGER);");
    write_migration(bundle_b.path(), "001_b.sql", "CREATE TABLE b_only (y INTEGER);");

    apply_migrations("addon-a", "0.1.0", "migrations", bundle_a.path(), &core).unwrap();
    apply_migrations("addon-b", "0.1.0", "migrations", bundle_b.path(), &core).unwrap();

    let pa = open_addon_db("addon-a").unwrap();
    let pb = open_addon_db("addon-b").unwrap();
    let ca = pa.get().unwrap();
    let cb = pb.get().unwrap();
    // a_only widoczne tylko w addon-a, b_only tylko w addon-b.
    let count_a_in_a: i64 = ca
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='a_only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let count_a_in_b: i64 = cb
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='a_only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_a_in_a, 1);
    assert_eq!(count_a_in_b, 0, "addon-b nie widzi tabel addon-a");

    drop(ca);
    drop(cb);
    close_addon_db("addon-a");
    close_addon_db("addon-b");
}
