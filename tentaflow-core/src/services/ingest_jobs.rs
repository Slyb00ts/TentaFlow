// ===== File: services/ingest_jobs.rs — durable ingest queue for the Project Studio worker =====
//
// Ingest work used to live only in the process that accepted it: Project
// Studio spawned a task and a restart silently dropped everything that had not
// finished. The queue makes the work durable — it is enqueued before the
// request returns and a worker claims it afterwards, in this process or in the
// next one. `queue` namespaces the rows per consumer, so a second consumer
// needs no second file, but today Project Studio is the only one.
//
// WHY A SEPARATE FILE (`<data>/jobs.db`)
//
// The same argument as the event log: the main database has ONE writer
// connection and every write serialises on it (`db/mod.rs`: "`write()` bierze
// jedyne polaczenie pisarza spod `Mutex`"), so a queue that claims, heartbeats
// and finishes would contend with settings, flows, agents and audit writes.
// It is separate from `events.db` because the event log is the one designed to
// be thrown away in bulk: its retention sweep is table-scoped today, but the
// planned monthly ROTATION (`docs/DOKONCZENIE_RAG_I_ZDARZENIA.md` §2.9) retires
// the FILE, and a rotation that retires the file would take outstanding queue
// rows with it.
//
// The queue therefore cannot enter the Sync Ledger, and that is a property of
// WHERE it lives rather than a rule someone has to remember: the ledger only
// reads tables listed in `sync::core_registry::CORE_SYNC_DESCRIPTORS`, and it
// reads every one of them out of the MAIN pool. A table in another file with no
// descriptor is unreachable to it. It also MUST NOT sync: a claim names the
// process instance holding the job, so a replicated row would let one node
// believe another node's worker is its own — and both would run the same
// ingest.
//
// WHAT THE FILE HOLDS: outstanding work only. `finish` DELETES the row, so the
// table never accumulates history and needs no retention of its own. Terminal
// state belongs to the subsystem that owns the job (Project Studio keeps it in
// the project's own `ingest_jobs` row) — the queue answers "what still has to
// run", nothing else.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::{info, warn};

use crate::db::DbPool;

/// Queue name of the Project Studio ingest worker. A queue is the consumer's
/// namespace inside the shared file: `claim` only ever hands a worker a job
/// enqueued for it.
pub const QUEUE_PROJECT_STUDIO: &str = "project_studio";

/// Global handle to the queue pool, set once in `init`. Mirrors
/// `project_studio::db` / `events::db` so a worker and a cancel request reach
/// the same connection without threading a pool through every call site.
static JOBS_POOL: OnceLock<DbPool> = OnceLock::new();

/// Returns the queue pool, or an error if `init` has not run. An error rather
/// than a panic: a node built without the queue must fail the enqueue loudly at
/// its call site, not abort the worker thread.
pub fn pool() -> Result<DbPool> {
    JOBS_POOL
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ingest queue database not initialised"))
}

/// Forces a WAL checkpoint (TRUNCATE) so a shutdown does not leave an
/// unflushed `-wal` file. No-op when the pool was never opened.
pub fn checkpoint_wal() -> Result<()> {
    let Some(pool) = JOBS_POOL.get() else {
        return Ok(());
    };
    let conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("ingest queue pool write: {e}"))?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    info!("ingest queue WAL checkpoint done");
    Ok(())
}

/// Opens (creating if absent) the queue database, applies the pragmas, runs the
/// migrations and publishes the pool. Idempotent: a second call leaves the
/// original pool in place and returns it.
pub fn init(db_path: &Path) -> Result<DbPool> {
    if let Some(existing) = JOBS_POOL.get() {
        return Ok(existing.clone());
    }

    info!("opening the ingest queue database: {:?}", db_path);
    let pool = open_pool_at(db_path)?;
    let _ = JOBS_POOL.set(pool.clone());
    info!("ingest queue database ready");
    Ok(pool)
}

/// Opens a queue database at `db_path` WITHOUT publishing it as the
/// process-wide pool. Every test gets its own file this way, and the queue
/// operations stay a function of the pool they are handed rather than of global
/// state.
pub fn open_pool_at(db_path: &Path) -> Result<DbPool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA mmap_size=268435456;\
         PRAGMA temp_store=MEMORY;\
         PRAGMA busy_timeout=5000;\
         PRAGMA wal_autocheckpoint=2000;",
    )?;
    run_migrations(&conn)?;
    Ok(Arc::new(crate::db::Db::from_connection(conn)))
}

/// Versioned migration runner. Tracks applied versions in
/// `ingest_jobs_schema_version` and applies each pending step in its own
/// transaction.
fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ingest_jobs_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM ingest_jobs_schema_version",
        [],
        |row| row.get(0),
    )?;

    for (version, sql) in MIGRATIONS {
        if *version > current {
            info!("ingest queue migration {}", version);
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO ingest_jobs_schema_version (version) VALUES (?1)",
                params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}

const MIGRATIONS: &[(i64, &str)] = &[(1, INITIAL_SCHEMA)];

/// `status` admits only the two NON-terminal states, because a finished job is
/// deleted rather than kept — the CHECK is how that decision stays true.
///
/// `owner_instance` is the whole orphan story. It carries the id of the process
/// RUN that claimed the job, minted fresh on every start, so a `running` row
/// stamped with any other value can only have been written by a process that no
/// longer exists. That is the same marker idea as `ml_studio`'s
/// `register_local_run` ("we supervise this" vs "nobody watches this") pushed
/// into the row itself, and it needs no time heuristic: a job is not orphaned
/// because it has been quiet for N minutes, it is orphaned because the process
/// that owned it is gone.
///
/// `cancel_requested` persists a cancel that arrives while the job is queued or
/// between the claim and the worker's in-memory registration — the one window an
/// in-process cancel registry cannot cover.
const INITIAL_SCHEMA: &str = "
CREATE TABLE ingest_jobs (
  job_id           TEXT    PRIMARY KEY,
  queue            TEXT    NOT NULL,
  payload_json     TEXT    NOT NULL,
  status           TEXT    NOT NULL DEFAULT 'queued' CHECK(status IN ('queued','running')),
  owner_instance   TEXT    NOT NULL DEFAULT '',
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  enqueued_at_ms   INTEGER NOT NULL,
  claimed_at_ms    INTEGER,
  heartbeat_at_ms  INTEGER
);
CREATE INDEX ix_ingest_jobs_ready ON ingest_jobs(queue, status, enqueued_at_ms);
CREATE INDEX ix_ingest_jobs_owner ON ingest_jobs(status, owner_instance);
";

/// Identifier of THIS process run. Minted once per start (never persisted), so
/// comparing it against a row's `owner_instance` answers "does the process that
/// claimed this job still exist" without consulting a clock.
pub fn instance_id() -> &'static str {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("ingest queue read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("ingest queue write: {e}")
}

/// One outstanding job. `payload_json` is opaque to the queue — the consumer
/// that enqueued it is the only party that knows how to read it.
#[derive(Debug, Clone)]
pub struct QueuedJob {
    pub job_id: String,
    pub queue: String,
    pub payload_json: String,
    pub cancel_requested: bool,
    pub enqueued_at_ms: i64,
}

/// What a heartbeat learned about the job it beat for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobLiveness {
    /// Still ours and still wanted.
    Running,
    /// Someone asked for cancellation; the worker stops at its next safe point.
    CancelRequested,
    /// The row is gone or belongs to another instance — nothing to run.
    Gone,
}

/// What a cancel request did to the queue row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The job had not been claimed yet and was removed from the queue; it will
    /// never run, so the caller closes its own record. Carries the payload so
    /// the caller can find that record.
    Dequeued(String),
    /// The job is running; the flag is persisted and the next heartbeat
    /// surfaces it.
    Signalled,
    /// No outstanding row — the job already finished or was never queued.
    Unknown,
}

/// Adds a job to the back of `queue`. The caller owns `job_id` (Project Studio
/// reuses the id of its own job row), so a duplicate id is an error rather than
/// a silent second run.
pub fn enqueue(pool: &DbPool, queue: &str, job_id: &str, payload_json: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO ingest_jobs (job_id, queue, payload_json, enqueued_at_ms) \
         VALUES (?1, ?2, ?3, ?4)",
        params![job_id, queue, payload_json, now_ms()],
    )?;
    Ok(())
}

/// Claims the oldest queued job of `queue` for this process as a SINGLE atomic
/// `UPDATE … RETURNING` — two workers can never claim the same job (SQLite
/// serialises the statement on the one writer connection and there is no
/// separate SELECT to race). Splitting this into a SELECT and an UPDATE is
/// exactly the bug `claim_runs_once_under_two_workers` mutates for.
pub fn claim(pool: &DbPool, queue: &str) -> Result<Option<QueuedJob>> {
    let conn = pool.write().map_err(write_err)?;
    conn.query_row(
        "UPDATE ingest_jobs \
            SET status = 'running', owner_instance = ?2, claimed_at_ms = ?3, heartbeat_at_ms = ?3 \
          WHERE job_id = (SELECT job_id FROM ingest_jobs \
                           WHERE queue = ?1 AND status = 'queued' \
                           ORDER BY enqueued_at_ms, job_id LIMIT 1) \
          RETURNING job_id, queue, payload_json, cancel_requested, enqueued_at_ms",
        params![queue, instance_id(), now_ms()],
        read_job,
    )
    .optional()
    .map_err(Into::into)
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedJob> {
    Ok(QueuedJob {
        job_id: row.get(0)?,
        queue: row.get(1)?,
        payload_json: row.get(2)?,
        cancel_requested: row.get::<_, i64>(3)? != 0,
        enqueued_at_ms: row.get(4)?,
    })
}

/// Records progress and asks, in the same statement, whether the job is still
/// ours and still wanted. A worker calls it between units of work: it is the
/// only place a cancel that arrived through another process — or during the
/// window between the claim and the in-memory cancel registration — becomes
/// visible to the running job.
pub fn heartbeat(pool: &DbPool, job_id: &str) -> Result<JobLiveness> {
    let conn = pool.write().map_err(write_err)?;
    let state: Option<i64> = conn
        .query_row(
            "UPDATE ingest_jobs SET heartbeat_at_ms = ?2 \
              WHERE job_id = ?1 AND status = 'running' AND owner_instance = ?3 \
              RETURNING cancel_requested",
            params![job_id, now_ms(), instance_id()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match state {
        None => JobLiveness::Gone,
        Some(0) => JobLiveness::Running,
        Some(_) => JobLiveness::CancelRequested,
    })
}

/// Drops the job from the queue. Called after the consumer has written its own
/// terminal record, so the absence of a queue row always means "this job is
/// accounted for somewhere else".
pub fn finish(pool: &DbPool, job_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute("DELETE FROM ingest_jobs WHERE job_id = ?1", params![job_id])?;
    Ok(())
}

/// How many jobs of `queue` are outstanding AHEAD of `job_id` — enqueued
/// earlier in exactly the order `claim` hands them out. The table holds
/// outstanding work only, so this is a count of real work, not an estimate:
/// it is what lets a consumer say "queued behind N" without claiming to know
/// how long that will take. Returns 0 once the job itself is gone.
pub fn jobs_ahead(pool: &DbPool, queue: &str, job_id: &str) -> Result<i64> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT COUNT(*) FROM ingest_jobs AS other \
           JOIN ingest_jobs AS self_row ON self_row.job_id = ?2 \
          WHERE other.queue = ?1 \
            AND (other.enqueued_at_ms < self_row.enqueued_at_ms \
                 OR (other.enqueued_at_ms = self_row.enqueued_at_ms \
                     AND other.job_id < self_row.job_id))",
        params![queue, job_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Whether the queue still owes work for `job_id`. The consumer's own record
/// says "running" from the moment the job is accepted, so this is how it tells
/// a job waiting for a worker from one whose worker died.
pub fn is_pending(pool: &DbPool, job_id: &str) -> Result<bool> {
    let conn = pool.read().map_err(read_err)?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM ingest_jobs WHERE job_id = ?1",
            params![job_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Requests cancellation.
///
/// A job that no worker has claimed is removed here and now: the callers that
/// cancel (project delete, source delete) then WAIT for a terminal status, and
/// leaving the row for a busy worker to pick up would make that wait depend on
/// how much other ingest is in flight.
///
/// The two statements cannot race a claim between them. The delete only misses
/// when the row is no longer `queued`, and the only transition out of `queued`
/// is a claim into `running` — which is exactly the state the second statement
/// flags. There is no third state to slip into, because terminal rows do not
/// exist here.
pub fn request_cancel(pool: &DbPool, job_id: &str) -> Result<CancelOutcome> {
    let conn = pool.write().map_err(write_err)?;
    let dequeued: Option<String> = conn
        .query_row(
            "DELETE FROM ingest_jobs WHERE job_id = ?1 AND status = 'queued' \
             RETURNING payload_json",
            params![job_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(payload) = dequeued {
        return Ok(CancelOutcome::Dequeued(payload));
    }
    let flagged = conn.execute(
        "UPDATE ingest_jobs SET cancel_requested = 1 WHERE job_id = ?1 AND status = 'running'",
        params![job_id],
    )?;
    Ok(if flagged > 0 {
        CancelOutcome::Signalled
    } else {
        CancelOutcome::Unknown
    })
}

/// Closes the jobs of `queue` left `running` by a process that no longer exists
/// and returns them, so the consumer can close its own record. Run at startup,
/// before any worker claims: a row claimed by THIS process run is by definition
/// supervised and is never touched.
///
/// The delete is scoped to ONE queue because the returned rows are the only
/// notification a consumer gets: deleting another queue's row here would erase
/// the job while its owner never hears about it.
pub fn reconcile_orphans(pool: &DbPool, queue: &str) -> Result<Vec<QueuedJob>> {
    let conn = pool.write().map_err(write_err)?;
    let mut stmt = conn.prepare(
        "DELETE FROM ingest_jobs \
          WHERE queue = ?1 AND status = 'running' AND owner_instance <> ?2 \
         RETURNING job_id, queue, payload_json, cancel_requested, enqueued_at_ms",
    )?;
    let orphans = stmt
        .query_map(params![queue, instance_id()], read_job)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !orphans.is_empty() {
        warn!(
            count = orphans.len(),
            "ingest queue: closing jobs orphaned by a previous process"
        );
    }
    Ok(orphans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = open_pool_at(&dir.path().join("jobs.db")).expect("open queue");
        (dir, pool)
    }

    #[test]
    fn a_queued_job_survives_reopening_the_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jobs.db");
        {
            let pool = open_pool_at(&path).expect("open");
            enqueue(&pool, QUEUE_PROJECT_STUDIO, "job-1", "{\"n\":1}").expect("enqueue");
        }
        // "Restart": the pool (and its connection) is gone, the file is not.
        let pool = open_pool_at(&path).expect("reopen");
        let orphans = reconcile_orphans(&pool, QUEUE_PROJECT_STUDIO).expect("reconcile");
        assert!(orphans.is_empty(), "a queued job was never anyone's to orphan");

        let job = claim(&pool, QUEUE_PROJECT_STUDIO)
            .expect("claim")
            .expect("the job outlived the process");
        assert_eq!(job.job_id, "job-1");
        assert_eq!(job.payload_json, "{\"n\":1}");
        assert_eq!(heartbeat(&pool, "job-1").expect("beat"), JobLiveness::Running);
        finish(&pool, "job-1").expect("finish");
        assert!(!is_pending(&pool, "job-1").expect("pending"));
        assert!(claim(&pool, QUEUE_PROJECT_STUDIO).expect("claim").is_none());
    }

    #[test]
    fn claim_hands_each_job_to_exactly_one_worker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jobs.db");
        let pool = open_pool_at(&path).expect("open");
        const JOBS: usize = 64;
        for i in 0..JOBS {
            enqueue(
                &pool,
                QUEUE_PROJECT_STUDIO,
                &format!("job-{i}"),
                &format!("{{\"i\":{i}}}"),
            )
            .expect("enqueue");
        }

        // Two workers on the SAME pool, hammering `claim` until it runs dry.
        let claimed: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let pool = pool.clone();
            let claimed = claimed.clone();
            workers.push(std::thread::spawn(move || {
                let mut idle = 0;
                loop {
                    match claim(&pool, QUEUE_PROJECT_STUDIO).expect("claim") {
                        Some(job) => {
                            idle = 0;
                            claimed.lock().expect("lock").push(job.job_id);
                        }
                        None => {
                            idle += 1;
                            if idle > 4 {
                                break;
                            }
                            std::thread::yield_now();
                        }
                    }
                }
            }));
        }
        for w in workers {
            w.join().expect("worker");
        }

        let mut ids = claimed.lock().expect("lock").clone();
        ids.sort();
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "a job was claimed twice");
        assert_eq!(ids.len(), JOBS, "a job was lost");
    }

    #[test]
    fn reconcile_closes_a_dead_workers_job_and_spares_a_live_one() {
        let (_dir, pool) = test_pool();
        enqueue(&pool, QUEUE_PROJECT_STUDIO, "mine", "{}").expect("enqueue");
        enqueue(&pool, QUEUE_PROJECT_STUDIO, "waiting", "{}").expect("enqueue");
        let mine = claim(&pool, QUEUE_PROJECT_STUDIO).expect("claim").expect("job");
        assert_eq!(mine.job_id, "mine");

        // A row left `running` by a process run that no longer exists.
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO ingest_jobs (job_id, queue, payload_json, status, \
                 owner_instance, enqueued_at_ms, claimed_at_ms) \
                 VALUES ('dead', ?1, '{}', 'running', 'gone-instance', 1, 1)",
                params![QUEUE_PROJECT_STUDIO],
            )
            .expect("insert orphan");
        }

        let orphans = reconcile_orphans(&pool, QUEUE_PROJECT_STUDIO).expect("reconcile");
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].job_id, "dead");
        assert!(!is_pending(&pool, "dead").expect("pending"));
        // The job THIS process supervises is untouched, and so is the queued one.
        assert_eq!(heartbeat(&pool, "mine").expect("beat"), JobLiveness::Running);
        assert!(is_pending(&pool, "waiting").expect("pending"));
    }

    #[test]
    fn cancel_dequeues_a_waiting_job_and_flags_a_running_one() {
        let (_dir, pool) = test_pool();
        enqueue(&pool, QUEUE_PROJECT_STUDIO, "waiting", "{\"p\":1}").expect("enqueue");
        assert_eq!(
            request_cancel(&pool, "waiting").expect("cancel"),
            CancelOutcome::Dequeued("{\"p\":1}".to_string())
        );
        assert!(claim(&pool, QUEUE_PROJECT_STUDIO).expect("claim").is_none());

        enqueue(&pool, QUEUE_PROJECT_STUDIO, "live", "{}").expect("enqueue");
        claim(&pool, QUEUE_PROJECT_STUDIO).expect("claim").expect("job");
        assert_eq!(
            request_cancel(&pool, "live").expect("cancel"),
            CancelOutcome::Signalled
        );
        assert_eq!(
            heartbeat(&pool, "live").expect("beat"),
            JobLiveness::CancelRequested
        );
        finish(&pool, "live").expect("finish");
        assert_eq!(
            request_cancel(&pool, "live").expect("cancel"),
            CancelOutcome::Unknown
        );
        assert_eq!(heartbeat(&pool, "live").expect("beat"), JobLiveness::Gone);
    }

    /// The queue is runtime state of a single node and cannot be swept into the
    /// Sync Ledger. This pins the half a test can pin: no sync descriptor names
    /// these tables, so a later edit that adds one fails here. The other half —
    /// the ledger reading only out of the MAIN pool, which this file is not —
    /// is a property of `sync::ledger`, not something asserting on table names
    /// can show.
    #[test]
    fn ingest_jobs_stays_out_of_the_sync_ledger() {
        use crate::sync::core_registry::{descriptor_for_table, is_core_sync_table};
        for table in ["ingest_jobs", "ingest_jobs_schema_version"] {
            assert!(
                !is_core_sync_table(table),
                "{table} must stay out of core sync"
            );
            assert!(descriptor_for_table(table).is_none());
        }
    }
}
