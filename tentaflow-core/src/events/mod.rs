// ===== File: events/mod.rs — the run event log (§2 of DOKONCZENIE_RAG_I_ZDARZENIA) =====
//
// The store behind the Events browser: one SQLite file per node holding the
// timeline every run leaves behind, the writer that keeps it honest, the audit
// mirror and the retention sweep. It answers where a call came from, who made
// it and how it went, from ONE table so the browser can ask across origins.
//
// The store is `store` + `db`; `progress_log` is what fills it, by subscribing
// to the flow engine's progress broadcast (§2.6) and translating it — no new
// instrumentation anywhere, every timing a difference between two rows
// (invariant 5). `metrics` reads those differences back out (§2.7). The browser
// (§2.10) is a separate track.

pub mod audit_outbox;
pub mod db;
pub mod metrics;
pub mod progress_log;
pub mod retention;
pub mod store;
#[cfg(test)]
mod test_support;

use anyhow::Result;

pub use store::{
    append, append_in_tx, assistant_body_setting_key, read_run, AppendedEvent, AuditEnvelope,
    BodyOmission, EventKind, EventPayload, ResponseBody, RunEvent, StoredEvent,
};

/// Opens `<data>/events.db`, publishes the pool and STARTS everything the log
/// needs to be more than an empty file: the progress subscriber that fills it,
/// the audit-outbox delivery loop and the retention sweep.
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
    retention::start_retention_task(main_db.clone(), pool.clone());
    progress_log::start(pool, main_db.clone());
    Ok(())
}
