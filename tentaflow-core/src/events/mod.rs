// ===== File: events/mod.rs — the run event log (§2 of DOKONCZENIE_RAG_I_ZDARZENIA) =====
//
// The store behind the Events browser: one SQLite file per node holding the
// timeline every run leaves behind, the writer that keeps it honest, the audit
// mirror and the retention sweep. It answers where a call came from, who made
// it and how it went, from ONE table so the browser can ask across origins.
//
// This module is the STORE. It does not subscribe to the flow engine and it
// exposes no protocol variant or handler: the progress sink (§2.6) and the
// browser (§2.10) are separate tracks that call `store::append` and
// `store::read_run`.

pub mod audit_outbox;
pub mod db;
pub mod retention;
pub mod store;
#[cfg(test)]
mod test_support;

use anyhow::Result;

pub use store::{
    append, append_in_tx, read_run, AppendedEvent, AuditEnvelope, EventKind, EventPayload,
    RunEvent, StoredEvent,
};

/// Opens `<data>/events.db`, publishes the pool and STARTS the two background
/// loops the log needs to be more than a growing file: the audit-outbox
/// delivery loop and the retention sweep.
///
/// Starting them here is the point. The same two mechanisms exist in
/// `code_studio` — `audit_outbox::spawn_delivery_loop`,
/// `workspace_db::spawn_idle_sweeper` and `workspace_db::checkpoint_all` — and
/// none of them has a caller anywhere in the tree, so that outbox has never
/// been drained. One call site, both loops, no way to wire half of it.
///
/// Call once at startup from within the tokio runtime, next to
/// `project_studio::init`.
pub fn init(main_db: &crate::db::DbPool) -> Result<()> {
    let pool = db::init(&crate::paths::data_dir().join("events.db"))?;
    audit_outbox::spawn_delivery_loop(main_db.clone(), pool.clone());
    retention::start_retention_task(main_db.clone(), pool);
    Ok(())
}
