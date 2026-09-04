// ===== File: tentaquant/runs.rs — T1 execution, its stream and its bookkeeping =====
//
// One run on tier T1 is one `runs` row plus one blocking task on the node that
// received the request (plan §4.1). The row exists from the moment the request
// is accepted — `created → queued → running → { succeeded | failed |
// cancelled }` — so a run is never invisible while it waits for a slot.
//
// Four things live here, and each exists because the obvious implementation
// gets it wrong:
//
//   * a CONCURRENCY GUARD per laboratory. A state vector is the biggest
//     allocation this process makes; letting every request start one turns a
//     lecture room clicking "run" into an out-of-memory kill. Requests past
//     the limit queue in `queued` rather than being refused.
//   * a CANCELLATION token and a WALL CLOCK per run, asked together as one
//     `Limit`. Every loop whose length grows with the request answers to it:
//     the recorded evolution, which Core steps itself because it publishes a
//     frame per gate, and every loop inside the crate — shots AND gates — which
//     take the limit as a `Cancel` hook. Both dimensions matter: the qubit
//     ceiling of plan §4.2 bounds the cost of ONE pass over the state, while
//     the parser accepts programs of up to a million operations, so an
//     unhooked gate loop would hold a cancelled run for the whole simulation.
//     The clock is the laboratory's `cell_timeout_secs`: without it a run
//     nobody cancels holds an execution slot forever, and two of those wedge
//     the laboratory. Cancelling while queued costs nothing at all — no state
//     vector has been allocated.
//   * a STREAM with a monotonic `seq` and a bounded replay buffer (plan
//     §11.2), so a dashboard that lost its connection resumes with
//     `after_seq` instead of losing the evolution. Frames are published from
//     the blocking thread, which is why the buffer is a plain mutex and the
//     wakeup is a `Notify`.
//   * the LIVE-RUN REGISTRY, the ML Studio marker (§3.2). The supervising
//     task lives in the process that started it: after a restart nobody would
//     ever close the row. "Not terminal, on this node, and no marker in THIS
//     process" is the exact condition for an orphan — no time heuristics.

use std::cmp::Ordering;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use num_complex::Complex64;
use parking_lot::Mutex;
use serde::{Serialize, Serializer};
use tentaflow_protocol::tentaquant::{
    LabSettings, RunArtifactInfo, RunEvent, RunMetrics, SimulateOptions, StateKeyframe,
    RUN_EVENT_DONE, RUN_EVENT_METRICS, RUN_EVENT_OUTPUT, RUN_EVENT_STATE_KEYFRAME,
    RUN_STREAM_REPLAY_FRAMES,
};
use tentaflow_quantum::ir::OpKind;
use tentaflow_quantum::sim::statevector::{self, Simulator};
use tentaflow_quantum::sim::{stabilizer, Cancel, Precision};
use tentaflow_quantum::{Circuit, Error as QuantumError};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use super::{cas, circuit, db, keyframes};
use crate::db::DbPool;

/// Mime types of the outputs a T1 run produces (plan §4.3). They are the same
/// strings the kernel tier will emit through `IPython.display`, so a notebook
/// cell renders identically whichever tier filled it.
pub const MIME_COUNTS: &str = "application/x-tentaquant-counts+json";
pub const MIME_STATE: &str = "application/x-tentaquant-state+json";
pub const MIME_PROBS: &str = "application/x-tentaquant-probs+json";
pub const MIME_KEYFRAMES: &str = "application/x-tentaquant-keyframes+cbor";

/// Outputs at most this large travel inside the mime bundle; anything bigger
/// goes to the content store and the bundle carries the reference (plan §4.3).
const INLINE_OUTPUT_BYTES: usize = 64 * 1024;

/// Hard ceiling on a stored state artifact (§18 decision 9), measured on what
/// is actually WRITTEN — the JSON of `[re, im]` pairs, not the 16 bytes per
/// amplitude the vector occupies in memory. A run over it keeps its counts and
/// its keyframes and says the state was not stored, so the laboratory's
/// directory never receives a gigabyte nobody asked for.
pub const MAX_STATE_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

/// Message a run orphaned by a Core restart is closed with.
const ORPHAN_ERROR: &str = "run interrupted by a TentaFlow restart — start it again";

/// Reason a stream ends with; the consumer forwards these to the browser.
pub const END_COMPLETED: &str = "completed";
pub const END_CANCELLED: &str = "cancelled";
pub const END_GAP: &str = "gap";
pub const END_NOT_FOUND: &str = "not_found";

/// How long a finished run's frames stay replayable in memory. After that the
/// stream is dropped and a subscriber is answered from the row and the stored
/// artifacts instead.
const STREAM_RETENTION: Duration = Duration::from_secs(10 * 60);

// =============================================================================
// Live runs, cancellation and the per-laboratory slot count
// =============================================================================

fn live_runs() -> &'static Mutex<HashSet<String>> {
    static LIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn cancel_tokens() -> &'static DashMap<String, CancellationToken> {
    static TOKENS: OnceLock<DashMap<String, CancellationToken>> = OnceLock::new();
    TOKENS.get_or_init(DashMap::new)
}

/// Arms a run: the supervision marker and the cancellation token, in one act.
///
/// The CALLER arms it before spawning the supervising task, and that ordering
/// is load-bearing twice over. A spawned task does not run until the runtime
/// polls it, and in that window the row exists in `created` with nobody
/// watching: a concurrent `RunGet` would read it as orphaned and close it, and
/// a concurrent cancel would find no token and force the row shut. Arming
/// first makes both windows empty.
pub fn arm(run_id: &str) -> CancellationToken {
    let token = CancellationToken::new();
    live_runs().lock().insert(run_id.to_string());
    cancel_tokens().insert(run_id.to_string(), token.clone());
    token
}

/// Drops the marker and the token after the supervising task ends, whatever
/// the outcome.
pub fn disarm(run_id: &str) {
    live_runs().lock().remove(run_id);
    cancel_tokens().remove(run_id);
}

pub fn is_local_run_live(run_id: &str) -> bool {
    live_runs().lock().contains(run_id)
}

/// Asks a live run to stop. `false` means this process is not running it —
/// the caller then decides whether the row is finished or orphaned.
pub fn request_cancel(run_id: &str) -> bool {
    match cancel_tokens().get(run_id) {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

struct Slots {
    semaphore: Arc<Semaphore>,
    /// Permits the semaphore accounts for right now: the ones runs hold plus
    /// the ones waiting to be taken.
    issued: usize,
    /// Permits still owed for removal. A shrink cannot take a permit away from
    /// a run already using it, so the part it could not take now is remembered
    /// and collected as runs finish. Remembering it — rather than parking a
    /// task on the acquisitions — is what keeps a shrink, a grow and a second
    /// shrink from each spending permits the others counted on.
    debt: usize,
}

fn slot_map() -> &'static DashMap<String, Slots> {
    static SLOTS: OnceLock<DashMap<String, Slots>> = OnceLock::new();
    SLOTS.get_or_init(DashMap::new)
}

/// The laboratory's execution slots, resized to the current setting.
///
/// The effective ceiling is `issued - debt`, and every resize restores it to
/// the setting: growing cancels outstanding debt before minting new permits,
/// shrinking takes what is free and books the rest as debt.
fn slots(instance_id: &str, limit: u32) -> Arc<Semaphore> {
    let limit = limit.max(1) as usize;
    let mut entry = slot_map()
        .entry(instance_id.to_string())
        .or_insert_with(|| Slots {
            semaphore: Arc::new(Semaphore::new(limit)),
            issued: limit,
            debt: 0,
        });
    let effective = entry.issued - entry.debt;
    match effective.cmp(&limit) {
        Ordering::Less => {
            let mut missing = limit - effective;
            // Debt first: a shrink that never completed must not later take
            // permits from the grow that replaced it, which is exactly how a
            // laboratory ends up running fewer slots than its setting says.
            let cancelled = missing.min(entry.debt);
            entry.debt -= cancelled;
            missing -= cancelled;
            if missing > 0 {
                entry.semaphore.add_permits(missing);
                entry.issued += missing;
            }
        }
        Ordering::Greater => {
            let surplus = effective - limit;
            let taken = entry.semaphore.forget_permits(surplus);
            entry.issued -= taken;
            entry.debt += surplus - taken;
        }
        Ordering::Equal => {}
    }
    entry.semaphore.clone()
}

/// Hands a finished run's slot back and settles what a lowered limit could not
/// take while the run held it.
fn release_slot(instance_id: &str, permit: OwnedSemaphorePermit) {
    drop(permit);
    if let Some(mut entry) = slot_map().get_mut(instance_id) {
        if entry.debt > 0 {
            let taken = entry.semaphore.forget_permits(entry.debt);
            entry.debt -= taken;
            entry.issued -= taken;
        }
    }
}

// =============================================================================
// The run stream (plan §11.2)
// =============================================================================

struct StreamState {
    frames: VecDeque<RunEvent>,
    next_seq: u64,
    closed: Option<String>,
    closed_at: Option<Instant>,
}

/// One run's frame buffer. Public because a subscriber holds it across polls;
/// its state stays private, so frames can only be added through `publish`.
pub struct RunStream {
    state: Mutex<StreamState>,
    notify: Notify,
}

fn stream_map() -> &'static DashMap<String, Arc<RunStream>> {
    static STREAMS: OnceLock<DashMap<String, Arc<RunStream>>> = OnceLock::new();
    STREAMS.get_or_init(DashMap::new)
}

/// Drops the streams of runs that finished long enough ago. Called from every
/// path that creates or reads a stream — a laboratory whose last run ended
/// would otherwise hold its 512-frame buffer until some future run started —
/// so no task is needed to reclaim the buffers. The scan is over one entry per
/// recent run, which is why a subscriber can afford it on each poll.
fn sweep_streams() {
    let stale: Vec<String> = stream_map()
        .iter()
        .filter(|entry| {
            entry
                .value()
                .state
                .lock()
                .closed_at
                .is_some_and(|at| at.elapsed() > STREAM_RETENTION)
        })
        .map(|entry| entry.key().clone())
        .collect();
    for run_id in stale {
        stream_map().remove(&run_id);
    }
}

/// Opens the stream of a run before its first frame is published.
pub fn open_stream(run_id: &str) {
    sweep_streams();
    stream_map().insert(
        run_id.to_string(),
        Arc::new(RunStream {
            state: Mutex::new(StreamState {
                frames: VecDeque::new(),
                next_seq: 1,
                closed: None,
                closed_at: None,
            }),
            notify: Notify::new(),
        }),
    );
}

/// Appends one frame and wakes every subscriber. Callable from the blocking
/// executor thread: the buffer is a plain mutex and `notify_waiters` needs no
/// runtime context.
fn publish(run_id: &str, mut event: RunEvent) {
    let Some(stream) = stream_map().get(run_id).map(|e| e.value().clone()) else {
        return;
    };
    {
        let mut state = stream.state.lock();
        if state.closed.is_some() {
            return;
        }
        event.seq = state.next_seq;
        state.next_seq += 1;
        state.frames.push_back(event);
        while state.frames.len() > RUN_STREAM_REPLAY_FRAMES {
            state.frames.pop_front();
        }
    }
    stream.notify.notify_waiters();
}

fn event(kind: &str) -> RunEvent {
    RunEvent {
        seq: 0,
        kind: kind.to_string(),
        output: None,
        keyframe: None,
        metrics: None,
        run: None,
    }
}

pub fn publish_output(run_id: &str, output: RunArtifactInfo) {
    publish(
        run_id,
        RunEvent {
            output: Some(output),
            ..event(RUN_EVENT_OUTPUT)
        },
    );
}

pub fn publish_keyframe(run_id: &str, keyframe: StateKeyframe) {
    publish(
        run_id,
        RunEvent {
            keyframe: Some(keyframe),
            ..event(RUN_EVENT_STATE_KEYFRAME)
        },
    );
}

pub fn publish_metrics(run_id: &str, metrics: RunMetrics) {
    publish(
        run_id,
        RunEvent {
            metrics: Some(metrics),
            ..event(RUN_EVENT_METRICS)
        },
    );
}

/// Publishes the terminal frame and closes the stream with a reason.
///
/// `run` is the row the subscriber ends on. It is `None` only when there is no
/// row left to send — the run's project was deleted while it executed, and the
/// cascade took the row with it. The stream is closed all the same: an open
/// stream is never swept, so its replay buffer would stay in memory for the
/// life of the process.
pub fn close_stream(
    run_id: &str,
    run: Option<tentaflow_protocol::tentaquant::RunInfo>,
    reason: &str,
) {
    publish(
        run_id,
        RunEvent {
            run,
            ..event(RUN_EVENT_DONE)
        },
    );
    if let Some(stream) = stream_map().get(run_id).map(|e| e.value().clone()) {
        {
            let mut state = stream.state.lock();
            state.closed = Some(reason.to_string());
            state.closed_at = Some(Instant::now());
        }
        stream.notify.notify_waiters();
    }
}

/// What a subscriber gets for one poll.
pub enum StreamRead {
    /// Frames after the cursor, and the close reason once the run ended.
    Frames {
        frames: Vec<RunEvent>,
        closed: Option<String>,
    },
    /// The cursor is older than the replay buffer: the timeline has a hole in
    /// it, and saying so is the only honest answer (plan §11.2).
    Gap,
    /// No stream for this run in this process — it finished long ago, or it
    /// never ran here.
    Unknown,
}

pub fn read_stream(run_id: &str, after_seq: u64) -> StreamRead {
    sweep_streams();
    let Some(stream) = stream_map().get(run_id).map(|e| e.value().clone()) else {
        return StreamRead::Unknown;
    };
    let state = stream.state.lock();
    if let Some(oldest) = state.frames.front().map(|f| f.seq) {
        // Saturating: the cursor comes from the network and must not wrap.
        if after_seq.saturating_add(1) < oldest {
            return StreamRead::Gap;
        }
    }
    StreamRead::Frames {
        frames: state
            .frames
            .iter()
            .filter(|f| f.seq > after_seq)
            .cloned()
            .collect(),
        closed: state.closed.clone(),
    }
}

/// Wakeup handle a subscriber parks on between polls.
pub fn stream_handle(run_id: &str) -> Option<Arc<RunStream>> {
    sweep_streams();
    stream_map().get(run_id).map(|e| e.value().clone())
}

impl RunStream {
    /// Parks until a frame is published, at most `timeout`.
    ///
    /// The bound is not a poll loop: `notify_waiters` stores no permit, so a
    /// frame published between a subscriber's read and its await would leave
    /// it parked until the NEXT frame. The timeout closes that window, and in
    /// the common case the notification is what wakes it.
    pub async fn wait_for_change(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.notify.notified()).await;
    }
}

/// How long a subscriber parks before it re-reads regardless.
pub const STREAM_POLL: Duration = Duration::from_millis(250);

// =============================================================================
// Orphan reconciliation (plan §3.2)
// =============================================================================

/// Closes a run this node left open across a restart. The condition is exact:
/// not terminal, placed on THIS node, and no supervision marker in this
/// process. Returns whether the row was closed.
pub fn reconcile_orphan_local_run(
    pool: &DbPool,
    run: &mut db::RunRecord,
    local_node_id: &str,
) -> bool {
    if db::is_terminal_status(&run.status) {
        return false;
    }
    if run.node_id.as_deref() != Some(local_node_id) {
        return false;
    }
    if is_local_run_live(&run.id) {
        return false;
    }
    if db::finish_run(pool, &run.id, "failed", Some(ORPHAN_ERROR), None).unwrap_or(false) {
        run.status = "failed".to_string();
        run.error = Some(ORPHAN_ERROR.to_string());
        return true;
    }
    false
}

// =============================================================================
// Execution
// =============================================================================

/// Everything one T1 run needs, gathered before the task is spawned so the
/// executor touches no request state.
pub struct Job {
    pub instance_id: String,
    pub run_id: String,
    pub cell_id: String,
    pub circuit: Circuit,
    pub options: SimulateOptions,
    pub settings: LabSettings,
    pub data_dir: PathBuf,
    pub pool: DbPool,
}

/// Outcome of the blocking phase.
enum Outcome {
    Finished(RunMetrics),
    Cancelled,
    Failed(String),
}

/// Why the executor left a loop before the work was done.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stop {
    Cancelled,
    /// The laboratory's `cell_timeout_secs` elapsed. It is a FAILURE and not a
    /// cancellation: nobody asked for it, and the run view has to be able to
    /// say that the limit — not a person — ended the run.
    TimedOut,
}

/// The cancel token and the wall clock of one run, checked together.
struct Limit<'a> {
    cancel: &'a CancellationToken,
    deadline: Instant,
    timeout_secs: u32,
}

impl Limit<'_> {
    fn stopped(&self) -> Option<Stop> {
        if self.cancel.is_cancelled() {
            return Some(Stop::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Some(Stop::TimedOut);
        }
        None
    }

    fn outcome(&self, stop: Stop) -> Outcome {
        match stop {
            Stop::Cancelled => Outcome::Cancelled,
            Stop::TimedOut => Outcome::Failed(format!(
                "run exceeded this laboratory's time limit of {} s",
                self.timeout_secs
            )),
        }
    }
}

/// The state vector as the artifact stores it: `{"numQubits": n, "amplitudes":
/// [[re, im], ...]}`. Serialized straight from the amplitude slice — building a
/// `serde_json::Value` first would hold the whole state a second time, as a
/// tree of two-element arrays, before the string is even produced.
struct StatePayload<'a> {
    num_qubits: u32,
    amplitudes: &'a [Complex64],
}

impl Serialize for StatePayload<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("state", 2)?;
        state.serialize_field("numQubits", &self.num_qubits)?;
        state.serialize_field("amplitudes", &Amplitudes(self.amplitudes))?;
        state.end()
    }
}

struct Amplitudes<'a>(&'a [Complex64]);

impl Serialize for Amplitudes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(|a| [a.re, a.im]))
    }
}

/// The measurement distribution of the same state, in the same shape.
struct ProbabilityPayload<'a> {
    num_qubits: u32,
    amplitudes: &'a [Complex64],
}

impl Serialize for ProbabilityPayload<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("probabilities", 2)?;
        state.serialize_field("numQubits", &self.num_qubits)?;
        state.serialize_field("probabilities", &Probabilities(self.amplitudes))?;
        state.end()
    }
}

struct Probabilities<'a>(&'a [Complex64]);

impl Serialize for Probabilities<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(|a| a.norm_sqr()))
    }
}

/// Runs the job to completion on the calling (blocking) thread, publishing
/// frames and storing outputs as they are produced.
fn execute(job: &Job, cancel: &CancellationToken) -> Outcome {
    let started = Instant::now();
    let timeout_secs = job.settings.cell_timeout_secs.max(1);
    let limit = Limit {
        cancel,
        deadline: started + Duration::from_secs(timeout_secs as u64),
        timeout_secs,
    };
    let num_qubits = job.circuit.num_qubits() as u32;
    let options = circuit::sim_options(&job.options, job.settings.max_qubits_core);
    let clifford = job.circuit.is_clifford();
    let wants_amplitudes = job.options.want_state || job.options.want_probabilities;
    let steps = job.circuit.ops().len();
    let mut wants_evolution = circuit::records_evolution(&job.options, num_qubits, clifford);
    // The keyframe budgets are allocation sizes and they come from the wire, so
    // they are checked before anything is allocated. What happens next depends
    // on WHO asked: an explicit "record evolution" that cannot fit is a
    // refusal (the dispatch handler already refuses it before the row exists,
    // and this is the same answer for every other caller), while the size rule
    // of §13.6 recording it by default must never turn a runnable circuit into
    // a run that cannot start — it drops the recording and says why.
    let mut evolution_note = None;
    if wants_evolution {
        if let Err(reason) = circuit::validate_keyframe_budget(num_qubits, steps, &job.options) {
            if job.options.record_evolution == Some(true) {
                return Outcome::Failed(reason);
            }
            wants_evolution = false;
            evolution_note = Some(reason);
        }
    }

    // `auto` follows the browser tier: the tableau is chosen only when it can
    // answer everything that was asked for, because it has no amplitudes.
    let stabilizer_method = match job.options.method.as_str() {
        "statevector" => false,
        "stabilizer" => {
            if !clifford {
                return Outcome::Failed(
                    "the circuit is not Clifford, so the stabilizer method cannot run it"
                        .to_string(),
                );
            }
            if wants_amplitudes || wants_evolution {
                return Outcome::Failed(
                    "the stabilizer tableau has no amplitudes; ask for the statevector method"
                        .to_string(),
                );
            }
            true
        }
        _ => clifford && !wants_amplitudes && !wants_evolution,
    };

    let mut metrics = RunMetrics {
        duration_ms: 0,
        qubits: num_qubits,
        clbits: job.circuit.num_clbits() as u32,
        shots: 0,
        // The peak, not one copy of it: a run that stores its state reads the
        // amplitudes back into a fresh `Complex64` vector that lives beside
        // the simulator's own, and a run view that reported half of that would
        // under-report what the node actually spent.
        memory_bytes: if stabilizer_method {
            0
        } else {
            circuit::state_bytes(num_qubits, options.precision)
                + if wants_amplitudes {
                    circuit::read_back_bytes(num_qubits)
                } else {
                    0
                }
        },
        gates: job
            .circuit
            .ops()
            .iter()
            .filter(|op| matches!(op.kind, OpKind::Gate { .. }))
            .count() as u32,
        keyframes: 0,
        method: if stabilizer_method {
            "stabilizer".to_string()
        } else {
            "statevector".to_string()
        },
        precision: if options.precision == Precision::Single {
            "single".to_string()
        } else {
            "double".to_string()
        },
        evolution_note,
        backend: String::new(),
        state_note: None,
    };
    let mut seq: u32 = 0;

    // Phase 1 — the evolution. The only pass that can produce keyframes: a
    // measurement collapses the register, so a stepped run and a sampled
    // histogram cannot share one pass. It is stepped here rather than inside
    // the crate because a frame is published per gate and the limits are
    // checked per gate.
    let mut recorded: Vec<StateKeyframe> = Vec::new();
    let mut final_amplitudes = None;
    if !stabilizer_method && wants_evolution {
        let mut simulator = match Simulator::new(&job.circuit, &options) {
            Ok(simulator) => simulator,
            Err(error) => return Outcome::Failed(error.to_string()),
        };
        metrics.backend = simulator.backend_name().to_string();
        let budget = keyframes::options(
            job.options.keyframe_top_k,
            job.options.keyframe_probs_top,
            &job.options.keyframe_pairs,
        );
        while simulator.step() {
            if let Some(stop) = limit.stopped() {
                return limit.outcome(stop);
            }
            match simulator.keyframe(&budget) {
                Ok(frame) => {
                    let wire = keyframes::to_wire(&frame);
                    publish_keyframe(&job.run_id, wire.clone());
                    recorded.push(wire);
                }
                Err(error) => return Outcome::Failed(error.to_string()),
            }
        }
        metrics.keyframes = recorded.len() as u32;
        // The stepped pass ends on the final state, so phase 3 can keep it
        // instead of simulating the circuit a second time — but ONLY when the
        // state was asked for. A full read-back is 2^n amplitudes next to the
        // simulator's own copy of them, which is the allocation the qubit
        // ceiling and the slot count exist to bound.
        if wants_amplitudes && statevector::require_unitary(&job.circuit).is_ok() {
            final_amplitudes = Some(simulator.amplitudes());
        }
        match store_keyframes(job, &recorded) {
            Ok(Some(output)) => {
                if let Err(error) = persist_output(job, &mut seq, output) {
                    return Outcome::Failed(error.to_string());
                }
            }
            Ok(None) => {}
            Err(error) => return Outcome::Failed(error.to_string()),
        }
    }

    if let Some(stop) = limit.stopped() {
        return limit.outcome(stop);
    }

    // Phase 2 — the histogram. A circuit with no classical bits has nothing to
    // sample, which is a property of the circuit and not a failure: its answer
    // is the state below.
    if job.options.shots > 0 && job.circuit.num_clbits() > 0 {
        // The shot loop is the crate's, and so is the decision of which one to
        // run: a circuit whose final state depends on an outcome is replayed
        // once per shot, everything else is sampled from one pass over the
        // state. What Core adds is the ability to END it — the `Cancel` hook is
        // asked as the shots are consumed, so a run stops within one replay of
        // the ask instead of at the histogram.
        let stop_asked = || limit.stopped().is_some();
        let hook = Cancel::new(&stop_asked);
        let sampled = if stabilizer_method {
            stabilizer::run(&job.circuit, &options, job.options.shots, hook)
        } else {
            statevector::run(&job.circuit, &options, job.options.shots, hook)
        };
        let result = match sampled {
            Ok(result) => result,
            // The crate reports only THAT the hook stopped it; which of the two
            // limits fired is Core's bookkeeping, and a race between them ends
            // as a cancellation because that is the one a person asked for.
            Err(QuantumError::Cancelled) => {
                return limit.outcome(limit.stopped().unwrap_or(Stop::Cancelled))
            }
            Err(error) => return Outcome::Failed(error.to_string()),
        };
        metrics.shots = result.shots;
        if metrics.backend.is_empty() {
            metrics.backend = if stabilizer_method {
                "stabilizer".to_string()
            } else {
                "cpu".to_string()
            };
        }
        let payload = serde_json::json!({
            "counts": result.counts,
            "shots": result.shots,
            "numQubits": num_qubits,
            "numClbits": metrics.clbits,
        });
        let output = match build_output(job, MIME_COUNTS, &payload) {
            Ok(output) => output,
            Err(error) => return Outcome::Failed(error.to_string()),
        };
        if let Err(error) = persist_output(job, &mut seq, output) {
            return Outcome::Failed(error.to_string());
        }
    }

    if let Some(stop) = limit.stopped() {
        return limit.outcome(stop);
    }

    // Phase 3 — the state vector, when the circuit has one and it fits the
    // store. Above the ceiling the run keeps everything else and says so.
    if wants_amplitudes && !stabilizer_method {
        if let Err(reason) = statevector::require_unitary(&job.circuit) {
            metrics.state_note = Some(reason.to_string());
        } else if circuit::state_json_bytes(num_qubits) > MAX_STATE_ARTIFACT_BYTES {
            metrics.state_note = Some(format!(
                "the state vector of {num_qubits} qubits serializes to about {} bytes, over the \
                 {MAX_STATE_ARTIFACT_BYTES} byte artifact limit, so only the counts and the \
                 evolution were stored",
                circuit::state_json_bytes(num_qubits)
            ));
        } else {
            let amplitudes = match final_amplitudes {
                Some(amplitudes) => amplitudes,
                None => {
                    // The same hook as the histogram, for the same reason: this
                    // is one pass over the state PER GATE, so without it a
                    // cancelled run would still simulate the whole program.
                    let stop_asked = || limit.stopped().is_some();
                    let simulated =
                        statevector::statevector(&job.circuit, &options, Cancel::new(&stop_asked));
                    match simulated {
                        Ok(amplitudes) => amplitudes,
                        Err(QuantumError::Cancelled) => {
                            return limit.outcome(limit.stopped().unwrap_or(Stop::Cancelled))
                        }
                        Err(error) => return Outcome::Failed(error.to_string()),
                    }
                }
            };
            if metrics.backend.is_empty() {
                metrics.backend = "cpu".to_string();
            }
            if job.options.want_state {
                let payload = StatePayload {
                    num_qubits,
                    amplitudes: &amplitudes,
                };
                let output = match build_output(job, MIME_STATE, &payload) {
                    Ok(output) => output,
                    Err(error) => return Outcome::Failed(error.to_string()),
                };
                if let Err(error) = persist_output(job, &mut seq, output) {
                    return Outcome::Failed(error.to_string());
                }
            }
            if job.options.want_probabilities {
                let payload = ProbabilityPayload {
                    num_qubits,
                    amplitudes: &amplitudes,
                };
                let output = match build_output(job, MIME_PROBS, &payload) {
                    Ok(output) => output,
                    Err(error) => return Outcome::Failed(error.to_string()),
                };
                if let Err(error) = persist_output(job, &mut seq, output) {
                    return Outcome::Failed(error.to_string());
                }
            }
        }
    }

    if metrics.backend.is_empty() {
        metrics.backend = "cpu".to_string();
    }
    metrics.duration_ms = started.elapsed().as_millis() as u64;
    publish_metrics(&job.run_id, metrics.clone());
    Outcome::Finished(metrics)
}

/// Stores the recorded evolution as one CBOR artifact and points the run row
/// at it. `None` when nothing was recorded.
fn store_keyframes(job: &Job, frames: &[StateKeyframe]) -> Result<Option<RunArtifactInfo>> {
    if frames.is_empty() {
        return Ok(None);
    }
    let bytes = keyframes::encode_bundle(frames)?;
    let size_bytes = bytes.len() as u64;
    // The budget check refuses a series this large before the run starts; a
    // bundle that still comes out over the limit means the estimate and the
    // encoder disagree, and storing it anyway is how a laboratory directory
    // fills up unnoticed.
    if size_bytes > circuit::MAX_KEYFRAME_BUNDLE_BYTES {
        return Err(anyhow!(
            "recorded evolution of {size_bytes} bytes exceeds the {} byte limit",
            circuit::MAX_KEYFRAME_BUNDLE_BYTES
        ));
    }
    let sha256 = cas::store_blob(&job.data_dir, &bytes)?;
    db::set_run_keyframes(&job.pool, &job.run_id, &sha256)?;
    Ok(Some(RunArtifactInfo {
        cell_id: job.cell_id.clone(),
        seq: 0,
        mime: MIME_KEYFRAMES.to_string(),
        size_bytes,
        sha256: Some(sha256),
        inline_json: None,
    }))
}

/// One JSON output, inline when it is small and in the content store when it
/// is not (plan §4.3).
fn build_output<T: Serialize>(job: &Job, mime: &str, payload: &T) -> Result<RunArtifactInfo> {
    let json = serde_json::to_string(payload)?;
    let size_bytes = json.len() as u64;
    if json.len() <= INLINE_OUTPUT_BYTES {
        return Ok(RunArtifactInfo {
            cell_id: job.cell_id.clone(),
            seq: 0,
            mime: mime.to_string(),
            size_bytes,
            sha256: None,
            inline_json: Some(json),
        });
    }
    let sha256 = cas::store_blob(&job.data_dir, json.as_bytes())?;
    Ok(RunArtifactInfo {
        cell_id: job.cell_id.clone(),
        seq: 0,
        mime: mime.to_string(),
        size_bytes,
        sha256: Some(sha256),
        inline_json: None,
    })
}

/// Writes one output to `cell_outputs` and streams it. The stored `mime_json`
/// IS a Jupyter mime bundle, so exporting the notebook later is serialization
/// rather than conversion (plan §4.3).
fn persist_output(job: &Job, seq: &mut u32, mut output: RunArtifactInfo) -> Result<()> {
    output.seq = *seq;
    *seq += 1;
    let value = match (&output.inline_json, &output.sha256) {
        (Some(json), _) => serde_json::from_str::<serde_json::Value>(json)
            .map_err(|e| anyhow!("output payload is not JSON: {e}"))?,
        (None, Some(sha256)) => serde_json::json!({
            "sha256": sha256,
            "size_bytes": output.size_bytes,
        }),
        (None, None) => serde_json::Value::Null,
    };
    let bundle = serde_json::json!({ output.mime.clone(): value });
    db::append_cell_output(
        &job.pool,
        &db::CellOutputRecord {
            run_id: job.run_id.clone(),
            cell_id: output.cell_id.clone(),
            seq: output.seq,
            mime_json: serde_json::to_string(&bundle)?,
            artifact_sha256: output.sha256.clone(),
        },
    )?;
    publish_output(&job.run_id, output);
    Ok(())
}

/// Reads one run's outputs back as wire artifacts. Inline payloads come out of
/// the mime bundle; stored ones are named by their hash and fetched with a
/// signed URL.
pub fn artifacts_of(pool: &DbPool, run_id: &str) -> Result<Vec<RunArtifactInfo>> {
    let mut out = Vec::new();
    for record in db::cell_outputs(pool, run_id)? {
        let bundle: serde_json::Value = serde_json::from_str(&record.mime_json)
            .map_err(|e| anyhow!("stored mime bundle is not JSON: {e}"))?;
        let Some((mime, value)) = bundle.as_object().and_then(|m| m.iter().next()) else {
            continue;
        };
        let inline_json = if record.artifact_sha256.is_none() {
            Some(value.to_string())
        } else {
            None
        };
        let size_bytes = match (
            &inline_json,
            value.get("size_bytes").and_then(|v| v.as_u64()),
        ) {
            (Some(json), _) => json.len() as u64,
            (None, Some(size)) => size,
            (None, None) => 0,
        };
        out.push(RunArtifactInfo {
            cell_id: record.cell_id,
            seq: record.seq,
            mime: mime.clone(),
            size_bytes,
            sha256: record.artifact_sha256,
            inline_json,
        });
    }
    Ok(out)
}

/// Reads the stored evolution of a run back from the content store.
pub fn stored_keyframes(data_dir: &Path, sha256: &str) -> Result<Vec<StateKeyframe>> {
    keyframes::decode_bundle(&cas::read_blob(data_dir, sha256)?)
}

/// Supervises one run from `created` to its terminal state: queue for a slot,
/// run the blocking work, close the row and the stream. `cancel` is the token
/// the caller armed with [`arm`] before spawning this.
///
/// The row is closed by whichever of the two ends first — a cancel that lands
/// while the executor is between gates writes `cancelled`, and the executor's
/// own result cannot overwrite it, because `finish_run` only moves a row that
/// is still open.
pub async fn supervise(
    job: Job,
    cancel: CancellationToken,
    on_finished: impl FnOnce(&db::RunRecord) + Send + 'static,
) {
    let run_id = job.run_id.clone();
    let instance_id = job.instance_id.clone();
    let pool = job.pool.clone();
    let limit = job.settings.max_concurrent_core_runs;

    let _ = db::set_run_status(&pool, &run_id, "queued");

    let outcome = {
        let semaphore = slots(&instance_id, limit);
        // Queueing is where a cancel is cheapest: nothing has been allocated,
        // so a run cancelled while it waits for a slot never touches memory.
        // Only the QUEUE wait is raced against the token: once a permit is
        // held, the executor itself carries the cancel — it checks it between
        // gates and between shots, and stopping the blocking thread from out
        // here is not something the runtime can do.
        tokio::select! {
            permit = semaphore.acquire_owned() => match permit {
                Ok(permit) => {
                    let _ = db::set_run_status(&pool, &run_id, "running");
                    let cancel_for_task = cancel.clone();
                    let result =
                        tokio::task::spawn_blocking(move || execute(&job, &cancel_for_task)).await;
                    release_slot(&instance_id, permit);
                    match result {
                        Ok(outcome) => outcome,
                        Err(error) => Outcome::Failed(format!("run task failed: {error}")),
                    }
                }
                Err(error) => Outcome::Failed(format!("execution slots unavailable: {error}")),
            },
            _ = cancel.cancelled() => Outcome::Cancelled,
        }
    };

    let (status, error, metrics_json) = match outcome {
        Outcome::Finished(metrics) => ("succeeded", None, serde_json::to_string(&metrics).ok()),
        Outcome::Cancelled => ("cancelled", Some("cancelled by the user".to_string()), None),
        Outcome::Failed(message) => ("failed", Some(message), None),
    };
    let _ = db::finish_run(
        &pool,
        &run_id,
        status,
        error.as_deref(),
        metrics_json.as_deref(),
    );
    disarm(&run_id);
    match db::run_row(&pool, &run_id) {
        Ok(Some(row)) => on_finished(&row),
        // No row to end on: the project was deleted while the run executed (the
        // cascade takes its runs), or the read failed. The stream still has to
        // be closed here, because `on_finished` is the only other path that
        // ever closes it.
        Ok(None) | Err(_) => close_stream(&run_id, None, END_NOT_FOUND),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::tentaquant::RunInfo;

    const BELL: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\n\
                        h q[0];\ncx q[0], q[1];\nc = measure q;\n";
    /// Two Hadamards and no measurement: a circuit that HAS a state vector, so
    /// the state and probability outputs are defined for it.
    const PLUS: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nh q[0];\nh q[1];\n";
    /// A reset makes the final state depend on the outcomes, so this circuit is
    /// replayed once per shot on both engines — the loop a cancel has to reach.
    /// 28 qubits on the tableau and 16 on the state vector are slow enough per
    /// shot that a million of them cannot finish while the test is watching.
    const WIDE_RESET: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[28] q;\n\
                              bit[28] c;\nh q;\nreset q[0];\nc = measure q;\n";
    const RESET_16: &str = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[16] q;\n\
                            bit[16] c;\nh q;\nreset q[0];\nc = measure q;\n";

    fn pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        // The same pragma the laboratory's database runs with, so a test sees
        // the cascade that deletes a project's runs.
        conn.execute_batch("PRAGMA foreign_keys=ON;").expect("fk");
        db::migrate(&conn).expect("migrate");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn job(
        pool: &DbPool,
        dir: &tempfile::TempDir,
        run_id: &str,
        source: &str,
        options: SimulateOptions,
    ) -> Job {
        let parsed = circuit::parse(source, "").expect("parses");
        Job {
            instance_id: "tentaquant-testtest".to_string(),
            run_id: run_id.to_string(),
            cell_id: run_id.to_string(),
            circuit: parsed.circuit,
            options,
            settings: LabSettings::default(),
            data_dir: dir.path().to_path_buf(),
            pool: pool.clone(),
        }
    }

    fn job_with(
        pool: &DbPool,
        dir: &tempfile::TempDir,
        instance_id: &str,
        run_id: &str,
        source: &str,
        options: SimulateOptions,
        settings: LabSettings,
    ) -> Job {
        Job {
            instance_id: instance_id.to_string(),
            settings,
            ..job(pool, dir, run_id, source, options)
        }
    }

    async fn wait_for_status(pool: &DbPool, run_id: &str, status: &str) {
        for _ in 0..600 {
            if db::run_row(pool, run_id)
                .expect("row")
                .is_some_and(|row| row.status == status)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let actual = db::run_row(pool, run_id).expect("row").map(|r| r.status);
        panic!("{run_id} never reached '{status}' (it is {actual:?})");
    }

    fn insert_run(pool: &DbPool, run_id: &str, node_id: Option<&str>) -> db::RunRecord {
        db::create_run(
            pool,
            &db::NewRun {
                id: run_id.to_string(),
                project_id: None,
                notebook_id: None,
                cell_id: Some(run_id.to_string()),
                kind: "circuit".to_string(),
                target: "core:node-a".to_string(),
                node_id: node_id.map(str::to_string),
                user_id: "anna".to_string(),
            },
        )
        .expect("run row")
    }

    fn counts_of(pool: &DbPool, run_id: &str) -> serde_json::Map<String, serde_json::Value> {
        let artifact = artifacts_of(pool, run_id)
            .expect("artifacts")
            .into_iter()
            .find(|a| a.mime == MIME_COUNTS)
            .expect("counts output");
        let payload: serde_json::Value =
            serde_json::from_str(&artifact.inline_json.expect("inline")).expect("json");
        payload
            .get("counts")
            .and_then(|c| c.as_object())
            .cloned()
            .expect("counts object")
    }

    /// The reproducibility promise of `method.md`: the same circuit, the same
    /// options and the same seed give the same histogram, and a Bell state
    /// only ever produces the two correlated outcomes.
    #[test]
    fn a_bell_run_is_deterministic_for_one_seed() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        let cancel = CancellationToken::new();
        // Explicitly the state vector: the same circuit on the tableau is a
        // different engine, and this test is about the state-vector seed.
        let options = SimulateOptions {
            shots: 1024,
            seed: 42,
            method: "statevector".to_string(),
            ..SimulateOptions::default()
        };

        insert_run(&pool, "run-a", Some("node-a"));
        open_stream("run-a");
        let first = match execute(&job(&pool, &dir, "run-a", BELL, options.clone()), &cancel) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        assert_eq!(first.shots, 1024);
        assert_eq!(first.qubits, 2);
        assert_eq!(first.method, "statevector");
        // 2 qubits × 16 bytes per `complex128` amplitude.
        assert_eq!(first.memory_bytes, 64);

        let counts_a = counts_of(&pool, "run-a");
        assert_eq!(
            counts_a.keys().cloned().collect::<Vec<_>>(),
            vec!["00".to_string(), "11".to_string()],
            "a Bell state has no uncorrelated outcomes"
        );
        assert_eq!(
            counts_a.values().filter_map(|v| v.as_u64()).sum::<u64>(),
            1024
        );

        insert_run(&pool, "run-b", Some("node-a"));
        open_stream("run-b");
        match execute(&job(&pool, &dir, "run-b", BELL, options), &cancel) {
            Outcome::Finished(_) => {}
            other => panic!("run did not finish: {}", outcome_name(&other)),
        }
        assert_eq!(counts_of(&pool, "run-b"), counts_a, "seed 42 is not stable");
    }

    fn outcome_name(outcome: &Outcome) -> String {
        match outcome {
            Outcome::Finished(_) => "finished".to_string(),
            Outcome::Cancelled => "cancelled".to_string(),
            Outcome::Failed(why) => format!("failed: {why}"),
        }
    }

    /// A cancel that lands before the first gate stops the run there: nothing
    /// is sampled and no output is written.
    #[test]
    fn a_cancelled_run_produces_no_output() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-c", Some("node-a"));
        open_stream("run-c");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let options = SimulateOptions {
            shots: 8,
            record_evolution: Some(true),
            ..SimulateOptions::default()
        };
        assert!(matches!(
            execute(&job(&pool, &dir, "run-c", BELL, options), &cancel),
            Outcome::Cancelled
        ));
        assert!(artifacts_of(&pool, "run-c").expect("artifacts").is_empty());
    }

    /// Recording the evolution stores ONE CBOR artifact, points the row at it
    /// and streams one keyframe per gate — the two halves of plan §13.6.
    #[test]
    fn a_recorded_run_stores_and_streams_its_keyframes() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-k", Some("node-a"));
        open_stream("run-k");
        let options = SimulateOptions {
            shots: 16,
            record_evolution: Some(true),
            want_state: true,
            ..SimulateOptions::default()
        };
        let metrics = match execute(
            &job(&pool, &dir, "run-k", PLUS, options),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        assert_eq!(metrics.keyframes, 2);

        let row = db::run_row(&pool, "run-k").expect("row").expect("exists");
        let sha256 = row.keyframes_sha256.expect("keyframes stored");
        let stored = stored_keyframes(dir.path(), &sha256).expect("read back");
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[1].step, 2);

        // The stream carried the same frames, plus the state output and the
        // metrics frame that closes the run.
        let StreamRead::Frames { frames, .. } = read_stream("run-k", 0) else {
            panic!("stream lost its frames");
        };
        let keyframes: Vec<_> = frames.iter().filter_map(|f| f.keyframe.as_ref()).collect();
        assert_eq!(keyframes.len(), 2);
        assert_eq!(keyframes[0], &stored[0]);
        assert!(frames
            .iter()
            .any(|f| f.output.as_ref().is_some_and(|o| o.mime == MIME_STATE)));
        assert!(frames.iter().any(|f| f.metrics.is_some()));
    }

    /// The recorded pass ends on the final state, and Core keeps that state
    /// ONLY when the run asked for it: a read-back is a second full state
    /// vector next to the simulator's own, which at the qubit ceiling is the
    /// largest allocation this tier makes. A run that wanted frames alone must
    /// neither take it nor be billed for it.
    #[test]
    fn a_recorded_run_only_keeps_the_state_it_was_asked_for() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        let options = SimulateOptions {
            shots: 0,
            record_evolution: Some(true),
            method: "statevector".to_string(),
            ..SimulateOptions::default()
        };

        insert_run(&pool, "run-frames", Some("node-a"));
        open_stream("run-frames");
        let frames_only = match execute(
            &job(&pool, &dir, "run-frames", PLUS, options.clone()),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        assert_eq!(frames_only.keyframes, 2);
        assert_eq!(
            frames_only.memory_bytes,
            circuit::state_bytes(2, Precision::Double),
            "frames alone cost one state vector"
        );
        assert!(
            artifacts_of(&pool, "run-frames")
                .expect("artifacts")
                .iter()
                .all(|a| a.mime != MIME_STATE),
            "no state was asked for, so none was stored"
        );

        insert_run(&pool, "run-frames-state", Some("node-a"));
        open_stream("run-frames-state");
        let with_state = match execute(
            &job(
                &pool,
                &dir,
                "run-frames-state",
                PLUS,
                SimulateOptions {
                    want_state: true,
                    ..options
                },
            ),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        assert_eq!(
            with_state.memory_bytes,
            circuit::state_bytes(2, Precision::Double) + circuit::read_back_bytes(2),
            "the stored state is a second copy and the run view says so"
        );
    }

    /// A circuit that has no state vector keeps its counts and SAYS why the
    /// state is missing, instead of leaving an unexplained gap.
    #[test]
    fn a_measured_circuit_reports_why_it_has_no_state() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-m", Some("node-a"));
        open_stream("run-m");
        let options = SimulateOptions {
            shots: 32,
            want_state: true,
            ..SimulateOptions::default()
        };
        let metrics = match execute(
            &job(&pool, &dir, "run-m", BELL, options),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        let note = metrics.state_note.expect("the missing state is explained");
        assert!(note.contains("measurement"), "{note}");
        let artifacts = artifacts_of(&pool, "run-m").expect("artifacts");
        assert!(artifacts.iter().all(|a| a.mime != MIME_STATE));
        assert!(artifacts.iter().any(|a| a.mime == MIME_COUNTS));
    }

    /// A Clifford circuit that needs no amplitudes runs on the tableau, which
    /// allocates no state vector at all.
    #[test]
    fn auto_picks_the_tableau_for_a_clifford_circuit() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-s", Some("node-a"));
        open_stream("run-s");
        let metrics = match execute(
            &job(
                &pool,
                &dir,
                "run-s",
                BELL,
                SimulateOptions {
                    shots: 64,
                    ..SimulateOptions::default()
                },
            ),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        assert_eq!(metrics.method, "stabilizer");
        assert_eq!(metrics.memory_bytes, 0);
    }

    /// Replay: a subscriber resuming at `after_seq` gets exactly the frames it
    /// has not seen, and a cursor older than the buffer is a `gap` rather than
    /// a silently incomplete timeline.
    #[test]
    fn the_stream_replays_after_a_cursor_and_reports_a_gap() {
        open_stream("run-r");
        // Ten frames more than the buffer holds, so seq 1..=10 are evicted and
        // the oldest frame still replayable is 11.
        for index in 0..(RUN_STREAM_REPLAY_FRAMES + 10) {
            publish_metrics(
                "run-r",
                RunMetrics {
                    gates: index as u32,
                    ..RunMetrics::default()
                },
            );
        }
        let StreamRead::Frames { frames, closed } = read_stream("run-r", 10) else {
            panic!("resuming at the oldest retained frame must not be a gap");
        };
        assert_eq!(frames.len(), RUN_STREAM_REPLAY_FRAMES);
        assert_eq!(frames[0].seq, 11);
        assert!(closed.is_none());

        let StreamRead::Frames { frames, .. } = read_stream("run-r", 11) else {
            panic!("resuming inside the buffer must not be a gap");
        };
        assert_eq!(frames.len(), RUN_STREAM_REPLAY_FRAMES - 1);
        assert_eq!(frames[0].seq, 12);

        // A subscriber that was away long enough to lose frame 11 is told the
        // timeline has a hole rather than handed a partial one.
        assert!(matches!(read_stream("run-r", 0), StreamRead::Gap));
        assert!(matches!(read_stream("run-r", 9), StreamRead::Gap));

        close_stream("run-r", Some(run_info_stub()), END_COMPLETED);
        let StreamRead::Frames { closed, .. } = read_stream("run-r", u64::MAX - 1) else {
            panic!("a closed stream still answers");
        };
        assert_eq!(closed.as_deref(), Some(END_COMPLETED));
        assert!(matches!(read_stream("run-missing", 0), StreamRead::Unknown));
    }

    fn run_info_stub() -> RunInfo {
        RunInfo {
            run_id: "run-r".to_string(),
            project_id: None,
            notebook_id: None,
            cell_id: None,
            kind: "circuit".to_string(),
            target: "core:node-a".to_string(),
            node_id: Some("node-a".to_string()),
            status: "succeeded".to_string(),
            started_at: "2026-09-04 10:00:00".to_string(),
            ended_at: None,
            error: None,
            metrics: None,
            user_id: "anna".to_string(),
            user_name: "Anna".to_string(),
            pinned_at: None,
            thumbnail_sha256: None,
            keyframes_sha256: None,
            artifacts: Vec::new(),
        }
    }

    /// The orphan condition of plan §3.2, exactly: not terminal, on THIS node,
    /// and unsupervised in THIS process. Every other row is left alone.
    #[test]
    fn only_an_unsupervised_local_row_is_reconciled() {
        let pool = pool();

        let mut mine = insert_run(&pool, "run-orphan", Some("node-a"));
        db::set_run_status(&pool, "run-orphan", "running").expect("status");
        mine.status = "running".to_string();
        assert!(reconcile_orphan_local_run(&pool, &mut mine, "node-a"));
        assert_eq!(mine.status, "failed");
        assert!(mine.error.as_deref().is_some_and(|e| e.contains("restart")));

        // A run this process supervises is in flight, not orphaned.
        let mut live = insert_run(&pool, "run-live", Some("node-a"));
        db::set_run_status(&pool, "run-live", "running").expect("status");
        live.status = "running".to_string();
        arm("run-live");
        assert!(!reconcile_orphan_local_run(&pool, &mut live, "node-a"));
        disarm("run-live");

        // A run placed on another node is that node's to close.
        let mut remote = insert_run(&pool, "run-remote", Some("node-b"));
        db::set_run_status(&pool, "run-remote", "running").expect("status");
        remote.status = "running".to_string();
        assert!(!reconcile_orphan_local_run(&pool, &mut remote, "node-a"));

        // A finished row is never touched again.
        let mut done = insert_run(&pool, "run-done", Some("node-a"));
        db::finish_run(&pool, "run-done", "succeeded", None, None).expect("finish");
        done.status = "succeeded".to_string();
        assert!(!reconcile_orphan_local_run(&pool, &mut done, "node-a"));
    }

    /// A cancellation that lands after the row closed must not reopen it, and
    /// a second outcome must not overwrite the first.
    #[test]
    fn a_closed_row_keeps_its_first_outcome() {
        let pool = pool();
        insert_run(&pool, "run-once", Some("node-a"));
        assert!(db::finish_run(&pool, "run-once", "cancelled", Some("by the user"), None).unwrap());
        assert!(!db::finish_run(&pool, "run-once", "succeeded", None, None).unwrap());
        let row = db::run_row(&pool, "run-once").unwrap().unwrap();
        assert_eq!(row.status, "cancelled");
    }

    /// A cancel that lands while the tableau is replaying shots stops the run
    /// there. This is the loop the concurrency guard cannot protect anyone
    /// from: a million shots of a 28-qubit Clifford circuit run for minutes, so
    /// a run that ignored the token would hold its slot until the process
    /// restarted.
    #[tokio::test]
    async fn a_cancel_stops_the_shot_loop_in_flight() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-stop", Some("node-a"));
        open_stream("run-stop");
        let cancel = CancellationToken::new();
        let job = job(
            &pool,
            &dir,
            "run-stop",
            WIDE_RESET,
            SimulateOptions {
                shots: 1_000_000,
                ..SimulateOptions::default()
            },
        );
        let token = cancel.clone();
        let running = tokio::task::spawn_blocking(move || execute(&job, &token));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(30), running)
            .await
            .expect("a cancelled run stops instead of running to the end")
            .expect("join");
        assert!(
            matches!(outcome, Outcome::Cancelled),
            "outcome was {}",
            outcome_name(&outcome)
        );
        assert!(artifacts_of(&pool, "run-stop")
            .expect("artifacts")
            .is_empty());
    }

    /// A long chain on a wide register: nothing but two-qubit gates, which are
    /// never fused, so the program has one step per gate and every step is a
    /// full pass over the state. `measured` decides which loop inside the crate
    /// walks it — the sampled histogram or the final state vector.
    fn long_chain(qubits: usize, gates: usize, measured: bool) -> String {
        let mut source = format!("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[{qubits}] q;\n");
        if measured {
            source.push_str(&format!("bit[{qubits}] c;\n"));
        }
        for index in 0..gates {
            source.push_str(&format!(
                "cx q[{}], q[{}];\n",
                index % (qubits - 1),
                qubits - 1
            ));
        }
        if measured {
            source.push_str("c = measure q;\n");
        }
        source
    }

    /// The shot count is not the only unbounded dimension of a run: the parser
    /// accepts a million operations, and the qubit ceiling bounds the cost of
    /// ONE pass over the state, not the number of passes. Neither run below
    /// records its evolution, so Core is not stepping them — the token has to
    /// reach the crate's own gate loops, or a cancelled run keeps a slot until
    /// the whole program is simulated.
    #[tokio::test]
    async fn a_cancel_stops_a_long_gate_loop_and_not_only_the_shot_loop() {
        let cases = [
            // Measured: 8 shots is nothing to tally, so only the gate loop of
            // the sampled histogram can end this.
            (
                "run-gates-counts",
                true,
                SimulateOptions {
                    shots: 8,
                    record_evolution: Some(false),
                    ..SimulateOptions::default()
                },
            ),
            // Unmeasured: no shot loop exists at all, and the state vector this
            // asks for is computed by one uninterruptible-looking call.
            (
                "run-gates-state",
                false,
                SimulateOptions {
                    shots: 0,
                    record_evolution: Some(false),
                    want_state: true,
                    ..SimulateOptions::default()
                },
            ),
        ];
        for (run_id, measured, options) in cases {
            let pool = pool();
            let dir = tempfile::tempdir().expect("dir");
            insert_run(&pool, run_id, Some("node-a"));
            open_stream(run_id);
            let cancel = CancellationToken::new();
            let job = job(
                &pool,
                &dir,
                run_id,
                &long_chain(20, 20_000, measured),
                options,
            );
            let token = cancel.clone();
            let running = tokio::task::spawn_blocking(move || execute(&job, &token));
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
            let outcome = tokio::time::timeout(Duration::from_secs(30), running)
                .await
                .expect("a cancelled run stops at the next gate, not at the next circuit")
                .expect("join");
            assert!(
                matches!(outcome, Outcome::Cancelled),
                "{run_id} ended as {}",
                outcome_name(&outcome)
            );
            assert!(artifacts_of(&pool, run_id).expect("artifacts").is_empty());
        }
    }

    /// The same for the state-vector replay, and through the OTHER limit: a run
    /// nobody cancels ends on the laboratory's `cell_timeout_secs`, as a
    /// failure that names the limit rather than as a slot held forever.
    #[tokio::test]
    async fn a_run_over_the_time_limit_fails_naming_it() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-slow", Some("node-a"));
        open_stream("run-slow");
        let job = job_with(
            &pool,
            &dir,
            "tentaquant-testtest",
            "run-slow",
            RESET_16,
            SimulateOptions {
                shots: 1_000_000,
                method: "statevector".to_string(),
                ..SimulateOptions::default()
            },
            LabSettings {
                cell_timeout_secs: 1,
                ..LabSettings::default()
            },
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::task::spawn_blocking(move || execute(&job, &CancellationToken::new())),
        )
        .await
        .expect("the time limit ends the run")
        .expect("join");
        match outcome {
            Outcome::Failed(message) => assert!(message.contains("time limit"), "{message}"),
            other => panic!("run did not hit the limit: {}", outcome_name(&other)),
        }
    }

    /// The concurrency guard, end to end: with one slot the second run WAITS in
    /// `queued` while the first holds it, and starts the moment it is free.
    #[tokio::test]
    async fn a_run_queues_until_a_slot_frees() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        let settings = LabSettings {
            max_concurrent_core_runs: 1,
            ..LabSettings::default()
        };
        let instance = "tentaquant-queueing";

        insert_run(&pool, "run-first", Some("node-a"));
        open_stream("run-first");
        let first = arm("run-first");
        tokio::spawn(supervise(
            job_with(
                &pool,
                &dir,
                instance,
                "run-first",
                WIDE_RESET,
                SimulateOptions {
                    shots: 1_000_000,
                    ..SimulateOptions::default()
                },
                settings.clone(),
            ),
            first.clone(),
            |_| {},
        ));
        wait_for_status(&pool, "run-first", "running").await;

        insert_run(&pool, "run-second", Some("node-a"));
        open_stream("run-second");
        let second = arm("run-second");
        tokio::spawn(supervise(
            job_with(
                &pool,
                &dir,
                instance,
                "run-second",
                BELL,
                SimulateOptions {
                    shots: 8,
                    ..SimulateOptions::default()
                },
                settings,
            ),
            second,
            |_| {},
        ));

        // The slot is taken, so the second run is visibly waiting rather than
        // competing for the node's memory.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let queued = db::run_row(&pool, "run-second")
            .expect("row")
            .expect("exists");
        assert_eq!(queued.status, "queued");

        first.cancel();
        wait_for_status(&pool, "run-second", "succeeded").await;
        assert_eq!(
            counts_of(&pool, "run-second")
                .values()
                .filter_map(|v| v.as_u64())
                .sum::<u64>(),
            8,
            "the queued run executed once the slot was free"
        );
    }

    /// `gates` is a gate count. Measurements, resets and barriers are steps of
    /// the program — they get a keyframe like everything else — but a run view
    /// that called them gates would be lying about the circuit.
    #[test]
    fn measurements_are_steps_and_not_gates() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        insert_run(&pool, "run-count", Some("node-a"));
        open_stream("run-count");
        let metrics = match execute(
            &job(
                &pool,
                &dir,
                "run-count",
                BELL,
                SimulateOptions {
                    shots: 8,
                    method: "statevector".to_string(),
                    record_evolution: Some(true),
                    ..SimulateOptions::default()
                },
            ),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!("run did not finish: {}", outcome_name(&other)),
        };
        // h and cx are gates; the two measurements of `c = measure q` are not.
        assert_eq!(metrics.gates, 2);
        assert_eq!(metrics.keyframes, 4);
    }

    /// Plan §13.6 both ways: a small circuit records its evolution without
    /// being asked, a large one does not, an explicit choice wins at any size —
    /// and a Clifford circuit that `auto` answers on the tableau is not moved
    /// onto the state vector just to produce frames.
    #[test]
    fn the_evolution_default_follows_the_register_size() {
        let small = SimulateOptions {
            method: "statevector".to_string(),
            ..SimulateOptions::default()
        };
        assert!(circuit::records_evolution(&small, 4, false));
        assert!(!circuit::records_evolution(&small, 26, false));
        assert!(circuit::records_evolution(
            &SimulateOptions {
                record_evolution: Some(true),
                ..small.clone()
            },
            26,
            false
        ));
        assert!(!circuit::records_evolution(
            &SimulateOptions {
                record_evolution: Some(false),
                ..small.clone()
            },
            4,
            false
        ));
        assert!(!circuit::records_evolution(
            &SimulateOptions::default(),
            4,
            true
        ));
    }

    /// The evolution the §13.6 size rule asked for, in a series that does not
    /// fit its budget: the run KEEPS running and says why the animation is
    /// missing. The same budget asked for explicitly is a refusal — a person
    /// who ticked "record evolution" must not be told a run succeeded when the
    /// one thing they asked for was silently dropped.
    #[test]
    fn a_default_evolution_that_cannot_fit_is_dropped_with_a_reason() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        let mut source = String::from("OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[16] q;\n");
        for index in 0..500 {
            source.push_str(&format!("t q[{}];\n", index % 16));
        }
        let options = SimulateOptions {
            shots: 0,
            method: "statevector".to_string(),
            keyframe_top_k: 1024,
            keyframe_probs_top: 256,
            keyframe_pairs: "all".to_string(),
            ..SimulateOptions::default()
        };

        insert_run(&pool, "run-budget", Some("node-a"));
        open_stream("run-budget");
        let metrics = match execute(
            &job(&pool, &dir, "run-budget", &source, options.clone()),
            &CancellationToken::new(),
        ) {
            Outcome::Finished(metrics) => metrics,
            other => panic!(
                "a default that does not fit must not fail the run: {}",
                outcome_name(&other)
            ),
        };
        assert_eq!(metrics.keyframes, 0);
        let note = metrics
            .evolution_note
            .expect("the missing evolution is explained");
        assert!(note.contains("evolution"), "{note}");

        insert_run(&pool, "run-asked", Some("node-a"));
        open_stream("run-asked");
        let outcome = execute(
            &job(
                &pool,
                &dir,
                "run-asked",
                &source,
                SimulateOptions {
                    record_evolution: Some(true),
                    ..options
                },
            ),
            &CancellationToken::new(),
        );
        assert!(
            matches!(outcome, Outcome::Failed(_)),
            "an explicit budget that cannot fit is a refusal, got {}",
            outcome_name(&outcome)
        );
    }

    /// The slot count is a live setting: raising it admits more runs at once,
    /// lowering it applies at once to the slots nobody is using, and neither
    /// resize loses the laboratory's semaphore.
    #[test]
    fn the_slot_count_follows_the_setting() {
        let semaphore = slots("tentaquant-slots", 2);
        assert_eq!(semaphore.available_permits(), 2);
        let grown = slots("tentaquant-slots", 5);
        assert!(Arc::ptr_eq(&semaphore, &grown));
        assert_eq!(grown.available_permits(), 5);

        let shrunk = slots("tentaquant-slots", 1);
        assert_eq!(shrunk.available_permits(), 1);
        slot_map().remove("tentaquant-slots");
    }

    /// A limit lowered while every slot is BUSY cannot take those permits back
    /// yet, and the debt it books must not outlive the setting that created it:
    /// raising the limit again has to cancel it. Losing that made a lab that
    /// went 5 → 1 → 5 keep running one run at a time forever, with the setting
    /// insisting it ran five.
    #[test]
    fn a_shrink_that_could_not_be_paid_is_cancelled_by_the_next_raise() {
        let lab = "tentaquant-resize";
        let semaphore = slots(lab, 5);
        let held: Vec<OwnedSemaphorePermit> = (0..5)
            .map(|_| {
                semaphore
                    .clone()
                    .try_acquire_owned()
                    .expect("the slot is free")
            })
            .collect();

        slots(lab, 1);
        assert_eq!(semaphore.available_permits(), 0, "nothing was free to take");
        slots(lab, 5);
        for permit in held {
            release_slot(lab, permit);
        }
        assert_eq!(
            semaphore.available_permits(),
            5,
            "the raised limit is the one the laboratory runs at"
        );

        // A shrink pays out of the FREE slots first: with one run holding a
        // permit, a ceiling of 2 leaves one slot free right away.
        let busy = semaphore.clone().try_acquire_owned().expect("free");
        slots(lab, 2);
        assert_eq!(semaphore.available_permits(), 1);
        release_slot(lab, busy);
        assert_eq!(semaphore.available_permits(), 2);

        // And a debt that could not be paid at all is collected as the runs
        // end, one returned permit at a time, until the ceiling is reached.
        let both: Vec<OwnedSemaphorePermit> = (0..2)
            .map(|_| semaphore.clone().try_acquire_owned().expect("free"))
            .collect();
        slots(lab, 1);
        let mut returning = both.into_iter();
        release_slot(lab, returning.next().expect("first"));
        assert_eq!(semaphore.available_permits(), 0, "the debt took it back");
        release_slot(lab, returning.next().expect("second"));
        assert_eq!(semaphore.available_permits(), 1);
        slot_map().remove(lab);
    }

    /// A stream whose run row disappears — its project was deleted while it
    /// executed — must still be closed. An open stream is never swept, so its
    /// replay buffer would be held for the life of the process.
    #[tokio::test]
    async fn a_run_whose_row_disappears_still_closes_its_stream() {
        let pool = pool();
        let dir = tempfile::tempdir().expect("dir");
        let project = db::create_project(&pool, "anna", "P", "", "private", None).expect("project");
        db::create_run(
            &pool,
            &db::NewRun {
                id: "run-gone".to_string(),
                project_id: Some(project.clone()),
                notebook_id: None,
                cell_id: Some("run-gone".to_string()),
                kind: "circuit".to_string(),
                target: "core:node-a".to_string(),
                node_id: Some("node-a".to_string()),
                user_id: "anna".to_string(),
            },
        )
        .expect("run row");

        open_stream("run-gone");
        let cancel = arm("run-gone");
        let handle = tokio::spawn(supervise(
            job(
                &pool,
                &dir,
                "run-gone",
                WIDE_RESET,
                SimulateOptions {
                    shots: 1_000_000,
                    ..SimulateOptions::default()
                },
            ),
            cancel.clone(),
            |_| panic!("there is no row left to finish with"),
        ));
        wait_for_status(&pool, "run-gone", "running").await;

        db::delete_project(&pool, &project).expect("delete");
        assert!(db::run_row(&pool, "run-gone").expect("read").is_none());
        cancel.cancel();
        handle.await.expect("supervision ends");

        match read_stream("run-gone", 0) {
            StreamRead::Frames { closed, .. } => {
                assert_eq!(closed.as_deref(), Some(END_NOT_FOUND))
            }
            _ => panic!("the stream must still be readable and closed"),
        }
    }
}
