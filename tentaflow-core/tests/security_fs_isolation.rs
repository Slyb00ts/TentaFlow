// =============================================================================
// File: tests/security_fs_isolation.rs — F1a M2.W11 security suite §17.5
// =============================================================================
//
// Per-addon FS sandbox guarantees beyond the unit-level path-traversal tests
// already in `addon::fs_sandbox::tests`:
//   1. Two addons get distinct on-disk paths even when their IDs share a
//      common prefix (regex collision attempt).
//   2. The per-addon SQLite pool is keyed by addon_id — opening pool A then
//      writing through pool B never touches A's underlying file.
//   3. Symlinks pointing outside the sandbox root are rejected when the
//      addon_id validator runs over a sanitized id.
//   4. Closing pool B does not affect pool A (lifetimes isolated).
//
// These tests run sequentially under a HOME env guard exposed via the
// `fs_sandbox` test_home_lock — fs_sandbox itself does the same for its
// inline tests, so the lock is the right serialization primitive.

#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::Path;

use tentaflow_core::addon::fs_sandbox::{addon_data_dir, addon_db_path, validate_addon_id};
use tentaflow_core::addon::storage_sql;

/// Same fixture as `fs_sandbox::tests::with_tmp_home`. Local copy because
/// the upstream helper is `#[cfg(test)]` and not exported. The global
/// mutex serializes HOME mutation across tests in this file — cargo runs
/// tests in this binary in parallel by default and the storage_sql global
/// pool registry shares state across them.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_tmp_home<F: FnOnce()>(f: F) {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if let Some(p) = prev {
        std::env::set_var("HOME", p);
    } else {
        std::env::remove_var("HOME");
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

// =============================================================================
// 1. Distinct paths for distinct addon_ids, even with shared prefixes
// =============================================================================

#[test]
fn distinct_addon_ids_resolve_to_distinct_paths() {
    with_tmp_home(|| {
        let a = addon_data_dir("alpha").expect("alpha ok");
        let b = addon_data_dir("alpha-extra").expect("alpha-extra ok");
        let c = addon_data_dir("beta").expect("beta ok");
        assert_ne!(a, b, "addon_id prefix overlap must not collide on disk");
        assert_ne!(a, c);
        assert_ne!(b, c);
        // None of them must contain a sibling's id as a path component.
        assert!(!a.ends_with("alpha-extra"));
        assert!(!b.ends_with("alpha"));
    });
}

// =============================================================================
// 2. Per-addon SQLite pools are isolated
// =============================================================================

#[test]
fn distinct_addon_pools_back_distinct_files() {
    with_tmp_home(|| {
        let pool_a = storage_sql::open_addon_db("iso-a").expect("pool A");
        let pool_b = storage_sql::open_addon_db("iso-b").expect("pool B");

        // Write a sentinel row through pool A.
        {
            let conn = pool_a.get().expect("a conn");
            conn.execute(
                "CREATE TABLE sentinel (v TEXT NOT NULL)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO sentinel(v) VALUES ('from-A')", []).unwrap();
        }

        // Pool B's database must not see that table at all.
        let b_has_table: i64 = {
            let conn = pool_b.get().expect("b conn");
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sentinel'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            b_has_table, 0,
            "addon B must not observe addon A's table — cross-addon SQL leak"
        );

        // Sanity: A still sees the table.
        let a_has_table: i64 = {
            let conn = pool_a.get().expect("a conn");
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sentinel'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(a_has_table, 1);

        storage_sql::close_addon_db("iso-a");
        storage_sql::close_addon_db("iso-b");
    });
}

// =============================================================================
// 3. Closing one addon's pool does not invalidate another's
// =============================================================================

#[test]
fn close_one_pool_leaves_other_pool_usable() {
    with_tmp_home(|| {
        let pool_a = storage_sql::open_addon_db("close-a").expect("A");
        let _pool_b = storage_sql::open_addon_db("close-b").expect("B");

        storage_sql::close_addon_db("close-b");

        // A must still be usable.
        let conn = pool_a.get().expect("A still usable after B close");
        let one: i64 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(one, 1);

        storage_sql::close_addon_db("close-a");
    });
}

// =============================================================================
// 4. addon_db_path always lives inside addon_data_dir
// =============================================================================

#[test]
fn db_path_is_strict_subpath_of_data_dir() {
    with_tmp_home(|| {
        let dir = addon_data_dir("strict-sub").expect("dir");
        let dbp = addon_db_path("strict-sub").expect("db");
        assert!(
            dbp.starts_with(&dir),
            "db path {:?} must live under data dir {:?}",
            dbp,
            dir
        );
    });
}

// =============================================================================
// 5. Symlinks crafted inside an addon's directory must not be followed
//    blindly by the validator that vets new addon ids
// =============================================================================

#[cfg(unix)]
#[test]
fn validate_addon_id_does_not_resolve_via_symlink() {
    // The validator is purely lexical — it must reject any id that contains
    // `/` regardless of what the filesystem says. Even on a host where
    // `~/.tentaflow/addons/legit/../etc/passwd` would resolve to /etc/passwd,
    // the id `legit/../etc` never reaches the path layer.
    assert!(validate_addon_id("legit/../etc").is_err());
    assert!(validate_addon_id("/etc/passwd").is_err());
    assert!(validate_addon_id("legit\0../etc").is_err());
}

// =============================================================================
// 6. A symlink placed inside addon A's directory pointing at addon B's
//    directory must not let addon B observe A's data via its own pool.
//    (Defence in depth — pool lookup is by id, not by filesystem walk.)
// =============================================================================

#[cfg(unix)]
#[test]
fn symlink_between_addon_dirs_does_not_punch_through_pool_keying() {
    with_tmp_home(|| {
        let dir_a = addon_data_dir("sym-a").expect("A");
        let dir_b = addon_data_dir("sym-b").expect("B");

        // Place a symlink inside A pointing at B's data dir. Pool A opens
        // ~/.tentaflow/addons/sym-a/data.db (not the symlink), so the leak
        // must not be possible regardless of attacker-controlled symlinks.
        let leak = dir_a.join("leak");
        let _ = std::fs::remove_file(&leak);
        symlink(&dir_b, &leak).expect("symlink");

        let pool_a = storage_sql::open_addon_db("sym-a").expect("A pool");
        let conn = pool_a.get().expect("A conn");
        conn.execute("CREATE TABLE marker_a (id INTEGER)", []).unwrap();

        let pool_b = storage_sql::open_addon_db("sym-b").expect("B pool");
        let conn_b = pool_b.get().expect("B conn");
        let saw_a: i64 = conn_b
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='marker_a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saw_a, 0, "B must not see A's table via symlink");

        storage_sql::close_addon_db("sym-a");
        storage_sql::close_addon_db("sym-b");
        let _ = std::fs::remove_file(&leak);
        // Quiet `dir_a unused`.
        assert!(Path::new(&dir_a).is_dir());
    });
}
