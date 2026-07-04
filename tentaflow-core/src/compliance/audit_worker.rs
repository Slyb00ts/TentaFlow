// =============================================================================
// Plik: compliance/audit_worker.rs — off-hot-path persistence for AI audit
// =============================================================================
// The compliance audit writes (event row + prompt payload at start, response
// payload + tool calls + audit_log + token bump at finish) are SQLite writes
// under the global writer mutex. Running them inline on the request path adds
// ~2 ms per call and serialises concurrent requests on that mutex. This worker
// moves the writes onto ONE dedicated OS thread so the request path only mints
// the in-memory ids and hands off owned data.
//
// Ordering invariant: a single consumer drains jobs in submit order, and the
// request flow always submits an event's `start` job before its `finish` job,
// so the finish INSERT (which FK-references the event row) never runs before
// the start INSERT. `submit` therefore BLOCKS when the queue is full instead of
// running inline — an inline finish could otherwise overtake a still-queued
// start of the same event. If the worker thread is gone (channel closed) the
// job runs inline as a last resort.
//
// Default mode is ASYNC. Forcing SYNC (env `TENTAFLOW_AI_AUDIT_SYNC=1`, or
// `set_audit_async(false)`) restores the old inline behaviour, which keeps the
// GDPR guarantee that a prompt is persisted BEFORE dispatch even across a crash.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;

/// One deferred audit write. Owns everything it touches (DbPool is an Arc).
pub type AuditJob = Box<dyn FnOnce() + Send + 'static>;

/// Async is the default. Flipped to false only when the operator opts into
/// synchronous (compliance-strict) auditing.
static ASYNC_ENABLED: AtomicBool = AtomicBool::new(true);

/// Bounded queue to the worker thread. `None` until `init_audit_worker` runs;
/// callers then fall back to running jobs inline (tests / DB-less bootstraps).
static SENDER: OnceLock<SyncSender<AuditJob>> = OnceLock::new();

/// Bound picked so a burst backs pressure onto request threads long before it
/// can grow unbounded memory. One worker sustains far more than the GPU's
/// request rate, so this is only ever hit under pathological overload.
const QUEUE_CAPACITY: usize = 8192;

/// Set the audit mode. `true` = async (default), `false` = synchronous inline.
pub fn set_audit_async(enabled: bool) {
    ASYNC_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether audit writes are deferred to the worker.
pub fn audit_async_enabled() -> bool {
    ASYNC_ENABLED.load(Ordering::Relaxed)
}

/// Spawn the single worker thread. Idempotent — a second call is a no-op.
/// Safe to call unconditionally at startup even when sync mode is selected;
/// the worker simply never receives jobs then.
pub fn init_audit_worker() {
    if SENDER.get().is_some() {
        return;
    }
    let (tx, rx) = sync_channel::<AuditJob>(QUEUE_CAPACITY);
    // Ignore the race where two callers init at once — the loser drops its tx,
    // the receiver for that channel is dropped with it, and the winner's worker
    // is the live one.
    if SENDER.set(tx).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("ai-audit-worker".to_string())
        .spawn(move || {
            // Runs each write to completion in submit order. A panic in one job
            // must not kill the worker (that would silently drop all later
            // audits), so we isolate it.
            while let Ok(job) = rx.recv() {
                if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
                    tracing::error!(?panic, "ai-audit-worker: job panicked (audit row dropped)");
                }
            }
            tracing::info!("ai-audit-worker: channel closed, worker exiting");
        })
        .expect("spawn ai-audit-worker thread");
}

/// Run an audit write. In async mode the job is queued to the worker (blocking
/// when the queue is full, so start→finish order is preserved); in sync mode,
/// or when no worker is wired, it runs inline on the caller. Returns whether it
/// was deferred, so the caller can decide error handling (inline jobs surface
/// their own Result; deferred jobs log inside the closure).
pub fn submit(job: AuditJob) {
    if audit_async_enabled() {
        if let Some(tx) = SENDER.get() {
            // Blocking send: preserves ordering under backpressure. A dead
            // worker (disconnected) returns the job in SendError → run inline.
            match tx.send(job) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(returned)) => {
                    tracing::warn!("ai-audit-worker: channel disconnected, running audit inline");
                    returned();
                    return;
                }
            }
        }
    }
    job();
}
