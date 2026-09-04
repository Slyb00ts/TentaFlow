// =============================================================================
// File: tentaquant.rs
// Purpose: Binary CBOR protocol for TentaQuant — the quantum lab application.
//          One lab is one INSTANCE of the `tentaquant` package, so every
//          request but `LabListRequest` names the instance it means through
//          `instance_id`; the server proves that id is an enabled instance of
//          this package and evaluates THAT instance's permission matrix before
//          anything else happens.
//
//          Lab membership is not modelled here: it is the instance's
//          permission matrix in Addons INTERSECTED with that instance's
//          Visibility (plan §10.1-10.2) — `quant.read` defaults to allow, so
//          the matrix alone would admit the whole organization and Visibility
//          is what scopes a lab to its group. There are deliberately no
//          variants for granting permissions — `AddonPermission*` and the
//          Visibility tab already do that — and `LabPeople*` is a READ of that
//          intersection. `PeopleCandidates*` is the wider read next to it: the
//          organization's accounts, so a project owner can invite anybody with
//          a TentaFlow account and be told which of them the lab admits today.
//
//          Project ownership follows ML Studio: the creator owns the project,
//          it is private by default and reachable only through an explicit
//          share (`editor`/`viewer`) or `visibility = "lab"` (read-only).
// Example: MessageBody::TentaQuantBody(TentaQuantPayload::LabListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// The six permission ids of `app-manifest.toml` (plan §10.2), in the order the
/// UI shows them. Wire responses carry granted subsets of exactly these.
pub const PERMISSION_IDS: [&str; 6] = [
    "quant.read",
    "quant.run",
    "quant.run.gpu",
    "quant.run.qpu",
    "quant.instruct",
    "quant.admin",
];

/// One lab (= one installed instance of the package) the caller may enter.
///
/// There is no `owner` field and there will not be one: a lab is not owned
/// (§18 decision 26). What the tile shows instead is how many people the lab
/// admits — the matrix intersected with the instance's Visibility — and which
/// permissions the caller holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabInfo {
    /// The instance `addon_id` (`tentaquant-<8hex>`), which every other request
    /// of the family carries.
    pub instance_id: String,
    pub display_name: String,
    /// A disabled instance is listed for admins only, and refuses every other
    /// request — the tile has to be able to say WHY it is inert.
    pub enabled: bool,
    /// Granted subset of [`PERMISSION_IDS`] for the calling user, resolved
    /// through the permission checker.
    pub my_permissions: Vec<String>,
    /// People this instance admits: granted `quant.read` by the matrix AND
    /// inside the instance's Visibility.
    pub people_count: u32,
    /// Projects visible to the CALLER (own + shared + lab), never the total.
    pub project_count: u32,
    /// Last content change in this lab visible to the caller, RFC3339-ish
    /// SQLite datetime, or absent for an untouched lab.
    pub last_activity_at: Option<String>,
    pub nodes: Vec<LabNodeInfo>,
}

/// A node of the fleet as this instance's reconcile left it. `instance_status`
/// is the platform's `__node_status/<node_id>` row ("ready" | "unsupported" |
/// "init_error" | "unknown"), not a live probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabNodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    pub instance_status: String,
}

/// One person the instance admits — granted `quant.read` by the matrix and
/// inside the instance's Visibility — with the permissions they actually
/// resolve to. Purely derived: there is no membership table to disagree with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabPersonInfo {
    pub user_id: String,
    pub display_name: String,
    pub permissions: Vec<String>,
}

/// Upper bound the server clamps `PeopleCandidatesRequest.limit` to. A picker
/// shows a handful of matches and the caller keeps typing; a larger answer would
/// only be a slower way to say "narrow your query".
pub const PEOPLE_CANDIDATES_LIMIT_MAX: u32 = 20;

/// One account the share picker may offer. Every TentaFlow user of the
/// organization is a candidate — inviting somebody is not gated on their lab
/// access — so `in_lab` says which of them the share reaches TODAY: the
/// instance matrix intersected with its Visibility, the same predicate
/// `ProjectShareInfo.has_lab_access` reports for a share already made.
///
/// It deliberately carries no avatar field: the two letters over a row are
/// derived from `display_name` in the browser (`format.js::initials`), which is
/// what every other people list in this screen already draws, and a second copy
/// on the wire could only disagree with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonCandidate {
    pub user_id: String,
    pub display_name: String,
    /// Whether this person is in the laboratory the request named. `false` is
    /// not a refusal to share — the share is stored dormant and the window says
    /// so — it is what the picker has to warn about before the click.
    #[serde(default)]
    pub in_lab: bool,
}

/// Instance-wide settings (`settings` table, plan §9.2). Sent and stored whole:
/// the form edits one document, and a partial write would silently reset the
/// fields an older dashboard build does not know about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabSettings {
    /// Course ranking, on by default (§18 decision 20); `quant.instruct` may
    /// turn it off.
    pub ranking_enabled: bool,
    /// Qubit ceilings per execution tier (§4.2). Browser is the wasm32 limit,
    /// core the native state-vector limit, python the service, gpu the node.
    pub max_qubits_browser: u32,
    pub max_qubits_core: u32,
    pub max_qubits_python: u32,
    pub max_qubits_gpu: u32,
    /// Tier a new notebook starts on: "browser" | "core" | "python" | "gpu".
    pub default_tier: String,
    /// Kernel session idle TTL and cell time limits (§3.2 state machine).
    pub kernel_idle_ttl_secs: u32,
    pub cell_timeout_secs: u32,
    pub gpu_cell_timeout_secs: u32,
    /// T1 runs this laboratory executes at once on one node; the rest queue.
    /// A state vector is the biggest allocation a run makes, so this is the
    /// number that keeps a room full of people clicking "run" from taking the
    /// node down. Defaulted rather than required: an older dashboard that does
    /// not send the field must not reset it to zero.
    #[serde(default = "default_max_concurrent_core_runs")]
    pub max_concurrent_core_runs: u32,
}

fn default_max_concurrent_core_runs() -> u32 {
    2
}

/// The half of the settings document `quant.admin` owns alone (§10.2): how
/// untrusted code is isolated, who accepted running it natively, and how long
/// run artifacts are kept. Kept apart from [`LabSettings`] because the wire
/// answer must be able to OMIT it — a member reads the qubit ceilings (the
/// `device="auto"` rule of §4.2 needs them) without reading the lab's
/// isolation posture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabAdminSettings {
    /// "container" | "trusted_native" (§18 decision 4).
    pub isolation_mode: String,
    /// Run-artifact retention (§9.4).
    pub retention_days: u32,
    /// Who accepted `trusted_native` for a multi-person lab and when
    /// ("<user_id> <timestamp>"), absent while nobody has.
    pub trusted_native_ack: Option<String>,
}

impl Default for LabSettings {
    /// The defaults a freshly installed lab runs with, mirrored by the
    /// `settings` seed of the instance database.
    fn default() -> Self {
        Self {
            ranking_enabled: true,
            max_qubits_browser: 24,
            max_qubits_core: 28,
            max_qubits_python: 28,
            max_qubits_gpu: 30,
            default_tier: "core".to_string(),
            kernel_idle_ttl_secs: 1800,
            cell_timeout_secs: 300,
            gpu_cell_timeout_secs: 900,
            max_concurrent_core_runs: default_max_concurrent_core_runs(),
        }
    }
}

impl Default for LabAdminSettings {
    /// Untrusted code is containerised until an admin says otherwise, and run
    /// artifacts live half a year (§9.4).
    fn default() -> Self {
        Self {
            isolation_mode: "container".to_string(),
            retention_days: 180,
            trusted_native_ack: None,
        }
    }
}

/// A project row as the caller sees it: `my_role` is resolved server-side from
/// ownership, shares and visibility, so the UI never re-derives access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub owner_user_id: String,
    pub owner_name: String,
    /// "private" | "lab".
    pub visibility: String,
    /// "owner" | "editor" | "viewer" | "none". `none` is the answer to a
    /// transfer that left the caller without any access to the project they
    /// just handed over; every other response names a role they hold. It can
    /// only ever reach a client on that one `ProjectResponse` — a project the
    /// caller has no role in is filtered out of every list and answers
    /// `NotFound` on a get — so a UI reads it as "you no longer have this
    /// project", not as a role to render.
    pub my_role: String,
    pub share_count: u32,
    pub file_count: u32,
    pub notebook_count: u32,
    pub run_count: u32,
    /// Project Studio project this one is linked to, when any (§13.7).
    pub linked_project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

/// One share row. `has_lab_access` is false when the person is not in the lab
/// — the matrix withholds `quant.read` or the instance's Visibility does not
/// reach them — and the share is then dormant, which the UI has to say instead
/// of showing an access that does not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectShareInfo {
    pub user_id: String,
    pub display_name: String,
    /// "editor" | "viewer".
    pub role: String,
    pub granted_by: String,
    pub granted_at: String,
    pub has_lab_access: bool,
}

/// A file of a project. The bytes live in the instance CAS (`files/<sha256>`);
/// the row is the name under which the project refers to them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_id: String,
    pub project_id: String,
    pub path: String,
    /// "notebook" | "py" | "qasm" | "data" | "md".
    pub kind: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub updated_at: String,
}

/// Notebook head. Cells travel separately (`NotebookGetResponse.cells_json`)
/// because the list view needs the head without the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookInfo {
    pub notebook_id: String,
    pub project_id: String,
    pub file_id: String,
    pub name: String,
    /// Version the head currently holds; `NotebookSaveRequest.expected_version`
    /// must equal it or the save is a conflict.
    pub current_version: u32,
    pub updated_by: String,
    pub updated_at: String,
}

/// One append-only notebook version. The cells of an old version are fetched
/// with `NotebookGetRequest { version: Some(v) }`, so the list stays cheap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotebookVersionInfo {
    pub version: u32,
    pub sha256: String,
    pub author: String,
    pub created_at: String,
}

// =============================================================================
// Circuits, runs and targets (plan §11.1 `Circuit*` / `Run*` / `Target*`)
// =============================================================================

/// One diagnostic of the OpenQASM 3 front end, positioned in the source.
///
/// The parser stops at the first problem, so a rejected program answers with
/// exactly one entry; it is a list because the editor renders a list either
/// way and because a later front end may report more than one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircuitDiagnostic {
    /// "syntax" | "semantic" | "unsupported" | "parser" | "input" | "invalid"
    /// | "capacity" | "not_clifford" — the class the editor colours by.
    pub kind: String,
    pub message: String,
    /// 1-based, absent for a diagnostic the parser could not place (a rejected
    /// program with no position, a capacity refusal).
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

/// What one T1 simulation is asked to do. Sent whole and defaulted whole: a
/// dashboard build that does not know a field yet must not silently turn it
/// off, so a missing field takes the value from [`SimulateOptions::default`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SimulateOptions {
    pub shots: u64,
    /// Seed of the measurement stream. The same circuit, options and seed
    /// produce the same counts on every tier — that is what `method.md`
    /// promises and what the golden tests assert.
    pub seed: u64,
    /// "auto" | "statevector" | "stabilizer" (plan §6.1).
    pub method: String,
    /// "single" | "double" — amplitude precision of the state vector.
    pub precision: String,
    /// Record one [`StateKeyframe`] per gate (plan §13.6, "record evolution").
    ///
    /// Three-valued on purpose. `None` means "the caller did not decide", and
    /// the server then applies the rule of §13.6: a keyframe costs one pass
    /// over the state per gate, which is free on a small register and ~0.5 s
    /// per gate at 28 qubits, so the evolution is recorded up to
    /// `KEYFRAME_DEFAULT_QUBITS` and is an explicit opt-in above it. A plain
    /// `false` default would silently deny the animation to every small run
    /// whose client omits the field; a plain `true` one would make a 28-qubit
    /// run crawl for a client that never asked.
    #[serde(default)]
    pub record_evolution: Option<bool>,
    /// Store the final state vector as a run artifact, when the circuit has
    /// one and it fits the CAS ceiling (§18 decision 9).
    pub want_state: bool,
    pub want_probabilities: bool,
    /// JSON object binding `input float` parameters, name → number.
    pub inputs_json: String,
    /// Keyframe budget (plan §13.6: K = 256 amplitudes, 16 probabilities).
    pub keyframe_top_k: u32,
    pub keyframe_probs_top: u32,
    /// Which reduced two-qubit density matrices a keyframe carries:
    /// "none" | "gate" (the qubits of the gate that just ran) | "all".
    pub keyframe_pairs: String,
}

impl Default for SimulateOptions {
    fn default() -> Self {
        Self {
            shots: 1024,
            seed: 0,
            method: "auto".to_string(),
            precision: "double".to_string(),
            record_evolution: None,
            want_state: false,
            want_probabilities: false,
            inputs_json: String::new(),
            keyframe_top_k: 256,
            keyframe_probs_top: 16,
            keyframe_pairs: "gate".to_string(),
        }
    }
}

/// Register size up to which the evolution is recorded when the caller did
/// not decide (plan §13.6: above it a keyframe per gate is an opt-in).
pub const KEYFRAME_DEFAULT_QUBITS: u32 = 24;

/// Register size up to which a keyframe may carry the FULL entanglement map
/// (`keyframe_pairs = "all"`). Above it the map is n(n-1)/2 reduced density
/// matrices per gate, which plan §13.6 makes an on-demand query instead.
pub const KEYFRAME_ALL_PAIRS_QUBITS: u32 = 16;

/// Upper bounds on the per-keyframe budgets a caller may ask for. They are
/// allocation sizes inside the simulator (`top_k` sizes a heap, `probs_top` a
/// second one), so an unchecked value from the wire is an out-of-memory abort
/// of the whole node rather than a large answer.
pub const MAX_KEYFRAME_TOP_K: u32 = 1024;
pub const MAX_KEYFRAME_PROBS_TOP: u32 = 256;

/// Measured facts of one finished (or running) run — `runs.metrics_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunMetrics {
    pub duration_ms: u64,
    pub qubits: u32,
    pub clbits: u32,
    pub shots: u64,
    /// Bytes the state vector occupied, the number `max_qubits` bounds.
    pub memory_bytes: u64,
    /// Gates of the circuit. Measurements, resets and barriers are steps of the
    /// program and are NOT counted here — `keyframes` is the number that
    /// follows the step count, because §13.6 records one frame per step.
    pub gates: u32,
    pub keyframes: u32,
    /// "statevector" | "stabilizer" — what actually ran, after `auto` decided.
    pub method: String,
    pub precision: String,
    /// Why the evolution is not in this run, when it was not recorded although
    /// the size rule of §13.6 would have recorded it. A run whose keyframe
    /// budget does not fit keeps its counts and its state and says so; only an
    /// EXPLICIT "record evolution" that cannot fit is a refusal.
    #[serde(default)]
    pub evolution_note: Option<String>,
    /// Simulator backend name, so the UI never has to guess what computed this.
    pub backend: String,
    /// Why the run has no stored state vector, when one was asked for and not
    /// produced: a measured circuit has no single state, and one over the
    /// storage ceiling is not written. A missing artifact with no explanation
    /// is the thing this field exists to prevent.
    #[serde(default)]
    pub state_note: Option<String>,
}

/// One output of a run: a Jupyter-style mime bundle (plan §4.3), inline when
/// it is small and a CAS reference when it is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunArtifactInfo {
    /// Cell the output belongs to; the synthetic id of a circuit run is its
    /// run id, so a run outside a notebook still has one key.
    pub cell_id: String,
    pub seq: u32,
    /// "application/x-tentaquant-counts+json" | "-state+json" |
    /// "-probs+json" | "-keyframes+cbor".
    pub mime: String,
    pub size_bytes: u64,
    /// Content hash in the lab's store, present exactly when the payload was
    /// too large to travel inline. Fetch it with `RunArtifactRequest`.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The payload itself, for outputs under the inline budget.
    #[serde(default)]
    pub inline_json: Option<String>,
}

/// One run row as the caller sees it (plan §9.2 `runs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub notebook_id: Option<String>,
    #[serde(default)]
    pub cell_id: Option<String>,
    /// "cell" | "circuit" | "program" | "kata" | "flow".
    pub kind: String,
    /// The target the run was placed on: `core:<node_id>` for T1.
    pub target: String,
    #[serde(default)]
    pub node_id: Option<String>,
    /// "created" | "queued" | "running" | "succeeded" | "failed" | "cancelled".
    pub status: String,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub metrics: Option<RunMetrics>,
    pub user_id: String,
    pub user_name: String,
    #[serde(default)]
    pub pinned_at: Option<String>,
    #[serde(default)]
    pub thumbnail_sha256: Option<String>,
    #[serde(default)]
    pub keyframes_sha256: Option<String>,
    /// Outputs stored for the run. Empty on a list, filled on a get.
    #[serde(default)]
    pub artifacts: Vec<RunArtifactInfo>,
}

/// The gate a keyframe was taken after: its name, the qubits it acted on and
/// its dense matrix (row-major, `[re, im]` per entry) so the browser can
/// interpolate the frames between two keyframes exactly (plan §13.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeGate {
    pub name: String,
    pub qubits: Vec<u32>,
    pub matrix: Vec<[f64; 2]>,
}

/// Reduced density matrix of one qubit pair, with the two entanglement numbers
/// the map draws from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframePair {
    pub qubits: [u32; 2],
    /// 4×4, row-major, `[re, im]` per entry.
    pub rho: Vec<[f64; 2]>,
    pub mutual_information: f64,
    pub concurrence: f64,
}

/// One large amplitude with the amplitudes the last gate mixed it with, so the
/// bars can be animated without the full state (plan §13.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeAmplitude {
    pub index: u64,
    pub amplitude: [f64; 2],
    pub partners: Vec<KeyframePartner>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframePartner {
    pub index: u64,
    pub amplitude: [f64; 2],
}

/// State of the register after one gate: everything §13.6 draws — Bloch
/// vectors, purity, pair density matrices, the heaviest amplitudes and the
/// heaviest bitstring probabilities.
///
/// One keyframe per gate, the last one after the measurement. They travel live
/// in `RunEvent` and are stored as ONE CBOR artifact in the lab's content
/// store, so the run view replays an evolution the browser never saw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateKeyframe {
    /// Number of program steps applied, i.e. the index of this frame.
    pub step: u32,
    #[serde(default)]
    pub gate: Option<KeyframeGate>,
    pub bloch: Vec<[f64; 3]>,
    pub purity: Vec<f64>,
    pub pairs: Vec<KeyframePair>,
    pub top: Vec<KeyframeAmplitude>,
    pub probs_top: Vec<KeyframeProbability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeProbability {
    pub bitstring: String,
    pub probability: f64,
}

/// One frame of a run stream (plan §11.2). `seq` is monotonic per run, so a
/// consumer deduplicates by comparing rather than by remembering, and
/// `RunSubscribeRequest.after_seq` resumes after a lost connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    pub seq: u64,
    /// "output" | "state_keyframe" | "metrics" | "done".
    pub kind: String,
    #[serde(default)]
    pub output: Option<RunArtifactInfo>,
    #[serde(default)]
    pub keyframe: Option<StateKeyframe>,
    #[serde(default)]
    pub metrics: Option<RunMetrics>,
    /// The final row, carried by the `done` frame.
    #[serde(default)]
    pub run: Option<RunInfo>,
}

/// Kinds of [`RunEvent`], as the producer and the browser both name them.
pub const RUN_EVENT_OUTPUT: &str = "output";
pub const RUN_EVENT_STATE_KEYFRAME: &str = "state_keyframe";
pub const RUN_EVENT_METRICS: &str = "metrics";
pub const RUN_EVENT_DONE: &str = "done";

/// Frames a run stream keeps for replay (plan §11.2). A consumer that fell
/// further behind than this is told `gap` instead of being handed a timeline
/// with a hole in it.
pub const RUN_STREAM_REPLAY_FRAMES: usize = 512;

/// One execution target the laboratory offers (plan §4.1). T0 is the browser,
/// T1 is Core on a node; the tiers above do not exist yet and are reported as
/// unavailable rather than hidden, so the UI can say WHY a big circuit has
/// nowhere to go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetInfo {
    /// "browser" or `core:<node_id>` — what a run stores in `runs.target`.
    pub target: String,
    /// "T0" | "T1".
    pub tier: String,
    #[serde(default)]
    pub node_id: Option<String>,
    pub node_name: String,
    pub is_local: bool,
    pub online: bool,
    /// Whether a run may be placed here right now.
    pub available: bool,
    pub max_qubits: u32,
    /// "single" | "double".
    pub precision: String,
    /// Why it is unavailable, when it is.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A tier the `device="auto"` rule considered and could not use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetUnavailable {
    /// "T0" | "T2" | "T3" | "T4".
    pub tier: String,
    pub reason: String,
}

/// TentaQuant message family (request + response). ciborium encodes variants
/// external-tagged by variant NAME, so never rename a variant or a field
/// without updating the frontend and the golden test (`tentaquant_wire_golden`).
/// The enum is append-only: add at the end, never insert or reorder, and give
/// every new field `#[serde(default)]` so peers that omit it still decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TentaQuantPayload {
    // ---- Lab (the instance itself) ----
    /// The only request WITHOUT an `instance_id`: it is what discovers them.
    LabListRequest {},
    LabListResponse {
        labs: Vec<LabInfo>,
        local_node_id: String,
        /// True when the caller may install another instance from the catalog,
        /// which is what the "+ new lab" tile needs to know.
        can_create: bool,
    },
    LabOverviewRequest {
        instance_id: String,
    },
    LabOverviewResponse {
        instance_id: String,
        /// Projects the caller owns.
        my_projects: u32,
        /// Projects shared with the caller by name.
        shared_with_me: u32,
        /// Projects published to the whole lab (`visibility = "lab"`).
        lab_projects: u32,
        /// Runs of the caller in the last seven days, split by outcome.
        runs_7d_total: u32,
        runs_7d_succeeded: u32,
        runs_7d_failed: u32,
        runs_7d_running: u32,
        /// People the lab admits: `quant.read` from the matrix, inside the
        /// instance's Visibility.
        people_with_access: u32,
        last_activity_at: Option<String>,
    },
    /// Matrix expansion, for `quant.instruct` only.
    LabPeopleRequest {
        instance_id: String,
    },
    LabPeopleResponse {
        instance_id: String,
        people: Vec<LabPersonInfo>,
    },
    SettingsGetRequest {
        instance_id: String,
    },
    SettingsSetRequest {
        instance_id: String,
        settings: LabSettings,
        /// Present only when the caller edits the admin half; a supervisor
        /// sends `None` and never has to echo values it may not read.
        #[serde(default)]
        admin: Option<LabAdminSettings>,
    },
    /// Answer to both `SettingsGetRequest` and `SettingsSetRequest` — the
    /// stored document after the operation, so a rejected field never leaves
    /// the form showing a value the server did not keep. `admin` is filled
    /// only for a caller holding `quant.admin`; for everyone else the admin
    /// half is absent rather than faked with defaults.
    SettingsResponse {
        instance_id: String,
        settings: LabSettings,
        #[serde(default)]
        admin: Option<LabAdminSettings>,
    },

    // ---- Projects ----
    ProjectListRequest {
        instance_id: String,
        #[serde(default)]
        include_archived: bool,
    },
    ProjectListResponse {
        instance_id: String,
        projects: Vec<ProjectInfo>,
    },
    ProjectGetRequest {
        instance_id: String,
        project_id: String,
    },
    ProjectGetResponse {
        instance_id: String,
        project: ProjectInfo,
        /// Empty for anyone but the owner: who a project is shared with is the
        /// owner's business.
        shares: Vec<ProjectShareInfo>,
    },
    ProjectCreateRequest {
        instance_id: String,
        name: String,
        #[serde(default)]
        description: String,
        /// "private" | "lab"; publishing to the lab needs `quant.instruct`.
        visibility: String,
        #[serde(default)]
        linked_project_id: Option<String>,
    },
    ProjectUpdateRequest {
        instance_id: String,
        project_id: String,
        name: String,
        #[serde(default)]
        description: String,
        visibility: String,
        #[serde(default)]
        linked_project_id: Option<String>,
    },
    ProjectArchiveRequest {
        instance_id: String,
        project_id: String,
        archived: bool,
    },
    /// Hands the project to another member; the previous owner keeps no
    /// implicit access (the new owner may share it back).
    ProjectTransferRequest {
        instance_id: String,
        project_id: String,
        new_owner_user_id: String,
    },
    /// Answer to create/update/archive/transfer: the project as it now stands.
    ProjectResponse {
        instance_id: String,
        project: ProjectInfo,
    },
    ProjectDeleteRequest {
        instance_id: String,
        project_id: String,
    },
    ProjectDeleteResponse {
        instance_id: String,
        project_id: String,
    },
    ProjectShareSetRequest {
        instance_id: String,
        project_id: String,
        user_id: String,
        /// "editor" | "viewer".
        role: String,
    },
    ProjectShareRemoveRequest {
        instance_id: String,
        project_id: String,
        user_id: String,
    },
    ProjectSharesResponse {
        instance_id: String,
        project_id: String,
        shares: Vec<ProjectShareInfo>,
    },

    // ---- Files (CAS, 4 MiB chunks) ----
    FileUploadChunkRequest {
        instance_id: String,
        project_id: String,
        /// Client-chosen stream id; chunks must arrive in order and `seq == 0`
        /// restarts the stream.
        upload_id: String,
        path: String,
        kind: String,
        seq: u32,
        total_chunks: u32,
        bytes: Vec<u8>,
    },
    FileUploadChunkResponse {
        instance_id: String,
        project_id: String,
        upload_id: String,
        received_chunks: u32,
        received_bytes: u64,
        complete: bool,
        /// Present only on the chunk that completed the file.
        file: Option<FileInfo>,
    },
    FileListRequest {
        instance_id: String,
        project_id: String,
    },
    FileListResponse {
        instance_id: String,
        project_id: String,
        files: Vec<FileInfo>,
    },
    FileDeleteRequest {
        instance_id: String,
        project_id: String,
        file_id: String,
    },
    FileDeleteResponse {
        instance_id: String,
        project_id: String,
        file_id: String,
    },

    // ---- Notebooks (append-only versions, optimistic locking) ----
    NotebookListRequest {
        instance_id: String,
        project_id: String,
    },
    NotebookListResponse {
        instance_id: String,
        project_id: String,
        notebooks: Vec<NotebookInfo>,
    },
    NotebookCreateRequest {
        instance_id: String,
        project_id: String,
        name: String,
        /// JSON array of cells; an empty string starts an empty notebook.
        #[serde(default)]
        cells_json: String,
    },
    NotebookGetRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
        /// Absent = the head version.
        #[serde(default)]
        version: Option<u32>,
    },
    NotebookGetResponse {
        instance_id: String,
        notebook: NotebookInfo,
        /// Cells of the requested version.
        version: u32,
        cells_json: String,
    },
    NotebookSaveRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
        cells_json: String,
        /// Version the editor last read. A mismatch is a `Conflict`, never a
        /// silent overwrite.
        expected_version: u32,
    },
    /// Answer to create/save: the notebook head after the write.
    NotebookResponse {
        instance_id: String,
        notebook: NotebookInfo,
    },
    NotebookVersionsRequest {
        instance_id: String,
        project_id: String,
        notebook_id: String,
    },
    NotebookVersionsResponse {
        instance_id: String,
        notebook_id: String,
        versions: Vec<NotebookVersionInfo>,
    },

    // ---- Directory search (the share picker) ----
    /// Searches the organization's TentaFlow accounts so a project owner can
    /// invite anybody who has an account, not only the people already in this
    /// laboratory. Answered for any caller the instance admits — `quant.read`
    /// plus Visibility, the same membership every other request needs — because
    /// sharing is the owner's decision and needs no supervisor to enumerate the
    /// lab first. `instance_id` is still what makes the answer meaningful: it
    /// is the laboratory `in_lab` is resolved against.
    PeopleCandidatesRequest {
        instance_id: String,
        /// Matched case-insensitively as a substring of the display name and of
        /// the login. An empty query answers with nothing rather than the whole
        /// organization — a picker opens empty and fills as the user types.
        #[serde(default)]
        query: String,
        /// Clamped to [`PEOPLE_CANDIDATES_LIMIT_MAX`]; `0` means "as many as
        /// that allows".
        #[serde(default)]
        limit: u32,
    },
    PeopleCandidatesResponse {
        instance_id: String,
        #[serde(default)]
        people: Vec<PersonCandidate>,
    },

    // ---- Circuits (OpenQASM 3, tier T1 in Core) ----
    /// Parse and check a program without running it. Answers the IR the editor
    /// draws plus, on rejection, the diagnostic with its line and column.
    CircuitValidateRequest {
        instance_id: String,
        qasm3: String,
        /// JSON object binding `input float` parameters, name → number.
        #[serde(default)]
        inputs_json: String,
    },
    CircuitValidateResponse {
        instance_id: String,
        valid: bool,
        /// The circuit IR as JSON — the same shape the browser tier returns —
        /// or an empty string when the program was rejected.
        ir_json: String,
        num_qubits: u32,
        num_clbits: u32,
        is_clifford: bool,
        errors: Vec<CircuitDiagnostic>,
    },
    /// Start a T1 run of one circuit on the node that receives the request.
    /// Answers with the `runs` row; outputs arrive through `RunSubscribe`.
    CircuitSimulateRequest {
        instance_id: String,
        qasm3: String,
        #[serde(default)]
        options: SimulateOptions,
        /// Project the run belongs to, when it was started from one. A run
        /// without a project is the scratch run of the circuit studio.
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        notebook_id: Option<String>,
        #[serde(default)]
        cell_id: Option<String>,
    },
    /// Translate a circuit into another textual form: "qasm3" (canonical
    /// OpenQASM 3 out of the IR), "qiskit" (a Python program) or "ir" (the
    /// JSON IR itself).
    CircuitExportRequest {
        instance_id: String,
        qasm3: String,
        format: String,
        #[serde(default)]
        inputs_json: String,
    },
    CircuitExportResponse {
        instance_id: String,
        format: String,
        content: String,
        /// Name the browser saves the content under.
        filename: String,
    },

    // ---- Runs ----
    RunListRequest {
        instance_id: String,
        /// Only runs of one project; absent lists every run the caller may see.
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        pinned_only: bool,
        /// 0 means the server's page size.
        #[serde(default)]
        limit: u32,
    },
    RunListResponse {
        instance_id: String,
        runs: Vec<RunInfo>,
    },
    RunGetRequest {
        instance_id: String,
        run_id: String,
    },
    /// Answer to get, cancel, pin and to starting a simulation: the row as it
    /// now stands.
    RunResponse {
        instance_id: String,
        run: RunInfo,
    },
    RunCancelRequest {
        instance_id: String,
        run_id: String,
    },
    RunPinRequest {
        instance_id: String,
        run_id: String,
        pinned: bool,
    },
    /// Mints a signed download URL for one artifact of a run (scope
    /// `TentaQuantArtifact`), the way a Project Studio export is fetched.
    RunArtifactRequest {
        instance_id: String,
        run_id: String,
        sha256: String,
    },
    RunArtifactResponse {
        instance_id: String,
        run_id: String,
        sha256: String,
        url: String,
        expires_at_ms: u64,
        size_bytes: u64,
        mime: String,
    },
    /// Live stream of one run. Frames carry a monotonic `seq`; a reconnect
    /// resumes with `after_seq` out of the replay buffer.
    RunSubscribeRequest {
        instance_id: String,
        run_id: String,
        #[serde(default)]
        after_seq: u64,
    },
    RunEventChunk {
        instance_id: String,
        run_id: String,
        event: RunEvent,
    },
    /// Terminal frame of a run stream: "completed" | "gap" | "cancelled" |
    /// "not_found" | "error".
    RunStreamEnd {
        instance_id: String,
        run_id: String,
        reason: String,
    },
    /// The recorded evolution of a finished run, read back from the CBOR
    /// artifact in the lab's content store.
    RunKeyframesRequest {
        instance_id: String,
        run_id: String,
    },
    RunKeyframesResponse {
        instance_id: String,
        run_id: String,
        keyframes: Vec<StateKeyframe>,
    },

    // ---- Targets (tiers, nodes and the `device="auto"` rule) ----
    TargetListRequest {
        instance_id: String,
    },
    TargetListResponse {
        instance_id: String,
        local_node_id: String,
        targets: Vec<TargetInfo>,
        /// Tiers the laboratory does not offer yet, with the reason — the UI
        /// must not present T2/T3/T4 as choices that silently do nothing.
        unavailable: Vec<TargetUnavailable>,
    },
    /// The `device="auto"` rule of plan §5.3, evaluated server-side so the UI
    /// can show "auto → T1 · node-a" BEFORE the run starts.
    TargetResolveRequest {
        instance_id: String,
        num_qubits: u32,
        /// The caller is the browser and could run the circuit itself (T0).
        #[serde(default)]
        from_browser: bool,
        /// The unit is a Python cell, so only a kernel tier could run it.
        #[serde(default)]
        needs_kernel: bool,
    },
    TargetResolveResponse {
        instance_id: String,
        /// "browser" or `core:<node_id>`; empty when no tier can take it.
        target: String,
        /// "T0" | "T1" | "none".
        tier: String,
        #[serde(default)]
        node_id: Option<String>,
        /// Why the rule chose this, in the words the UI shows.
        reason: String,
        unavailable: Vec<TargetUnavailable>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageBody;

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    fn round_trip(payload: TentaQuantPayload) {
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded = crate::cbor::decode::<TentaQuantPayload>(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn lab_family_round_trips() {
        round_trip(TentaQuantPayload::LabListRequest {});
        round_trip(TentaQuantPayload::LabListResponse {
            labs: vec![LabInfo {
                instance_id: "tentaquant-0a1b2c3d".to_string(),
                display_name: "Kwanty R&D".to_string(),
                enabled: true,
                my_permissions: vec!["quant.read".to_string(), "quant.run".to_string()],
                people_count: 42,
                project_count: 3,
                last_activity_at: Some("2026-09-03 14:02:00".to_string()),
                nodes: vec![LabNodeInfo {
                    node_id: "node-1".to_string(),
                    node_name: "spark-01".to_string(),
                    is_local: true,
                    online: true,
                    instance_status: "ready".to_string(),
                }],
            }],
            local_node_id: "node-1".to_string(),
            can_create: false,
        });
        round_trip(TentaQuantPayload::SettingsResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            settings: LabSettings::default(),
            admin: Some(LabAdminSettings::default()),
        });
    }

    #[test]
    fn project_and_notebook_families_round_trip() {
        round_trip(TentaQuantPayload::ProjectCreateRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            name: "Grover 4-kubitowy".to_string(),
            description: String::new(),
            visibility: "private".to_string(),
            linked_project_id: None,
        });
        round_trip(TentaQuantPayload::NotebookSaveRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            notebook_id: "n1".to_string(),
            cells_json: "[]".to_string(),
            expected_version: 3,
        });
        round_trip(TentaQuantPayload::FileUploadChunkRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            upload_id: "up-1".to_string(),
            path: "data/bell.json".to_string(),
            kind: "data".to_string(),
            seq: 0,
            total_chunks: 1,
            bytes: vec![0x00, 0xff, 0x10],
        });
    }

    #[test]
    fn people_candidates_round_trips() {
        round_trip(TentaQuantPayload::PeopleCandidatesRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            query: "nowak".to_string(),
            limit: PEOPLE_CANDIDATES_LIMIT_MAX,
        });
        round_trip(TentaQuantPayload::PeopleCandidatesResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            people: vec![PersonCandidate {
                user_id: "u5".to_string(),
                display_name: "Marek Nowak".to_string(),
                in_lab: false,
            }],
        });
    }

    /// The tier-T1 families: every one of them has to survive the wire, and
    /// the keyframe is the one that would hurt most if it did not — it is the
    /// stored artifact as well as the streamed frame.
    #[test]
    fn circuit_run_and_target_families_round_trip() {
        round_trip(TentaQuantPayload::CircuitValidateRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            qasm3: "h q[0];".to_string(),
            inputs_json: "{\"theta\":0.5}".to_string(),
        });
        round_trip(TentaQuantPayload::CircuitValidateResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            valid: false,
            ir_json: String::new(),
            num_qubits: 0,
            num_clbits: 0,
            is_clifford: false,
            errors: vec![CircuitDiagnostic {
                kind: "syntax".to_string(),
                message: "line 4, column 1: syntax error".to_string(),
                line: Some(4),
                column: Some(1),
            }],
        });
        round_trip(TentaQuantPayload::CircuitSimulateRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            qasm3: "h q[0];".to_string(),
            options: SimulateOptions::default(),
            project_id: Some("p1".to_string()),
            notebook_id: None,
            cell_id: Some("c1".to_string()),
        });
        round_trip(TentaQuantPayload::RunResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            run: RunInfo {
                run_id: "r1".to_string(),
                project_id: None,
                notebook_id: None,
                cell_id: Some("c1".to_string()),
                kind: "circuit".to_string(),
                target: "core:node-a".to_string(),
                node_id: Some("node-a".to_string()),
                status: "succeeded".to_string(),
                started_at: "2026-09-04 10:00:00".to_string(),
                ended_at: Some("2026-09-04 10:00:01".to_string()),
                error: None,
                metrics: Some(RunMetrics {
                    duration_ms: 12,
                    qubits: 2,
                    clbits: 2,
                    shots: 1024,
                    memory_bytes: 64,
                    gates: 3,
                    keyframes: 3,
                    method: "statevector".to_string(),
                    precision: "double".to_string(),
                    evolution_note: None,
                    backend: "cpu".to_string(),
                    state_note: None,
                }),
                user_id: "u1".to_string(),
                user_name: "Anna".to_string(),
                pinned_at: None,
                thumbnail_sha256: None,
                keyframes_sha256: Some("ab".repeat(32)),
                artifacts: vec![RunArtifactInfo {
                    cell_id: "c1".to_string(),
                    seq: 0,
                    mime: "application/x-tentaquant-counts+json".to_string(),
                    size_bytes: 42,
                    sha256: None,
                    inline_json: Some("{\"shots\":1024}".to_string()),
                }],
            },
        });
        round_trip(TentaQuantPayload::RunEventChunk {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            run_id: "r1".to_string(),
            event: RunEvent {
                seq: 7,
                kind: RUN_EVENT_STATE_KEYFRAME.to_string(),
                output: None,
                keyframe: Some(StateKeyframe {
                    step: 2,
                    gate: Some(KeyframeGate {
                        name: "cx".to_string(),
                        qubits: vec![0, 1],
                        matrix: vec![[1.0, 0.0], [0.0, -1.0]],
                    }),
                    bloch: vec![[0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
                    purity: vec![1.0, 0.5],
                    pairs: vec![KeyframePair {
                        qubits: [0, 1],
                        rho: vec![[0.5, 0.0], [0.0, 0.5]],
                        mutual_information: 2.0,
                        concurrence: 1.0,
                    }],
                    top: vec![KeyframeAmplitude {
                        index: 3,
                        amplitude: [0.707, 0.0],
                        partners: vec![KeyframePartner {
                            index: 0,
                            amplitude: [0.707, 0.0],
                        }],
                    }],
                    probs_top: vec![KeyframeProbability {
                        bitstring: "11".to_string(),
                        probability: 0.5,
                    }],
                }),
                metrics: None,
                run: None,
            },
        });
        round_trip(TentaQuantPayload::RunStreamEnd {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            run_id: "r1".to_string(),
            reason: "gap".to_string(),
        });
        round_trip(TentaQuantPayload::TargetListResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            local_node_id: "node-a".to_string(),
            targets: vec![TargetInfo {
                target: "core:node-a".to_string(),
                tier: "T1".to_string(),
                node_id: Some("node-a".to_string()),
                node_name: "spark-01".to_string(),
                is_local: true,
                online: true,
                available: true,
                max_qubits: 28,
                precision: "double".to_string(),
                reason: None,
            }],
            unavailable: vec![TargetUnavailable {
                tier: "T3".to_string(),
                reason: "no GPU tier in this build".to_string(),
            }],
        });
        round_trip(TentaQuantPayload::TargetResolveResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            target: "core:node-a".to_string(),
            tier: "T1".to_string(),
            node_id: Some("node-a".to_string()),
            reason: "26 qubits fit Core on spark-01 (up to 28)".to_string(),
            unavailable: Vec::new(),
        });
    }

    /// A run of one circuit produces one keyframe per gate and the browser
    /// interpolates between them, so the CBOR has to carry the numbers a
    /// frame is built from — losing `partners` or `rho` would be a silent
    /// downgrade of the animation rather than a decode failure.
    #[test]
    fn a_keyframe_survives_the_wire_with_its_numbers() {
        let frame = StateKeyframe {
            step: 1,
            gate: Some(KeyframeGate {
                name: "h".to_string(),
                qubits: vec![0],
                matrix: vec![[0.5f64.sqrt(), 0.0], [0.5f64.sqrt(), 0.0]],
            }),
            bloch: vec![[1.0, 0.0, 0.0]],
            purity: vec![1.0],
            pairs: Vec::new(),
            top: vec![KeyframeAmplitude {
                index: 0,
                amplitude: [0.5f64.sqrt(), 0.0],
                partners: vec![KeyframePartner {
                    index: 1,
                    amplitude: [0.5f64.sqrt(), 0.0],
                }],
            }],
            probs_top: vec![KeyframeProbability {
                bitstring: "0".to_string(),
                probability: 0.5,
            }],
        };
        let bytes = crate::cbor::encode(&vec![frame.clone()]).expect("encode");
        let decoded = crate::cbor::decode::<Vec<StateKeyframe>>(&bytes).expect("decode");
        assert_eq!(decoded, vec![frame]);
    }

    /// The options struct defaults as a WHOLE (`#[serde(default)]` on the
    /// container), so a dashboard that knows only `shots` gets the server's
    /// defaults for the rest instead of zeros — a zero `keyframe_top_k` would
    /// silently produce empty frames.
    #[test]
    fn simulate_options_default_field_by_field() {
        let partial: SimulateOptions =
            serde_json::from_str(r#"{"shots": 16}"#).expect("partial options decode");
        assert_eq!(partial.shots, 16);
        assert_eq!(partial.keyframe_top_k, 256);
        assert_eq!(partial.method, "auto");
        assert_eq!(partial.precision, "double");
        assert_eq!(partial.keyframe_pairs, "gate");
        // The one field that must NOT resolve to a value here: "not decided"
        // travels as absent, and the server applies the §13.6 size rule to it.
        assert_eq!(partial.record_evolution, None);
    }

    #[test]
    fn message_body_tentaquant_round_trip() {
        let body = MessageBody::TentaQuantBody(TentaQuantPayload::LabOverviewRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, a field or the
    /// `MessageBody::TentaQuantBody` tag into a test failure instead of a
    /// silent break of every deployed peer.
    #[test]
    fn tentaquant_wire_golden() {
        // TentaQuantPayload::LabListRequest {} — the family's entry point and
        // the one request without an instance id.
        let list = TentaQuantPayload::LabListRequest {};
        let bytes = crate::cbor::encode(&list).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e4c61624c69737452657175657374a0"),
            "LabListRequest wire drift"
        );

        // MessageBody::TentaQuantBody(LabListRequest) — outer body tag + variant tag.
        let body = MessageBody::TentaQuantBody(list);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16e54656e74615175616e74426f6479a16e4c61624c69737452657175657374a0"),
            "MessageBody::TentaQuantBody wire drift"
        );

        // Every other request carries the lab it means — pin that field name.
        let overview = TentaQuantPayload::LabOverviewRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
        };
        let bytes = crate::cbor::encode(&overview).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1724c61624f7665727669657752657175657374a16b696e7374616e63655f69647374656e74617175616e742d3061316232633364"
            ),
            "LabOverviewRequest wire drift"
        );

        // The optimistic-locking save — full field set, `expected_version` last.
        let save = TentaQuantPayload::NotebookSaveRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            project_id: "p1".to_string(),
            notebook_id: "n1".to_string(),
            cells_json: "[]".to_string(),
            expected_version: 3,
        };
        let bytes = crate::cbor::encode(&save).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1734e6f7465626f6f6b5361766552657175657374a56b696e7374616e63655f69647374656e74617175616e742d30613162326333646a70726f6a6563745f69646270316b6e6f7465626f6f6b5f6964626e316a63656c6c735f6a736f6e625b5d7065787065637465645f76657273696f6e03"
            ),
            "NotebookSaveRequest wire drift"
        );

        // The settings document with its defaults: pins every field name AND
        // the default values a fresh lab starts with, in both halves.
        let settings = TentaQuantPayload::SettingsResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            settings: LabSettings::default(),
            admin: Some(LabAdminSettings::default()),
        };
        let bytes = crate::cbor::encode(&settings).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17053657474696e6773526573706f6e7365a36b696e7374616e63655f69647374656e74617175616e742d30613162326333646873657474696e6773aa6f72616e6b696e675f656e61626c6564f5726d61785f7175626974735f62726f7773657218186f6d61785f7175626974735f636f7265181c716d61785f7175626974735f707974686f6e181c6e6d61785f7175626974735f677075181e6c64656661756c745f7469657264636f7265746b65726e656c5f69646c655f74746c5f736563731907087163656c6c5f74696d656f75745f7365637319012c756770755f63656c6c5f74696d656f75745f7365637319038478186d61785f636f6e63757272656e745f636f72655f72756e73026561646d696ea36e69736f6c6174696f6e5f6d6f646569636f6e7461696e65726e726574656e74696f6e5f6461797318b472747275737465645f6e61746976655f61636bf6"
            ),
            "SettingsResponse wire drift"
        );

        // The directory search: pins the variant name and all three field
        // names, because the dashboard's encoder builds it from a JSON object
        // keyed exactly like this.
        let candidates = TentaQuantPayload::PeopleCandidatesRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            query: "nowak".to_string(),
            limit: 20,
        };
        let bytes = crate::cbor::encode(&candidates).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17750656f706c6543616e6469646174657352657175657374a36b696e7374616e63655f69647374656e74617175616e742d3061316232633364657175657279656e6f77616b656c696d697414"
            ),
            "PeopleCandidatesRequest wire drift"
        );

        // One candidate row — `in_lab` is what the picker warns on, so its name
        // is part of the contract.
        let answer = TentaQuantPayload::PeopleCandidatesResponse {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            people: vec![PersonCandidate {
                user_id: "u5".to_string(),
                display_name: "Marek Nowak".to_string(),
                in_lab: false,
            }],
        };
        let bytes = crate::cbor::encode(&answer).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1781850656f706c6543616e64696461746573526573706f6e7365a26b696e7374616e63655f69647374656e74617175616e742d30613162326333646670656f706c6581a367757365725f69646275356c646973706c61795f6e616d656b4d6172656b204e6f77616b66696e5f6c6162f4"
            ),
            "PeopleCandidatesResponse wire drift"
        );

        // The request that STARTS a run. It pins the whole `SimulateOptions`
        // document — every field name and every default — because the browser
        // encoder builds it from a JSON object keyed exactly like this, and a
        // renamed field would silently fall back to a default instead of
        // failing.
        let simulate = TentaQuantPayload::CircuitSimulateRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            qasm3: "h q[0];".to_string(),
            options: SimulateOptions::default(),
            project_id: None,
            notebook_id: None,
            cell_id: None,
        };
        let bytes = crate::cbor::encode(&simulate).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1764369726375697453696d756c61746552657175657374a66b696e7374616e63655f69647374656e74617175616e742d3061316232633364657161736d33676820715b305d3b676f7074696f6e73ab6573686f7473190400647365656400666d6574686f64646175746f69707265636973696f6e66646f75626c65707265636f72645f65766f6c7574696f6ef66a77616e745f7374617465f47277616e745f70726f626162696c6974696573f46b696e707574735f6a736f6e606e6b65796672616d655f746f705f6b190100726b65796672616d655f70726f62735f746f70106e6b65796672616d655f706169727364676174656a70726f6a6563745f6964f66b6e6f7465626f6f6b5f6964f66763656c6c5f6964f6"
            ),
            "CircuitSimulateRequest wire drift"
        );

        // Resuming a run stream after a lost connection: `after_seq` is the
        // whole contract of the replay buffer.
        let subscribe = TentaQuantPayload::RunSubscribeRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            run_id: "r1".to_string(),
            after_seq: 512,
        };
        let bytes = crate::cbor::encode(&subscribe).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17352756e53756273637269626552657175657374a36b696e7374616e63655f69647374656e74617175616e742d30613162326333646672756e5f69646272316961667465725f736571190200"
            ),
            "RunSubscribeRequest wire drift"
        );

        // One streamed keyframe. The frame is ALSO the stored artifact, so its
        // field names are a storage format as well as a wire format: renaming
        // one would make every recorded evolution unreadable.
        let chunk = TentaQuantPayload::RunEventChunk {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            run_id: "r1".to_string(),
            event: RunEvent {
                seq: 3,
                kind: RUN_EVENT_STATE_KEYFRAME.to_string(),
                output: None,
                keyframe: Some(StateKeyframe {
                    step: 1,
                    gate: Some(KeyframeGate {
                        name: "h".to_string(),
                        qubits: vec![0],
                        matrix: vec![[1.0, 0.0]],
                    }),
                    bloch: vec![[1.0, 0.0, 0.0]],
                    purity: vec![1.0],
                    pairs: Vec::new(),
                    top: Vec::new(),
                    probs_top: vec![KeyframeProbability {
                        bitstring: "0".to_string(),
                        probability: 0.5,
                    }],
                }),
                metrics: None,
                run: None,
            },
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16d52756e4576656e744368756e6ba36b696e7374616e63655f69647374656e74617175616e742d30613162326333646672756e5f6964627231656576656e74a66373657103646b696e646e73746174655f6b65796672616d65666f7574707574f6686b65796672616d65a76473746570016467617465a3646e616d656168667175626974738100666d61747269788182f93c00f9000065626c6f63688183f93c00f90000f900006670757269747981f93c006570616972738063746f70806970726f62735f746f7081a269626974737472696e6761306b70726f626162696c697479f93800676d657472696373f66372756ef6"
            ),
            "RunEventChunk wire drift"
        );

        // The `device="auto"` question, as the UI asks it before a run starts.
        let resolve = TentaQuantPayload::TargetResolveRequest {
            instance_id: "tentaquant-0a1b2c3d".to_string(),
            num_qubits: 26,
            from_browser: true,
            needs_kernel: false,
        };
        let bytes = crate::cbor::encode(&resolve).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1745461726765745265736f6c766552657175657374a46b696e7374616e63655f69647374656e74617175616e742d30613162326333646a6e756d5f717562697473181a6c66726f6d5f62726f77736572f56c6e656564735f6b65726e656cf4"
            ),
            "TargetResolveRequest wire drift"
        );
    }
}
