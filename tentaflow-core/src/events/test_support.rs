// ===== File: events/test_support.rs — fixtures shared by the event-log tests =====
//
// One place for the two databases every test in this module needs, so a change
// to how either is opened does not have to be repeated four times.

use std::path::Path;
use std::sync::Arc;

use crate::db::DbPool;

/// A main database with the full core schema, including migration v129 — the
/// one that seeds the `events` retention policy and adds
/// `audit_log.correlation_id`.
pub fn main_db() -> DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("memory db");
    crate::db::migrations::run(&conn).expect("core migrations");
    Arc::new(crate::db::Db::from_connection(conn))
}

/// An event log in its own temporary directory. Returns the directory so the
/// caller keeps it alive for the length of the test.
pub fn events_db() -> (tempfile::TempDir, DbPool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = open_events_db(dir.path());
    (dir, pool)
}

/// A SECOND, independent connection to an event log that already exists — what
/// a genuinely concurrent writer looks like. Going through the same `DbPool`
/// would only prove that a `Mutex` serialises.
pub fn open_events_db(dir: &Path) -> DbPool {
    super::db::open_pool_at(&dir.join("events.db")).expect("open events.db")
}
