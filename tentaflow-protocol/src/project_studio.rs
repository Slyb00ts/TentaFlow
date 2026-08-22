// =============================================================================
// File: project_studio.rs
// Purpose: Binary CBOR protocol for Project Studio ("Projekty") — project
//          registry, members and creator grants, knowledge sources with chunked
//          upload and ingest jobs, source files, KB search, overview/activity,
//          per-user project chats, settings/tags, plus live ingest and chat
//          streaming. Chats are private per user: the server filters every chat
//          query by the authenticated caller, the wire never exposes another
//          user's chats.
// Example: MessageBody::ProjectStudioBody(ProjectStudioPayload::ProjectsListRequest { .. })
// =============================================================================

use serde::{Deserialize, Serialize};

/// Project row for the registry list and detail views. `my_role` is `None` for
/// an org admin inspecting a project they are not a member of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub project_id: String,
    pub name: String,
    pub description: String,
    /// 'active' | 'archived'.
    pub status: String,
    /// "tests" | "docs" | "tests_docs" | "custom".
    pub template: String,
    /// Enabled modules: "knowledge", "tests", "docs", "chat", "tasks".
    pub modules: Vec<String>,
    pub owner_user_id: String,
    pub owner_name: String,
    pub member_count: u32,
    pub source_count: u32,
    pub sources_ready: u32,
    pub my_role: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Project member with display data resolved server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub user_id: String,
    pub display_name: String,
    pub email: String,
    /// 'owner' | 'manager' | 'editor' | 'tester' | 'viewer'.
    pub role: String,
    pub invited_by: String,
    pub invited_by_name: String,
    pub created_at: String,
}

/// Member entry sent by the client when creating a project or adding members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberInputWire {
    pub user_id: String,
    pub role: String,
}

/// Lightweight user reference for member-candidate pickers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserRefWire {
    pub user_id: String,
    pub display_name: String,
    pub email: String,
}

/// Project-creation grant row (admin view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatorGrantInfo {
    pub user_id: String,
    pub display_name: String,
    pub granted_by: String,
    pub created_at: String,
}

/// Ingest job state; polling `IngestStatusRequest` is the source of truth,
/// the stream is only a live view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestJobWire {
    pub job_id: String,
    pub source_id: String,
    /// 'running' | 'success' | 'failed' | 'cancelled'.
    pub status: String,
    pub files_total: u32,
    pub files_done: u32,
    pub chunks_done: u32,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Knowledge source with its latest ingest job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub source_id: String,
    /// 'document' | 'url' | 'git' | 'zip' | 'api_spec' (F1 accepts document+url,
    /// the rest is rejected with BadRequest until F3).
    pub kind: String,
    pub name: String,
    /// 'pending' | 'indexing' | 'ready' | 'error' | 'cancelled'.
    pub status: String,
    pub config_json: String,
    pub error: Option<String>,
    pub file_count: u32,
    pub chunk_count: u32,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_job: Option<IngestJobWire>,
}

/// Single file inside a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFileInfo {
    pub file_id: String,
    pub source_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub mime: String,
    /// 'pending' | 'indexing' | 'ready' | 'skipped' | 'error'.
    pub status: String,
    /// Skip/error reason.
    pub error: Option<String>,
    pub chunk_count: u32,
    pub updated_at: String,
}

/// Knowledge-base search hit. `location` is a human-readable position,
/// e.g. "str. 24" or "l. 88–121".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KbHit {
    pub source_id: String,
    pub source_name: String,
    pub source_kind: String,
    pub file_id: String,
    pub file_path: String,
    pub chunk_index: u32,
    pub score: f32,
    pub snippet: String,
    pub location: String,
    pub metadata_json: String,
}

/// KPI counters for the project overview screen. `my_chat_count` counts only
/// the caller's own chats (chats are private per user).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewKpis {
    pub sources_total: u32,
    pub sources_ready: u32,
    pub files_total: u32,
    pub chunks_total: u32,
    pub member_count: u32,
    pub open_ingest_jobs: u32,
    pub my_chat_count: u32,
    // F2 counters appended with #[serde(default)] so frames produced by F1
    // peers (which omit them) still decode — struct fields are append-only
    // on the wire, same as enum variants.
    #[serde(default)]
    pub cases_total: u32,
    #[serde(default)]
    pub cases_approved: u32,
    #[serde(default)]
    pub suites_total: u32,
    #[serde(default)]
    pub runs_open: u32,
    /// Pending run items assigned to (or claimable by) the caller.
    #[serde(default)]
    pub my_run_items_pending: u32,
    #[serde(default)]
    pub tasks_open: u32,
    #[serde(default)]
    pub defects_open: u32,
    #[serde(default)]
    pub generations_running: u32,
    // F3 counters appended with #[serde(default)] for the same reason:
    // frames produced by F2 peers omit them and must still decode.
    #[serde(default)]
    pub environments_approved: u32,
    #[serde(default)]
    pub environments_pending: u32,
    #[serde(default)]
    pub auto_runs_open: u32,
    // F4 counters appended with #[serde(default)] for the same reason:
    // frames produced by F3 peers omit them and must still decode.
    #[serde(default)]
    pub schedules_enabled: u32,
    /// Enabled schedules that cannot fire (environment not approved or the
    /// failure breaker tripped) — the overview flags them before they silently
    /// stop producing runs.
    #[serde(default)]
    pub schedules_blocked: u32,
    #[serde(default)]
    pub ml_links: u32,
}

/// One activity-log entry for the project feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub actor_user_id: String,
    pub actor_name: String,
    /// 'user' | 'agent' | 'system'.
    pub actor_kind: String,
    /// e.g. 'source.created', 'member.added', 'settings.saved'.
    pub action: String,
    pub object_type: String,
    pub object_id: String,
    pub details_json: String,
    pub created_at: String,
}

/// Chat summary. Only chats owned by the caller are ever returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatInfo {
    pub chat_id: String,
    pub title: String,
    pub last_message_preview: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Persisted chat message with RAG citations serialized as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageWire {
    pub message_id: String,
    pub role: String,
    pub content: String,
    pub citations_json: String,
    pub created_at: String,
}

/// Agent bound to a project function. `agent_id` empty = platform default;
/// `model_label` shows the resolved model next to the select (read-only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectAgentBinding {
    /// 'chat', 'generator_manual', 'generator_ui', 'generator_api',
    /// 'generator_unit', 'generator_perf', 'security', 'documentalist',
    /// 'critic', 'supervisor' (F1 UI exposes only 'chat').
    pub function: String,
    pub agent_id: String,
    pub agent_name: String,
    pub model_label: String,
}

/// Project tag with usage counter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagInfo {
    pub tag_id: String,
    pub name: String,
    pub usage_count: u32,
}

/// Aggregated settings view for the project settings screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub name: String,
    pub description: String,
    pub modules: Vec<String>,
    pub agents: Vec<ProjectAgentBinding>,
    pub tags: Vec<TagInfo>,
    /// Whether an ingest of this project also extracts a knowledge graph.
    /// Appended field: a peer predating it omits it, and `false` is the same
    /// answer the server gives for a project that never opted in.
    #[serde(default)]
    pub graph_extraction: bool,
}

/// Attachment reference stored on cases, run items, steps and tasks.
/// Bytes are content-addressed by `sha256`; download goes through
/// `AttachmentGetRequest`, upload reuses `SourceUploadChunkRequest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentWire {
    pub sha256: String,
    pub name: String,
    pub size_bytes: u64,
    pub mime: String,
}

/// Manual test case row for lists. Cases with `review_state == "pending"`
/// (agent output awaiting review) are excluded from every list/search —
/// they surface only inside `GenerationGetResponse.pending_cases`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestCaseInfo {
    pub case_id: String,
    /// 'manual' | 'ui' | 'api' | 'unit' | 'perf' | 'security' (F2 accepts
    /// only 'manual', the rest is rejected until later phases).
    pub kind: String,
    pub title: String,
    /// 'low' | 'medium' | 'high' | 'critical'.
    pub priority: String,
    /// 'draft' | 'review' | 'approved' | 'deprecated'.
    pub status: String,
    /// Reason for the last status change (required on every downgrade).
    pub status_reason: String,
    /// '' | 'pending' | 'accepted' — agent-generated cases start 'pending'.
    pub review_state: String,
    /// 'user' | 'agent'.
    pub origin: String,
    /// Generation run that produced the case ('' for user-authored).
    pub generation_run_id: String,
    pub language: String,
    pub current_version: u32,
    pub tag_ids: Vec<String>,
    pub linked_source_ids: Vec<String>,
    pub attachment_count: u32,
    /// Latest run-item verdict for the case, `None` = never executed.
    pub last_result: Option<String>,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One entry of the append-only case version history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseVersionInfo {
    pub version: u32,
    pub change_note: String,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
}

/// Full case view: current content plus attachments and version history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestCaseDetail {
    pub info: TestCaseInfo,
    pub content_json: String,
    pub attachments: Vec<AttachmentWire>,
    pub versions: Vec<CaseVersionInfo>,
}

/// One rejected CSV row from the import (line numbers are 1-based).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsvImportError {
    pub line: u32,
    pub message: String,
}

/// Test suite summary with the latest run (if any).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteInfo {
    pub suite_id: String,
    pub name: String,
    pub description: String,
    pub case_count: u32,
    /// True when the suite still references deprecated cases.
    pub has_deprecated: bool,
    pub last_run: Option<TestRunInfo>,
    pub created_at: String,
    pub updated_at: String,
}

/// Ordered case reference inside a suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteCaseRef {
    pub case_id: String,
    pub position: u32,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub priority: String,
}

/// Test run header. Result counters are SQL aggregates over run items
/// (never denormalized), so a plain `RunGetRequest` poll is consistent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRunInfo {
    pub run_id: String,
    /// Human-friendly sequential number, unique per project.
    pub run_no: u32,
    pub name: String,
    pub suite_id: String,
    pub suite_name: String,
    /// 'manual' | 'auto' | 'perf'.
    pub run_type: String,
    /// Environment the run targets ('' for manual runs without one).
    pub environment_id: String,
    pub env_note: String,
    /// 'single' | 'per_case' | 'pool'.
    pub assignment_mode: String,
    /// 'running' | 'completed' | 'cancelled'.
    pub status: String,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub blocked: u32,
    pub skipped: u32,
    pub pending: u32,
    pub in_progress: u32,
    pub created_by: String,
    pub created_by_name: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    // F3 fields appended with #[serde(default)] so frames produced by F2
    // peers (which omit them) still decode — struct fields are append-only
    // on the wire, same as enum variants.
    #[serde(default)]
    pub environment_name: String,
    /// Runner service that executed the run ('' for manual runs).
    #[serde(default)]
    pub runner_service_id: String,
    /// Items that ended in an execution error (automated runs only).
    #[serde(default)]
    pub errored: u32,
    /// Aggregated perf summary JSON, present only for finished perf runs.
    #[serde(default)]
    pub perf_summary_json: Option<String>,
}

/// Per-case assignment sent by the client for 'per_case' runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunAssignmentWire {
    pub case_id: String,
    pub user_id: String,
}

/// One executable item of a run. The item pins `case_version` + `case_title`
/// at run creation, so later case edits never mutate a running execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunItemWire {
    pub item_id: String,
    pub run_id: String,
    pub case_id: String,
    pub case_title: String,
    pub case_version: u32,
    pub position: u32,
    /// '' = pool item (claimable by any tester).
    pub assigned_to: String,
    pub assigned_to_name: String,
    /// 'pending' | 'in_progress' | 'passed' | 'failed' | 'blocked' | 'skipped'.
    pub status: String,
    pub result_note: String,
    pub tester_config: String,
    pub duration_secs: u32,
    pub attachments: Vec<AttachmentWire>,
    pub steps_total: u32,
    pub steps_done: u32,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One step of a run item (action/expected are snapshots copied from the
/// case content at run creation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStepWire {
    pub step_index: u32,
    pub action: String,
    pub expected: String,
    pub status: String,
    pub note: String,
    pub attachments: Vec<AttachmentWire>,
}

/// Cross-project "my test work" aggregate for the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MyWorkEntry {
    pub project_id: String,
    pub project_name: String,
    pub run_id: String,
    pub run_no: u32,
    pub run_name: String,
    pub items_pending: u32,
    pub items_in_progress: u32,
}

/// Task / defect row for lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInfo {
    pub task_id: String,
    /// Human-friendly sequential number, unique per project.
    pub task_no: u32,
    /// 'task' | 'defect'.
    pub task_type: String,
    pub title: String,
    /// Defect severity ('' for plain tasks).
    pub severity: String,
    pub priority: String,
    /// 'todo' | 'in_progress' | 'review' | 'done'.
    pub status: String,
    pub assigned_to: String,
    pub assigned_to_name: String,
    pub due_date: String,
    /// JSON array of `{kind:'case'|'run'|'run_item'|'step', id, label}`.
    pub links_json: String,
    pub comment_count: u32,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Task comment with author display data resolved server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCommentWire {
    pub comment_id: String,
    pub author_user_id: String,
    pub author_name: String,
    pub body_md: String,
    pub created_at: String,
    pub edited_at: Option<String>,
}

/// Full task view for the detail panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub info: TaskInfo,
    pub description_md: String,
    pub attachments: Vec<AttachmentWire>,
    pub comments: Vec<TaskCommentWire>,
}

/// Agent generation run. Polling `GenerationGetRequest` every 2-4 s is the
/// source of truth; the agent-run event stream is only a live view for the
/// initiator (run-scope ACL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationRunInfo {
    pub gen_id: String,
    pub kind: String,
    /// 'running' | 'review' | 'accepted' | 'rejected' | 'failed' | 'cancelled'.
    pub status: String,
    pub agent_id: String,
    pub agent_name: String,
    pub agent_run_id: String,
    pub source_ids: Vec<String>,
    pub instructions: String,
    pub requested_count: u32,
    pub max_cases: u32,
    pub cases_generated: u32,
    pub cases_accepted: u32,
    pub cases_rejected: u32,
    pub error: Option<String>,
    pub started_by: String,
    pub started_by_name: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Personal notification row. Rows live in the central DB and every query is
/// filtered by the authenticated caller — the wire never exposes another
/// user's notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationWire {
    pub notification_id: String,
    pub project_id: String,
    pub project_name: String,
    /// 'run_item_assigned' | 'run_closed' | 'generation_finished' | 'task_assigned'.
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link_json: String,
    pub read_at: Option<String>,
    pub created_at: String,
}

/// Test environment (F3). `has_secret` only signals that an encrypted secret
/// is stored — the secret itself NEVER travels on the wire after save (the
/// save request carries it input-only, reads return this flag instead).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub environment_id: String,
    pub name: String,
    /// 'web' | 'api'.
    pub env_type: String,
    pub base_url: String,
    /// 'none' | 'bearer' | 'api_key' | 'basic'.
    pub auth_type: String,
    pub has_secret: bool,
    /// JSON object of extra request headers sent by the runner.
    pub extra_headers_json: String,
    /// Extra hosts the sandboxed run may reach besides `base_url`'s host.
    pub host_allowlist: Vec<String>,
    /// 'pending' | 'approved' | 'rejected' — private-address environments
    /// start 'pending' and need an explicit admin decision (reverse of the
    /// public-web SSRF guard: LAN targets require a human in the loop).
    pub approval_status: String,
    pub approval_reason: String,
    pub is_private_address: bool,
    pub requested_by: String,
    pub requested_by_name: String,
    pub decided_by: String,
    pub decided_by_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub decided_at: Option<String>,
}

/// One row of the admin "environments awaiting approval" queue. Cross-project
/// on purpose: admins decide without opening each project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvApprovalItem {
    pub project_id: String,
    pub project_name: String,
    pub environment: EnvironmentInfo,
    /// Requester's justification for a private-address environment.
    pub justification: String,
}

/// Build/test recipe for a code source (git/zip), one per source. Unit-test
/// runs execute `install_cmd` + `test_cmd` inside the sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildProfileWire {
    pub profile_id: String,
    pub source_id: String,
    /// 'python' | 'node' | 'dotnet' | 'jvm' | 'rust' | 'go'.
    pub toolchain: String,
    pub base_image: String,
    pub install_cmd: String,
    pub test_cmd: String,
    pub workdir: String,
    /// '' = user-authored, otherwise the agent that proposed the profile.
    pub proposed_by: String,
}

/// One toolchain advertised by a test-runner service (`GET /health`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerToolchain {
    pub language: String,
    pub frameworks: Vec<String>,
    pub version: String,
}

/// Discovered test-runner service with its advertised capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerInfo {
    pub service_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub endpoint_url: String,
    pub status: String,
    pub toolchains: Vec<RunnerToolchain>,
}

/// Reference to a stored run artifact; bytes download via
/// `RunArtifactGetRequest` (dashboard is binary-protocol only, no signed URL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub artifact_id: String,
    pub name: String,
    /// 'log' | 'screenshot' | 'trace' | 'junit' | 'perf_stats' | 'har' | 'other'.
    pub kind: String,
    pub size_bytes: u64,
    pub mime: String,
    pub download_ref: String,
}

/// One item of an automated run. Unlike manual `RunItemWire` there is no
/// assignee/steps snapshot — the runner executes the case content directly
/// and reports duration, message and produced artifacts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRunItemAutoWire {
    pub item_id: String,
    pub case_id: String,
    pub case_title: String,
    pub kind: String,
    pub language: String,
    pub position: u32,
    /// 'pending' | 'running' | 'passed' | 'failed' | 'blocked' | 'skipped' | 'error'.
    pub status: String,
    pub duration_ms: u64,
    pub message: String,
    pub artifact_refs: Vec<ArtifactRef>,
    pub steps_total: u32,
    pub steps_done: u32,
}

/// Per-endpoint aggregate of a perf run (Locust-style stats table row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfStatsWire {
    pub endpoint: String,
    pub requests: u64,
    pub failures: u64,
    pub rps: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

/// One sample of the perf-run timeline chart (sampled by the runner).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfTimelinePoint {
    pub ts_s: u64,
    pub rps: f64,
    pub p90_ms: f64,
    pub failures: u64,
    pub users: u32,
}

/// Scheduled run definition (F4). `next_run_at` and `next_runs_preview` are
/// computed SERVER-side from `schedule_kind`/`schedule_expr`/`timezone` — the
/// UI must never recompute them, otherwise the preview and the loop that
/// actually fires would disagree around DST transitions. `auto_disabled`
/// marks a schedule stopped by the failure breaker (resume is manual only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleInfo {
    pub schedule_id: String,
    pub name: String,
    pub enabled: bool,
    pub auto_disabled: bool,
    /// 'manual' | 'auto' | 'perf'.
    pub run_type: String,
    pub suite_id: String,
    pub suite_name: String,
    pub case_ids: Vec<String>,
    pub cases_count: u32,
    pub environment_id: String,
    pub environment_name: String,
    /// Approval state of the bound environment ('' when none is bound). A
    /// schedule whose environment is not 'approved' is blocked at fire time.
    pub environment_status: String,
    pub runner_service_id: String,
    pub runner_display_name: String,
    pub perf_profile_json: String,
    /// 'single' | 'per_case' | 'pool' (manual runs only).
    pub assignment_mode: String,
    pub assignees: Vec<String>,
    /// 'once' | 'interval' | 'cron'.
    pub schedule_kind: String,
    /// RFC3339 instant ('once'), duration like '30m'/'1h'/'1d' ('interval')
    /// or a daily 'minute hour * * *' expression ('cron').
    pub schedule_expr: String,
    /// IANA timezone name the cron expression is evaluated in.
    pub timezone: String,
    pub next_run_at: String,
    /// Next three fire instants, already rendered by the server.
    pub next_runs_preview: Vec<String>,
    pub last_trigger_at: String,
    pub last_run_id: String,
    pub last_run_no: u32,
    /// Last run status, or the trigger outcome when nothing started.
    pub last_status: String,
    pub last_reason: String,
    pub consecutive_failures: u32,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One trigger attempt of a schedule. Attempts that did NOT start a run are
/// recorded too ('skipped' / 'blocked' / 'error' with a reason), so an admin
/// can tell "never fired" from "fired and refused".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRunWire {
    pub trigger_id: String,
    pub scheduled_for: String,
    pub fired_at: String,
    /// 'started' | 'skipped' | 'blocked' | 'error'.
    pub outcome: String,
    pub reason: String,
    pub run_id: String,
    pub run_no: u32,
    pub run_status: String,
    /// '' for loop-fired triggers, the user id for "run now".
    pub actor: String,
    pub actor_name: String,
}

/// One row of the project-role → ML-Studio-role mapping. ML Studio only knows
/// 'editor' and 'viewer', so the five project roles collapse onto two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlRoleMapEntry {
    /// 'owner' | 'manager' | 'editor' | 'tester' | 'viewer'.
    pub project_role: String,
    /// 'editor' | 'viewer'.
    pub ml_role: String,
}

/// Read-only ML Studio project snapshot rendered on the project card. Training
/// details are flattened (no nested struct) because the card shows them as
/// plain fields and a nested optional would only add a wire level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlProjectSummaryWire {
    pub ml_project_id: String,
    pub name: String,
    pub project_type: String,
    pub project_type_label: String,
    pub status: String,
    pub dataset_count: u32,
    pub model_count: u32,
    /// Model display names, capped server-side for the chip row.
    pub models: Vec<String>,
    pub last_training_run_id: String,
    pub last_training_status: String,
    pub last_training_started_at: String,
    pub last_training_finished_at: String,
    pub last_training_metrics_json: String,
    pub training_in_progress: bool,
    /// Dashboard route opening this project in ML Studio.
    pub deep_link: String,
}

/// Link between a Project Studio project and an ML Studio project.
/// `summary` is `None` when the ML project is gone or unreadable;
/// `can_open` reflects the caller's ML membership, NOT the project role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlLinkInfo {
    pub link_id: String,
    pub ml_project_id: String,
    pub label: String,
    /// 'created_from_project' | 'linked_existing'.
    pub origin: String,
    pub sync_permissions: bool,
    pub role_map: Vec<MlRoleMapEntry>,
    pub last_sync_at: String,
    pub last_sync_result: String,
    pub created_by: String,
    pub created_by_name: String,
    pub created_at: String,
    pub summary: Option<MlProjectSummaryWire>,
    pub can_open: bool,
}

/// Result of one permission-sync pass over a link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlSyncOutcomeWire {
    pub applied_add: u32,
    pub applied_update: u32,
    pub applied_remove: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

/// Content census of an export archive. Shown before an import so the user
/// decides with the real sizes in hand; `vector_dim`/`embedding_*` carry the
/// fingerprint that decides whether the vector file can be reused verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveInventoryWire {
    pub cases: u32,
    pub suites: u32,
    pub runs: u32,
    pub tasks: u32,
    pub documents: u32,
    pub sources: u32,
    pub files: u32,
    pub bytes_files: u64,
    pub bytes_runs: u64,
    pub vectors: u64,
    pub vector_dim: u32,
    pub embedding_alias: String,
    /// Model the alias resolved to when the archive was written.
    pub embedding_model: String,
}

/// Project Studio message family (request + response + stream). ciborium
/// encodes variants external-tagged by variant NAME, so never rename variants
/// or fields without updating the frontend and the golden test
/// (`project_studio_wire_golden`). Variant order is the wire contract:
/// append-only, never insert or reorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectStudioPayload {
    // ---- Registry (P01, P02) ----
    ProjectsListRequest {
        #[serde(default)]
        include_archived: bool,
    },
    ProjectsListResponse {
        projects: Vec<ProjectInfo>,
        can_create: bool,
    },
    ProjectCreateRequest {
        name: String,
        description: String,
        template: String,
        modules: Vec<String>,
        members: Vec<MemberInputWire>,
    },
    ProjectCreateResponse {
        project_id: String,
    },
    ProjectGetRequest {
        project_id: String,
    },
    ProjectGetResponse {
        project: ProjectInfo,
    },
    ProjectUpdateRequest {
        project_id: String,
        name: String,
        description: String,
    },
    ProjectUpdateResult {
        ok: bool,
    },
    ProjectArchiveRequest {
        project_id: String,
        archived: bool,
    },
    ProjectArchiveResult {
        ok: bool,
    },
    ProjectDeleteRequest {
        project_id: String,
    },
    ProjectDeleteResult {
        ok: bool,
    },
    // ---- Members (X03, P02 step 3) ----
    MembersListRequest {
        project_id: String,
    },
    MembersListResponse {
        members: Vec<MemberInfo>,
    },
    /// `project_id: None` = creation wizard (grant holders), `Some` = the
    /// "Invite" modal (manager+); candidates exclude existing members.
    MemberCandidatesRequest {
        project_id: Option<String>,
        query: String,
        limit: u32,
    },
    MemberCandidatesResponse {
        users: Vec<UserRefWire>,
    },
    MembersAddRequest {
        project_id: String,
        members: Vec<MemberInputWire>,
    },
    MembersAddResponse {
        added: u32,
    },
    MemberRoleSetRequest {
        project_id: String,
        user_id: String,
        role: String,
    },
    MemberRoleSetResult {
        ok: bool,
    },
    MemberRemoveRequest {
        project_id: String,
        user_id: String,
    },
    MemberRemoveResult {
        ok: bool,
    },
    OwnershipTransferRequest {
        project_id: String,
        new_owner_user_id: String,
    },
    OwnershipTransferResult {
        ok: bool,
    },
    // ---- Creator grants (admin) ----
    CreatorGrantsListRequest,
    CreatorGrantsListResponse {
        grants: Vec<CreatorGrantInfo>,
    },
    CreatorGrantSetRequest {
        user_id: String,
        granted: bool,
    },
    CreatorGrantSetResult {
        ok: bool,
    },
    // ---- Knowledge: sources + upload + ingest (W01, W02) ----
    SourcesListRequest {
        project_id: String,
    },
    SourcesListResponse {
        sources: Vec<SourceInfo>,
    },
    SourceUploadChunkRequest {
        project_id: String,
        upload_id: String,
        filename: String,
        mime: String,
        seq: u32,
        total_chunks: u32,
        /// Raw chunk bytes. `serde_bytes` forces a CBOR byte-string
        /// (length-prefixed) — a bare `Vec<u8>` would encode as an
        /// array-of-integers (~2x the size), unacceptable for file uploads.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    /// `file_ref` = content sha256, present only after the last chunk.
    SourceUploadChunkResponse {
        upload_id: String,
        received_chunks: u32,
        received_bytes: u64,
        file_ref: Option<String>,
    },
    /// Creates the source and starts an ingest job immediately.
    SourceCreateRequest {
        project_id: String,
        kind: String,
        name: String,
        config_json: String,
        #[serde(default)]
        file_refs: Vec<String>,
    },
    SourceCreateResponse {
        source_id: String,
        job_id: String,
    },
    SourceUpdateRequest {
        project_id: String,
        source_id: String,
        name: String,
        config_json: String,
    },
    SourceUpdateResponse {
        source_id: String,
        job_id: Option<String>,
    },
    SourceDeleteRequest {
        project_id: String,
        source_id: String,
    },
    SourceDeleteResult {
        ok: bool,
    },
    SourceReingestRequest {
        project_id: String,
        source_id: String,
        #[serde(default)]
        file_id: Option<String>,
    },
    SourceReingestResponse {
        job_id: String,
    },
    IngestCancelRequest {
        project_id: String,
        job_id: String,
    },
    IngestCancelResult {
        ok: bool,
    },
    /// Polling every 2-4 s is the source of truth for job state.
    IngestStatusRequest {
        project_id: String,
        job_id: String,
    },
    IngestStatusResponse {
        job: IngestJobWire,
    },
    // ---- Source files (W01/W04) ----
    SourceFilesListRequest {
        project_id: String,
        source_id: String,
        offset: u32,
        limit: u32,
        #[serde(default)]
        filter: String,
    },
    SourceFilesListResponse {
        files: Vec<SourceFileInfo>,
        total: u32,
    },
    SourceFileDeleteRequest {
        project_id: String,
        file_id: String,
    },
    SourceFileDeleteResult {
        ok: bool,
    },
    /// Server clamps `max_bytes` (e.g. 256 KiB); text-only preview.
    SourceFilePreviewRequest {
        project_id: String,
        file_id: String,
        max_bytes: u32,
    },
    SourceFilePreviewResponse {
        content: String,
        truncated: bool,
        mime: String,
    },
    // ---- Knowledge-base search (W03) ----
    KbSearchRequest {
        project_id: String,
        query: String,
        #[serde(default)]
        source_ids: Vec<String>,
        limit: u32,
    },
    KbSearchResponse {
        hits: Vec<KbHit>,
    },
    // ---- Overview + activity (P03) ----
    OverviewRequest {
        project_id: String,
    },
    OverviewResponse {
        kpis: OverviewKpis,
        activity: Vec<ActivityEntry>,
    },
    ActivityListRequest {
        project_id: String,
        #[serde(default)]
        before_id: Option<i64>,
        limit: u32,
    },
    ActivityListResponse {
        entries: Vec<ActivityEntry>,
        has_more: bool,
    },
    // ---- Chat (C01) — always filtered to the caller's own chats ----
    ChatsListRequest {
        project_id: String,
    },
    ChatsListResponse {
        chats: Vec<ChatInfo>,
    },
    ChatCreateRequest {
        project_id: String,
        title: String,
    },
    ChatCreateResponse {
        chat: ChatInfo,
    },
    ChatRenameRequest {
        project_id: String,
        chat_id: String,
        title: String,
    },
    ChatRenameResult {
        ok: bool,
    },
    ChatDeleteRequest {
        project_id: String,
        chat_id: String,
    },
    ChatDeleteResult {
        ok: bool,
    },
    ChatHistoryRequest {
        project_id: String,
        chat_id: String,
        #[serde(default)]
        before_message_id: Option<String>,
        limit: u32,
    },
    ChatHistoryResponse {
        messages: Vec<ChatMessageWire>,
        has_more: bool,
    },
    // ---- Settings (X04) ----
    SettingsGetRequest {
        project_id: String,
    },
    SettingsGetResponse {
        settings: ProjectSettings,
    },
    SettingsSaveRequest {
        project_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        agents_json: Option<String>,
        /// Full replacement list of enabled modules. `None` leaves the current
        /// set untouched, `Some` REPLACES it — a partial diff would make the
        /// "turn a module off" case impossible to express.
        #[serde(default)]
        modules: Option<Vec<String>>,
        /// Knowledge-graph extraction during ingest. `None` leaves the current
        /// value alone, so the basics/modules/agents saves that share this
        /// request cannot flip it by omission.
        #[serde(default)]
        graph_extraction: Option<bool>,
    },
    SettingsSaveResult {
        ok: bool,
    },
    TagSaveRequest {
        project_id: String,
        #[serde(default)]
        tag_id: Option<String>,
        name: String,
    },
    TagSaveResponse {
        tag_id: String,
    },
    TagDeleteRequest {
        project_id: String,
        tag_id: String,
    },
    TagDeleteResult {
        ok: bool,
    },
    // ---- Streaming (dispatch/stream_handlers.rs) ----
    IngestStreamRequest {
        project_id: String,
        job_id: String,
    },
    IngestStreamChunk {
        job_id: String,
        /// "log" | "phase" | "progress" | "file".
        kind: String,
        phase: String,
        line: String,
        progress_pct: u32,
        ts_ms: i64,
    },
    IngestStreamEnd {
        job_id: String,
        status: String,
        error: Option<String>,
    },
    ChatStreamRequest {
        project_id: String,
        chat_id: String,
        message: String,
    },
    ChatStreamChunk {
        chat_id: String,
        /// "token" | "citations" | "status".
        kind: String,
        text: String,
        citations_json: String,
    },
    ChatStreamEnd {
        chat_id: String,
        status: String,
        error: Option<String>,
        message_id: String,
    },
    // ---- F2: manual test cases (T01, T02) ----
    /// Cases with `review_state == 'pending'` are ALWAYS excluded
    /// server-side — agent output surfaces only via GenerationGetResponse
    /// until it passes review.
    CasesListRequest {
        project_id: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        priority: String,
        #[serde(default)]
        tag_id: String,
        #[serde(default)]
        origin: String,
        #[serde(default)]
        search: String,
        offset: u32,
        limit: u32,
    },
    CasesListResponse {
        cases: Vec<TestCaseInfo>,
        total: u32,
    },
    CaseGetRequest {
        project_id: String,
        case_id: String,
        #[serde(default)]
        include_versions: bool,
    },
    CaseGetResponse {
        detail: TestCaseDetail,
    },
    /// `case_id: None` = create. `expected_version` drives optimistic
    /// locking on edit: a stale version yields a conflict error, the client
    /// reloads instead of silently overwriting. A new version is recorded
    /// only when `content_json` actually changed.
    CaseSaveRequest {
        project_id: String,
        #[serde(default)]
        case_id: Option<String>,
        kind: String,
        title: String,
        priority: String,
        content_json: String,
        tag_ids: Vec<String>,
        linked_source_ids: Vec<String>,
        attachments_json: String,
        #[serde(default)]
        expected_version: Option<u32>,
        #[serde(default)]
        change_note: String,
    },
    CaseSaveResponse {
        case_id: String,
        version: u32,
    },
    /// Every status downgrade requires `reason`.
    CaseStatusSetRequest {
        project_id: String,
        case_id: String,
        status: String,
        #[serde(default)]
        reason: String,
    },
    CaseStatusSetResult {
        ok: bool,
    },
    CasesBulkStatusRequest {
        project_id: String,
        case_ids: Vec<String>,
        status: String,
        #[serde(default)]
        reason: String,
    },
    CasesBulkStatusResponse {
        updated: u32,
    },
    CaseDuplicateRequest {
        project_id: String,
        case_id: String,
    },
    CaseDuplicateResponse {
        case_id: String,
    },
    CaseDeleteRequest {
        project_id: String,
        case_id: String,
    },
    CaseDeleteResult {
        ok: bool,
    },
    CaseVersionGetRequest {
        project_id: String,
        case_id: String,
        version: u32,
    },
    CaseVersionGetResponse {
        content_json: String,
        change_note: String,
        created_by_name: String,
        created_at: String,
    },
    /// Restore creates a NEW version carrying the old content — history is
    /// append-only, nothing is rewritten, running executions keep their
    /// pinned snapshots.
    CaseRestoreVersionRequest {
        project_id: String,
        case_id: String,
        version: u32,
        expected_version: u32,
    },
    CaseRestoreVersionResponse {
        case_id: String,
        version: u32,
    },
    /// Server-side parsing with hard clamps (2 MiB / 500 rows); the import
    /// transaction is all-or-nothing, `dry_run` only validates.
    CasesImportCsvRequest {
        project_id: String,
        csv_text: String,
        #[serde(default)]
        dry_run: bool,
    },
    CasesImportCsvResponse {
        created: u32,
        errors: Vec<CsvImportError>,
    },
    /// Attachment download by content hash (server clamps `max_bytes` to
    /// 8 MiB); upload reuses the existing SourceUploadChunkRequest.
    AttachmentGetRequest {
        project_id: String,
        sha256: String,
        max_bytes: u32,
    },
    AttachmentGetResponse {
        /// Raw attachment bytes. `serde_bytes` forces a CBOR byte-string
        /// (length-prefixed) — a bare `Vec<u8>` would encode as an
        /// array-of-integers (~2x the size), unacceptable for downloads.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        mime: String,
        truncated: bool,
    },
    // ---- F2: test suites (T04) ----
    SuitesListRequest {
        project_id: String,
    },
    SuitesListResponse {
        suites: Vec<SuiteInfo>,
    },
    SuiteGetRequest {
        project_id: String,
        suite_id: String,
    },
    SuiteGetResponse {
        suite: SuiteInfo,
        cases: Vec<SuiteCaseRef>,
    },
    /// `suite_id: None` = create; `case_ids` order defines member positions.
    SuiteSaveRequest {
        project_id: String,
        #[serde(default)]
        suite_id: Option<String>,
        name: String,
        description: String,
        case_ids: Vec<String>,
    },
    SuiteSaveResponse {
        suite_id: String,
    },
    SuiteDeleteRequest {
        project_id: String,
        suite_id: String,
    },
    SuiteDeleteResult {
        ok: bool,
    },
    // ---- F2: test runs + execution (T06, T07, T08, T09) ----
    RunsListRequest {
        project_id: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        run_type: String,
        offset: u32,
        limit: u32,
    },
    RunsListResponse {
        runs: Vec<TestRunInfo>,
        total: u32,
    },
    /// Exactly ONE of `suite_id` / `case_ids` / `from_failed_run_id` selects
    /// the case source; the server rejects any other combination. Items
    /// snapshot case version, title and steps at creation.
    RunCreateRequest {
        project_id: String,
        name: String,
        #[serde(default)]
        suite_id: String,
        #[serde(default)]
        case_ids: Vec<String>,
        #[serde(default)]
        from_failed_run_id: String,
        #[serde(default)]
        env_note: String,
        assignment_mode: String,
        #[serde(default)]
        single_assignee: String,
        #[serde(default)]
        assignments: Vec<RunAssignmentWire>,
    },
    RunCreateResponse {
        run_id: String,
        run_no: u32,
    },
    RunGetRequest {
        project_id: String,
        run_id: String,
    },
    RunGetResponse {
        run: TestRunInfo,
        items: Vec<RunItemWire>,
    },
    RunCloseRequest {
        project_id: String,
        run_id: String,
        #[serde(default)]
        cancelled: bool,
    },
    RunCloseResult {
        ok: bool,
    },
    RunDeleteRequest {
        project_id: String,
        run_id: String,
    },
    RunDeleteResult {
        ok: bool,
    },
    /// `item_id: None` claims the nearest pool item — a single atomic
    /// UPDATE…RETURNING server-side, so two testers never claim the same
    /// item (no select-then-update race).
    RunItemClaimRequest {
        project_id: String,
        run_id: String,
        #[serde(default)]
        item_id: Option<String>,
    },
    /// `item: None` = nothing left to claim in this run.
    RunItemClaimResponse {
        item: Option<RunItemWire>,
    },
    RunItemReleaseRequest {
        project_id: String,
        item_id: String,
    },
    RunItemReleaseResult {
        ok: bool,
    },
    RunItemGetRequest {
        project_id: String,
        item_id: String,
    },
    RunItemGetResponse {
        item: RunItemWire,
        steps: Vec<RunStepWire>,
        preconditions: String,
        test_data: String,
    },
    RunStepSetRequest {
        project_id: String,
        item_id: String,
        step_index: u32,
        status: String,
        #[serde(default)]
        note: String,
        #[serde(default)]
        attachments_json: String,
    },
    RunStepSetResult {
        ok: bool,
    },
    /// Empty `status` = server derives the verdict from step results
    /// (fail > blocked > skip > pass); an explicit override requires
    /// `result_note`. `next_item` lets the tester chain into the next
    /// claimable item without a separate claim round-trip.
    RunItemFinishRequest {
        project_id: String,
        item_id: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        result_note: String,
        #[serde(default)]
        tester_config: String,
        duration_secs: u32,
        #[serde(default)]
        attachments_json: String,
    },
    RunItemFinishResponse {
        ok: bool,
        next_item: Option<RunItemWire>,
    },
    /// Cross-project: aggregates the caller's open run items over all active
    /// projects with the tests module enabled — hence no `project_id`.
    MyTestWorkRequest,
    MyTestWorkResponse {
        entries: Vec<MyWorkEntry>,
    },
    // ---- F2: tasks + defects (Z01, Z02) ----
    TasksListRequest {
        project_id: String,
        #[serde(default)]
        task_type: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        assigned_to: String,
        #[serde(default)]
        search: String,
        offset: u32,
        limit: u32,
        /// Defect severity filter ('' = any). Appended after F4, so older
        /// peers that omit it still decode.
        #[serde(default)]
        severity: String,
    },
    TasksListResponse {
        tasks: Vec<TaskInfo>,
        total: u32,
    },
    TaskGetRequest {
        project_id: String,
        task_id: String,
    },
    TaskGetResponse {
        detail: TaskDetail,
    },
    /// `task_id: None` = create; defects require `severity`.
    TaskSaveRequest {
        project_id: String,
        #[serde(default)]
        task_id: Option<String>,
        task_type: String,
        title: String,
        description_md: String,
        #[serde(default)]
        severity: String,
        priority: String,
        status: String,
        #[serde(default)]
        assigned_to: String,
        #[serde(default)]
        due_date: String,
        #[serde(default)]
        links_json: String,
        #[serde(default)]
        attachments_json: String,
    },
    TaskSaveResponse {
        task_id: String,
        task_no: u32,
    },
    TaskDeleteRequest {
        project_id: String,
        task_id: String,
    },
    TaskDeleteResult {
        ok: bool,
    },
    TaskCommentAddRequest {
        project_id: String,
        task_id: String,
        body_md: String,
    },
    TaskCommentAddResponse {
        comment: TaskCommentWire,
    },
    TaskCommentEditRequest {
        project_id: String,
        comment_id: String,
        body_md: String,
    },
    TaskCommentEditResult {
        ok: bool,
    },
    TaskCommentDeleteRequest {
        project_id: String,
        comment_id: String,
    },
    TaskCommentDeleteResult {
        ok: bool,
    },
    // ---- F2: agent case generation (G01/T05) ----
    /// `requested_count` 0 = server default (10); `agent_id: None` = the
    /// project's 'generator_manual' binding (seeded system agent fallback).
    GenerationStartRequest {
        project_id: String,
        kind: String,
        source_ids: Vec<String>,
        #[serde(default)]
        requested_count: u32,
        #[serde(default)]
        instructions: String,
        #[serde(default)]
        agent_id: Option<String>,
    },
    GenerationStartResponse {
        gen_id: String,
        agent_run_id: String,
    },
    GenerationsListRequest {
        project_id: String,
    },
    GenerationsListResponse {
        generations: Vec<GenerationRunInfo>,
    },
    /// Polling every 2-4 s is the source of truth for generation progress;
    /// the agent-run event stream is only a live view for the initiator.
    GenerationGetRequest {
        project_id: String,
        gen_id: String,
    },
    GenerationGetResponse {
        run: GenerationRunInfo,
        pending_cases: Vec<TestCaseInfo>,
    },
    GenerationCancelRequest {
        project_id: String,
        gen_id: String,
    },
    GenerationCancelResult {
        ok: bool,
    },
    GenerationReviewRequest {
        project_id: String,
        gen_id: String,
        #[serde(default)]
        accept_case_ids: Vec<String>,
        #[serde(default)]
        reject_case_ids: Vec<String>,
    },
    GenerationReviewResponse {
        accepted: u32,
        rejected: u32,
        run_status: String,
    },
    GenerationDeleteRequest {
        project_id: String,
        gen_id: String,
    },
    GenerationDeleteResult {
        ok: bool,
    },
    // ---- F2: notifications (G02) — central DB, always caller-scoped ----
    NotificationsListRequest {
        #[serde(default)]
        only_unread: bool,
        #[serde(default)]
        before_id: Option<String>,
        limit: u32,
    },
    NotificationsListResponse {
        notifications: Vec<NotificationWire>,
        unread_count: u32,
        has_more: bool,
    },
    /// Empty `notification_ids` = mark ALL of the caller's as read.
    NotificationsMarkReadRequest {
        #[serde(default)]
        notification_ids: Vec<String>,
    },
    NotificationsMarkReadResult {
        ok: bool,
    },
    // ---- F2: reports (T14) ----
    /// One generic report variant on purpose: later phases add report kinds
    /// ('runs_over_time' | 'suite_pass_rate' | 'tester_stats' |
    /// 'source_coverage' | 'defects' in F2) without touching the wire —
    /// `rows_json` schema is per report.
    ReportQueryRequest {
        project_id: String,
        report: String,
        #[serde(default)]
        from_date: String,
        #[serde(default)]
        to_date: String,
        #[serde(default)]
        suite_id: String,
        /// F4: explicit run selection ('perf_compare' takes exactly two).
        #[serde(default)]
        run_ids: Vec<String>,
    },
    ReportQueryResponse {
        rows_json: String,
    },
    // ---- F3: test environments (T12) ----
    EnvironmentsListRequest {
        project_id: String,
    },
    EnvironmentsListResponse {
        environments: Vec<EnvironmentInfo>,
    },
    /// `environment_id: None` = create. `secret` is INPUT-ONLY: `None` keeps
    /// the stored secret, `Some("")` clears it, `Some(v)` replaces it — reads
    /// only ever expose `has_secret`. A public `base_url` is auto-approved,
    /// a private/LAN one goes 'pending' until an admin decides; changing the
    /// address class resets the approval.
    EnvironmentSaveRequest {
        project_id: String,
        #[serde(default)]
        environment_id: Option<String>,
        name: String,
        env_type: String,
        base_url: String,
        auth_type: String,
        #[serde(default)]
        secret: Option<String>,
        #[serde(default)]
        extra_headers_json: String,
        #[serde(default)]
        host_allowlist: Vec<String>,
        #[serde(default)]
        justification: String,
    },
    EnvironmentSaveResponse {
        environment_id: String,
        approval_status: String,
    },
    EnvironmentDeleteRequest {
        project_id: String,
        environment_id: String,
    },
    EnvironmentDeleteResult {
        ok: bool,
    },
    /// Admin-only, cross-project: every environment awaiting approval.
    EnvApprovalsListRequest,
    EnvApprovalsListResponse {
        items: Vec<EnvApprovalItem>,
    },
    /// Rejection requires a non-empty `reason`.
    EnvApprovalDecideRequest {
        project_id: String,
        environment_id: String,
        approve: bool,
        #[serde(default)]
        reason: String,
    },
    EnvApprovalDecideResult {
        ok: bool,
    },
    // ---- F3: build profiles (unit tests over git/zip sources) ----
    BuildProfileGetRequest {
        project_id: String,
        source_id: String,
    },
    BuildProfileGetResponse {
        profile: Option<BuildProfileWire>,
    },
    /// Upserts the single profile of a source (source_id is UNIQUE).
    BuildProfileSaveRequest {
        project_id: String,
        source_id: String,
        toolchain: String,
        #[serde(default)]
        base_image: String,
        #[serde(default)]
        install_cmd: String,
        test_cmd: String,
        #[serde(default)]
        workdir: String,
    },
    BuildProfileSaveResponse {
        profile_id: String,
    },
    // ---- F3: runner discovery ----
    RunnersListRequest {
        project_id: String,
    },
    RunnersListResponse {
        runners: Vec<RunnerInfo>,
    },
    // ---- F3: automated runs (T10, T11) ----
    /// Exactly ONE of `suite_id` / `case_ids` / `from_run_id` selects the
    /// case source (same contract as RunCreateRequest). `environment_id`
    /// MUST reference an approved environment; empty `runner_service_id`
    /// lets the server match a runner by toolchain.
    RunStartAutoRequest {
        project_id: String,
        name: String,
        #[serde(default)]
        suite_id: String,
        #[serde(default)]
        case_ids: Vec<String>,
        #[serde(default)]
        from_run_id: String,
        environment_id: String,
        #[serde(default)]
        runner_service_id: String,
        #[serde(default)]
        perf_profile_json: String,
    },
    RunStartAutoResponse {
        run_id: String,
        run_no: u32,
    },
    /// Polling this every 2-4 s is the source of truth for automated-run
    /// progress; RunAutoStream is only a live view.
    RunAutoGetRequest {
        project_id: String,
        run_id: String,
    },
    RunAutoGetResponse {
        run: TestRunInfo,
        items: Vec<TestRunItemAutoWire>,
        perf_stats: Vec<PerfStatsWire>,
        perf_timeline: Vec<PerfTimelinePoint>,
    },
    RunAutoCancelRequest {
        project_id: String,
        run_id: String,
    },
    RunAutoCancelResult {
        ok: bool,
    },
    // ---- F3: try-run (T03 "Uruchom próbnie") ----
    /// Stream-initiating request (no plain response): the server executes the
    /// case ephemerally — nothing is persisted as a run — and streams
    /// TryRunStreamChunk/End back on the same subscription. `try_id` is
    /// client-minted (like upload_id) so TryRunCancelRequest can address the
    /// execution without a response round-trip.
    TryRunStartRequest {
        project_id: String,
        try_id: String,
        case_id: String,
        environment_id: String,
        /// Unsaved editor content; '' = run the saved case content.
        #[serde(default)]
        content_json_override: String,
        #[serde(default)]
        language: String,
        #[serde(default)]
        perf_profile_json: String,
    },
    TryRunCancelRequest {
        project_id: String,
        try_id: String,
    },
    TryRunCancelResult {
        ok: bool,
    },
    // ---- F3: git/zip/api_spec source operations (W01, W02) ----
    /// Git sources only: fetch + delta re-index of changed files.
    SourceRefreshRequest {
        project_id: String,
        source_id: String,
    },
    SourceRefreshResponse {
        job_id: String,
    },
    /// Parsed endpoint list of an api_spec source (JSON array).
    ApiSpecEndpointsRequest {
        project_id: String,
        source_id: String,
    },
    ApiSpecEndpointsResponse {
        endpoints_json: String,
    },
    /// Sets/clears the access token of a git source. INPUT-ONLY like the
    /// environment secret: `None` clears, reads never return it.
    SourceSecretSetRequest {
        project_id: String,
        source_id: String,
        #[serde(default)]
        token: Option<String>,
    },
    SourceSecretSetResult {
        ok: bool,
    },
    // ---- F3: run artifacts ----
    /// Artifact download by id (server clamps `max_bytes` to 32 MiB). No
    /// signed URLs on purpose — the dashboard talks binary protocol only.
    RunArtifactGetRequest {
        project_id: String,
        artifact_id: String,
        max_bytes: u32,
    },
    RunArtifactGetResponse {
        /// Raw artifact bytes. `serde_bytes` forces a CBOR byte-string
        /// (length-prefixed) — a bare `Vec<u8>` would encode as an
        /// array-of-integers (~2x the size), unacceptable for downloads.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        mime: String,
        truncated: bool,
    },
    // ---- F3: streaming (dispatch/stream_handlers.rs) ----
    RunAutoStreamRequest {
        project_id: String,
        run_id: String,
    },
    RunAutoStreamChunk {
        run_id: String,
        /// "log" | "item" | "phase" | "perf" | "artifact".
        kind: String,
        #[serde(default)]
        line: String,
        #[serde(default)]
        phase: String,
        /// Incremental item snapshot (kind "item").
        #[serde(default)]
        item: Option<TestRunItemAutoWire>,
        /// Perf summary/timeline delta as JSON (kind "perf").
        #[serde(default)]
        perf_json: String,
        /// Newly produced artifact (kind "artifact").
        #[serde(default)]
        artifact: Option<ArtifactRef>,
        ts_ms: i64,
    },
    RunAutoStreamEnd {
        run_id: String,
        status: String,
        error: Option<String>,
    },
    TryRunStreamChunk {
        try_id: String,
        /// "log" | "phase" | "status".
        kind: String,
        #[serde(default)]
        line: String,
        #[serde(default)]
        phase: String,
        ts_ms: i64,
    },
    TryRunStreamEnd {
        try_id: String,
        status: String,
        error: Option<String>,
        /// Parsed junit result summary (JSON) when the runner produced one.
        #[serde(default)]
        junit_summary_json: String,
    },
    // ---- F3: code assist (T03, tf-code-editor) ----
    /// Stream-initiating request routed through the project's
    /// `generator_<kind>` agent (AiGateway audit + RAG context) — never a raw
    /// chat completion. Tokens stream via CodeAssistStreamChunk; the final
    /// proposal arrives whole in CodeAssistStreamEnd for the diff view.
    CodeAssistRequest {
        project_id: String,
        case_id: String,
        kind: String,
        /// Selected editor fragment ('' = whole script).
        #[serde(default)]
        selection: String,
        instruction: String,
        full_content: String,
    },
    CodeAssistStreamChunk {
        token: String,
    },
    CodeAssistStreamEnd {
        proposal: String,
        error: Option<String>,
    },
    // ---- F4: run schedules (T13) ----
    SchedulesListRequest {
        project_id: String,
    },
    /// `server_timezone` is the node's IANA zone: the UI renders "next run"
    /// hints next to it so a user in another zone reads the same instant the
    /// scheduling loop will use.
    SchedulesListResponse {
        schedules: Vec<ScheduleInfo>,
        server_timezone: String,
    },
    /// `schedule_id: None` = create. Carries the COMPLETE definition — every
    /// omitted field is a real clear, so the toggle in the list row must not
    /// reuse this variant (see ScheduleSetEnabledRequest).
    ScheduleSaveRequest {
        project_id: String,
        #[serde(default)]
        schedule_id: Option<String>,
        name: String,
        run_type: String,
        #[serde(default)]
        suite_id: String,
        #[serde(default)]
        case_ids: Vec<String>,
        #[serde(default)]
        environment_id: String,
        #[serde(default)]
        runner_service_id: String,
        #[serde(default)]
        perf_profile_json: String,
        #[serde(default)]
        assignment_mode: String,
        #[serde(default)]
        assignees: Vec<String>,
        schedule_kind: String,
        schedule_expr: String,
        #[serde(default)]
        timezone: String,
        enabled: bool,
    },
    ScheduleSaveResponse {
        schedule_id: String,
        next_run_at: String,
        next_runs_preview: Vec<String>,
    },
    ScheduleDeleteRequest {
        project_id: String,
        schedule_id: String,
    },
    ScheduleDeleteResult {
        ok: bool,
    },
    /// Enable/disable toggle from the list row. Separate from ScheduleSave on
    /// purpose: the row does not hold the full definition, so saving through
    /// ScheduleSaveRequest would clear cases, assignees and the perf profile.
    ScheduleSetEnabledRequest {
        project_id: String,
        schedule_id: String,
        enabled: bool,
    },
    /// Re-enabling recomputes the next fire instant and clears the breaker,
    /// so the server returns the fresh schedule state instead of a bare ok.
    ScheduleSetEnabledResult {
        ok: bool,
        next_run_at: String,
        auto_disabled: bool,
    },
    /// Fires a schedule immediately through the same gate chain as the loop;
    /// it never moves `next_run_at`.
    ScheduleRunNowRequest {
        project_id: String,
        schedule_id: String,
    },
    ScheduleRunNowResponse {
        /// 'started' | 'skipped' | 'blocked' | 'error'.
        outcome: String,
        reason: String,
        run_id: String,
        run_no: u32,
    },
    ScheduleRunsListRequest {
        project_id: String,
        schedule_id: String,
        #[serde(default)]
        limit: u32,
    },
    ScheduleRunsListResponse {
        runs: Vec<ScheduleRunWire>,
    },
    // ---- F4: ML Studio links (X02) ----
    MlLinksListRequest {
        project_id: String,
    },
    /// Summaries are returned to every project viewer; `can_manage` gates the
    /// create/attach/detach actions, `MlLinkInfo::can_open` gates the deep link.
    MlLinksListResponse {
        links: Vec<MlLinkInfo>,
        can_manage: bool,
    },
    MlProjectCreateFromProjectRequest {
        project_id: String,
        ml_name: String,
        project_type: String,
        role_map: Vec<MlRoleMapEntry>,
        sync_permissions: bool,
        #[serde(default)]
        label: String,
    },
    MlProjectCreateFromProjectResponse {
        link_id: String,
        ml_project_id: String,
        members_mapped: u32,
        members_skipped: u32,
    },
    /// ML projects the caller OWNS and that are not linked yet — attaching
    /// requires ownership, since the sync writes through owner-only calls.
    MlProjectCandidatesRequest {
        project_id: String,
    },
    MlProjectCandidatesResponse {
        candidates: Vec<MlProjectSummaryWire>,
    },
    MlLinkAttachRequest {
        project_id: String,
        ml_project_id: String,
        #[serde(default)]
        label: String,
        sync_permissions: bool,
        #[serde(default)]
        role_map: Vec<MlRoleMapEntry>,
    },
    MlLinkAttachResponse {
        link_id: String,
    },
    MlLinkUpdateRequest {
        project_id: String,
        link_id: String,
        #[serde(default)]
        label: String,
        sync_permissions: bool,
        #[serde(default)]
        role_map: Vec<MlRoleMapEntry>,
    },
    MlLinkUpdateResult {
        ok: bool,
    },
    /// `revoke_members` also removes the ML memberships this link granted
    /// (never the ML owner). The ML project itself is never deleted.
    MlLinkDetachRequest {
        project_id: String,
        link_id: String,
        revoke_members: bool,
    },
    MlLinkDetachResult {
        ok: bool,
        members_removed: u32,
    },
    MlLinkSyncNowRequest {
        project_id: String,
        link_id: String,
    },
    MlLinkSyncNowResponse {
        outcome: MlSyncOutcomeWire,
        last_sync_at: String,
        last_sync_result: String,
    },
    // ---- F4: kanban board (Z01) ----
    /// Status-only task move. TaskInfo (what the board renders) does NOT carry
    /// `description_md` or `attachments`, so dragging a card through
    /// TaskSaveRequest would write those back as empty and destroy them.
    TaskStatusSetRequest {
        project_id: String,
        task_id: String,
        /// 'todo' | 'in_progress' | 'review' | 'done'.
        status: String,
    },
    TaskStatusSetResult {
        ok: bool,
        updated_at: String,
    },
    // ---- F4: project export / import ----
    ProjectExportStartRequest {
        project_id: String,
        include_runs: bool,
        include_vectors: bool,
        /// Copies display names into the archive so historical authorship
        /// stays readable on the target node (personal data — audited).
        include_user_names: bool,
    },
    ProjectExportStartResponse {
        job_id: String,
    },
    ProjectExportStatusRequest {
        project_id: String,
        job_id: String,
    },
    /// Polling this is the source of truth; ArchiveStream is only a live view.
    /// `signed_url` is populated once the archive is complete — the archive is
    /// downloaded over HTTP (signed URL) rather than the binary protocol
    /// because it can reach tens of gigabytes.
    ProjectExportStatusResponse {
        job_id: String,
        /// 'running' | 'success' | 'failed' | 'cancelled'.
        status: String,
        progress_pct: u32,
        phase: String,
        error: String,
        export_ref: String,
        signed_url: String,
        archive_bytes: u64,
        inventory: Option<ArchiveInventoryWire>,
    },
    /// Chunked archive upload straight to disk (`upload_id` is client-minted).
    ProjectImportUploadChunkRequest {
        upload_id: String,
        filename: String,
        seq: u32,
        total_chunks: u32,
        /// Raw chunk bytes. `serde_bytes` forces a CBOR byte-string; a bare
        /// `Vec<u8>` would encode as an array of integers (~2x the size).
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    ProjectImportUploadChunkResponse {
        complete: bool,
    },
    /// Reads ONLY the manifest of an uploaded archive — nothing is unpacked
    /// before the user confirms.
    ProjectImportPreviewRequest {
        upload_id: String,
    },
    ProjectImportPreviewResponse {
        archive_version: u32,
        exported_at: String,
        source_node_id: String,
        project_name: String,
        template: String,
        modules: Vec<String>,
        inventory: ArchiveInventoryWire,
        total_uncompressed_bytes: u64,
        /// True when the embedding fingerprint matches this node, so the
        /// vector file can be moved verbatim instead of re-indexed.
        vectors_reusable: bool,
        vectors_reason: String,
        has_runs: bool,
    },
    ProjectImportApplyRequest {
        upload_id: String,
        #[serde(default)]
        name_override: String,
        import_vectors: bool,
        import_runs: bool,
    },
    ProjectImportApplyResponse {
        job_id: String,
    },
    ProjectImportStatusRequest {
        job_id: String,
    },
    ProjectImportStatusResponse {
        job_id: String,
        /// 'running' | 'success' | 'failed' | 'cancelled'.
        status: String,
        progress_pct: u32,
        phase: String,
        error: String,
        /// Set once the project row exists (import is all-or-nothing).
        project_id: String,
        reindex_job_ids: Vec<String>,
        vectors_imported: bool,
    },
    /// Live progress of an export or import job (job owner only). Stream
    /// initiating request — no plain response.
    ArchiveStreamRequest {
        job_id: String,
    },
    ArchiveStreamChunk {
        job_id: String,
        phase: String,
        line: String,
        progress_pct: u32,
        ts_ms: i64,
    },
    ArchiveStreamEnd {
        job_id: String,
        status: String,
        error: Option<String>,
    },
    // ================================================================
    // Append-only past this point. F5+ additions (REST/MCP facades,
    // mesh sync) go into a NEW sub-enum instead: this one is nearing
    // the 256-variant budget of the frame format.
    // Never insert above: reordering existing variants breaks the wire
    // contract with older peers, so new variants go strictly at the end.
    // ================================================================
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn project_studio_payload_round_trip() {
        let payload = ProjectStudioPayload::ProjectCreateRequest {
            name: "Projekt QA".to_string(),
            description: "opis".to_string(),
            template: "tests".to_string(),
            modules: vec!["knowledge".to_string(), "chat".to_string()],
            members: vec![MemberInputWire {
                user_id: "u1".to_string(),
                role: "editor".to_string(),
            }],
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded = crate::cbor::decode::<ProjectStudioPayload>(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn message_body_project_studio_round_trip() {
        let body = MessageBody::ProjectStudioBody(ProjectStudioPayload::SourceUploadChunkRequest {
            project_id: "p1".to_string(),
            upload_id: "up1".to_string(),
            filename: "spec.pdf".to_string(),
            mime: "application/pdf".to_string(),
            seq: 0,
            total_chunks: 2,
            bytes: vec![0x00, 0xff, 0x10],
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, field or the
    /// `MessageBody::ProjectStudioBody` tag into a test failure.
    #[test]
    fn project_studio_wire_golden() {
        // ProjectStudioPayload::ProjectsListRequest { include_archived: false }
        let list = ProjectStudioPayload::ProjectsListRequest {
            include_archived: false,
        };
        let bytes = crate::cbor::encode(&list).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17350726f6a656374734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "ProjectsListRequest wire drift"
        );

        // MessageBody::ProjectStudioBody(ProjectsListRequest) — outer body tag + variant tag.
        let body = MessageBody::ProjectStudioBody(list);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17150726f6a65637453747564696f426f6479a17350726f6a656374734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "MessageBody::ProjectStudioBody wire drift"
        );

        // ProjectStudioPayload::IngestStreamChunk — full field set (order/names).
        let chunk = ProjectStudioPayload::IngestStreamChunk {
            job_id: "j1".to_string(),
            kind: "log".to_string(),
            phase: String::new(),
            line: "x".to_string(),
            progress_pct: 0,
            ts_ms: 0,
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a171496e6765737453747265616d4368756e6ba6666a6f625f6964626a31646b696e64636c6f6765706861736560646c696e6561786c70726f67726573735f706374006574735f6d7300"
            ),
            "IngestStreamChunk wire drift"
        );

        // SourcesListResponse with one SourceInfo carrying Some(IngestJobWire) —
        // pins the nested job on the wire.
        let sources = ProjectStudioPayload::SourcesListResponse {
            sources: vec![SourceInfo {
                source_id: "s1".to_string(),
                kind: "document".to_string(),
                name: "n".to_string(),
                status: "ready".to_string(),
                config_json: "{}".to_string(),
                error: None,
                file_count: 1,
                chunk_count: 2,
                created_by: "u1".to_string(),
                created_by_name: "U".to_string(),
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
                last_job: Some(IngestJobWire {
                    job_id: "j1".to_string(),
                    source_id: "s1".to_string(),
                    status: "success".to_string(),
                    files_total: 1,
                    files_done: 1,
                    chunks_done: 2,
                    error: None,
                    started_at: "t".to_string(),
                    finished_at: Some("t".to_string()),
                }),
            }],
        };
        let bytes = crate::cbor::encode(&sources).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a173536f75726365734c697374526573706f6e7365a167736f757263657381ad69736f757263655f6964627331646b696e6468646f63756d656e74646e616d65616e667374617475736572656164796b636f6e6669675f6a736f6e627b7d656572726f72f66a66696c655f636f756e74016b6368756e6b5f636f756e74026a637265617465645f62796275316f637265617465645f62795f6e616d6561556a637265617465645f617461746a757064617465645f61746174686c6173745f6a6f62a9666a6f625f6964626a3169736f757263655f69646273316673746174757367737563636573736b66696c65735f746f74616c016a66696c65735f646f6e65016b6368756e6b735f646f6e6502656572726f72f66a737461727465645f617461746b66696e69736865645f61746174"
            ),
            "SourcesListResponse wire drift"
        );

        // F2: CasesListRequest — pins the first appended F2 variant (name +
        // full field set with defaulted filters).
        let cases = ProjectStudioPayload::CasesListRequest {
            project_id: "p1".to_string(),
            kind: String::new(),
            status: String::new(),
            priority: String::new(),
            tag_id: String::new(),
            origin: String::new(),
            search: String::new(),
            offset: 0,
            limit: 50,
        };
        let bytes = crate::cbor::encode(&cases).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17043617365734c69737452657175657374a96a70726f6a6563745f6964627031646b696e64606673746174757360687072696f7269747960667461675f696460666f726967696e606673656172636860666f666673657400656c696d69741832"
            ),
            "CasesListRequest wire drift"
        );

        // F2: RunItemClaimRequest with item_id: None — pins the Option
        // encoding (CBOR null) for the pool-claim path.
        let claim = ProjectStudioPayload::RunItemClaimRequest {
            project_id: "p1".to_string(),
            run_id: "r1".to_string(),
            item_id: None,
        };
        let bytes = crate::cbor::encode(&claim).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17352756e4974656d436c61696d52657175657374a36a70726f6a6563745f69646270316672756e5f6964627231676974656d5f6964f6"
            ),
            "RunItemClaimRequest wire drift"
        );

        // F3: EnvironmentsListRequest — pins the first appended F3 variant.
        let envs = ProjectStudioPayload::EnvironmentsListRequest {
            project_id: "p1".to_string(),
        };
        let bytes = crate::cbor::encode(&envs).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a177456e7669726f6e6d656e74734c69737452657175657374a16a70726f6a6563745f6964627031"),
            "EnvironmentsListRequest wire drift"
        );

        // F3: RunStartAutoRequest — full field set (XOR case selectors with
        // defaults, environment + optional runner + perf profile).
        let auto = ProjectStudioPayload::RunStartAutoRequest {
            project_id: "p1".to_string(),
            name: "smoke".to_string(),
            suite_id: "s1".to_string(),
            case_ids: vec![],
            from_run_id: String::new(),
            environment_id: "e1".to_string(),
            runner_service_id: String::new(),
            perf_profile_json: String::new(),
        };
        let bytes = crate::cbor::encode(&auto).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17352756e53746172744175746f52657175657374a86a70726f6a6563745f6964627031646e616d6565736d6f6b656873756974655f696462733168636173655f696473806b66726f6d5f72756e5f6964606e656e7669726f6e6d656e745f69646265317172756e6e65725f736572766963655f69646071706572665f70726f66696c655f6a736f6e60"
            ),
            "RunStartAutoRequest wire drift"
        );

        // F3: TryRunStreamChunk — pins the try-run live-log chunk shape.
        let try_chunk = ProjectStudioPayload::TryRunStreamChunk {
            try_id: "t1".to_string(),
            kind: "log".to_string(),
            line: "x".to_string(),
            phase: String::new(),
            ts_ms: 0,
        };
        let bytes = crate::cbor::encode(&try_chunk).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17154727952756e53747265616d4368756e6ba5667472795f6964627431646b696e64636c6f67646c696e656178657068617365606574735f6d7300"
            ),
            "TryRunStreamChunk wire drift"
        );

        // F3: RunArtifactGetRequest — pins the binary artifact download path.
        let art = ProjectStudioPayload::RunArtifactGetRequest {
            project_id: "p1".to_string(),
            artifact_id: "a1".to_string(),
            max_bytes: 1024,
        };
        let bytes = crate::cbor::encode(&art).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17552756e417274696661637447657452657175657374a36a70726f6a6563745f69646270316b61727469666163745f6964626131696d61785f6279746573190400"
            ),
            "RunArtifactGetRequest wire drift"
        );

        // F4: ScheduleSaveRequest — pins the first appended F4 variant with
        // its complete field set (the toggle path must NOT reuse it).
        let sched = ProjectStudioPayload::ScheduleSaveRequest {
            project_id: "p1".to_string(),
            schedule_id: None,
            name: "nocny".to_string(),
            run_type: "auto".to_string(),
            suite_id: "s1".to_string(),
            case_ids: vec![],
            environment_id: "e1".to_string(),
            runner_service_id: String::new(),
            perf_profile_json: String::new(),
            assignment_mode: String::new(),
            assignees: vec![],
            schedule_kind: "cron".to_string(),
            schedule_expr: "30 2 * * *".to_string(),
            timezone: "Europe/Warsaw".to_string(),
            enabled: true,
        };
        let bytes = crate::cbor::encode(&sched).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1735363686564756c655361766552657175657374af6a70726f6a6563745f69646270316b7363686564756c655f6964f6646e616d65656e6f636e796872756e5f74797065646175746f6873756974655f696462733168636173655f696473806e656e7669726f6e6d656e745f69646265317172756e6e65725f736572766963655f69646071706572665f70726f66696c655f6a736f6e606f61737369676e6d656e745f6d6f6465606961737369676e656573806d7363686564756c655f6b696e646463726f6e6d7363686564756c655f657870726a33302032202a202a202a6874696d657a6f6e656d4575726f70652f57617273617767656e61626c6564f5"
            ),
            "ScheduleSaveRequest wire drift"
        );

        // F4: MlLinksListResponse with a full MlLinkInfo (nested role map and
        // Some(summary)) — pins the ML link payload end to end.
        let links = ProjectStudioPayload::MlLinksListResponse {
            links: vec![MlLinkInfo {
                link_id: "l1".to_string(),
                ml_project_id: "m1".to_string(),
                label: "wizja".to_string(),
                origin: "linked_existing".to_string(),
                sync_permissions: true,
                role_map: vec![MlRoleMapEntry {
                    project_role: "tester".to_string(),
                    ml_role: "viewer".to_string(),
                }],
                last_sync_at: "t".to_string(),
                last_sync_result: "ok".to_string(),
                created_by: "u1".to_string(),
                created_by_name: "U".to_string(),
                created_at: "t".to_string(),
                summary: Some(MlProjectSummaryWire {
                    ml_project_id: "m1".to_string(),
                    name: "Wizja".to_string(),
                    project_type: "detection".to_string(),
                    project_type_label: "Detekcja".to_string(),
                    status: "active".to_string(),
                    dataset_count: 2,
                    model_count: 1,
                    models: vec!["yolo".to_string()],
                    last_training_run_id: "tr1".to_string(),
                    last_training_status: "completed".to_string(),
                    last_training_started_at: "t".to_string(),
                    last_training_finished_at: "t".to_string(),
                    last_training_metrics_json: "{}".to_string(),
                    training_in_progress: false,
                    deep_link: "/ml/m1".to_string(),
                }),
                can_open: true,
            }],
            can_manage: true,
        };
        let bytes = crate::cbor::encode(&links).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a1734d6c4c696e6b734c697374526573706f6e7365a2656c696e6b7381ad676c696e6b5f6964626c316d6d6c5f70726f6a6563745f6964626d31656c6162656c6577697a6a61666f726967696e6f6c696e6b65645f6578697374696e677073796e635f7065726d697373696f6e73f568726f6c655f6d617081a26c70726f6a6563745f726f6c6566746573746572676d6c5f726f6c65667669657765726c6c6173745f73796e635f61746174706c6173745f73796e635f726573756c74626f6b6a637265617465645f62796275316f637265617465645f62795f6e616d6561556a637265617465645f617461746773756d6d617279af6d6d6c5f70726f6a6563745f6964626d31646e616d656557697a6a616c70726f6a6563745f7479706569646574656374696f6e7270726f6a6563745f747970655f6c6162656c68446574656b636a6166737461747573666163746976656d646174617365745f636f756e74026b6d6f64656c5f636f756e7401666d6f64656c738164796f6c6f746c6173745f747261696e696e675f72756e5f696463747231746c6173745f747261696e696e675f73746174757369636f6d706c6574656478186c6173745f747261696e696e675f737461727465645f6174617478196c6173745f747261696e696e675f66696e69736865645f61746174781a6c6173745f747261696e696e675f6d6574726963735f6a736f6e627b7d74747261696e696e675f696e5f70726f6772657373f469646565705f6c696e6b662f6d6c2f6d316863616e5f6f70656ef56a63616e5f6d616e616765f5"
            ),
            "MlLinksListResponse wire drift"
        );

        // F4: TaskStatusSetRequest — the kanban move must stay a three-field
        // status-only write.
        let move_task = ProjectStudioPayload::TaskStatusSetRequest {
            project_id: "p1".to_string(),
            task_id: "t1".to_string(),
            status: "in_progress".to_string(),
        };
        let bytes = crate::cbor::encode(&move_task).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a1745461736b53746174757353657452657175657374a36a70726f6a6563745f6964627031677461736b5f6964627431667374617475736b696e5f70726f6772657373"),
            "TaskStatusSetRequest wire drift"
        );

        // F4: ProjectImportUploadChunkRequest — `bytes` MUST land as a CBOR
        // byte string (0x43 = 3-byte string), never an integer array.
        let chunk = ProjectStudioPayload::ProjectImportUploadChunkRequest {
            upload_id: "up1".to_string(),
            filename: "projekt.zip".to_string(),
            seq: 0,
            total_chunks: 2,
            bytes: vec![0x00, 0xff, 0x10],
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a1781f50726f6a656374496d706f727455706c6f61644368756e6b52657175657374a56975706c6f61645f6964637570316866696c656e616d656b70726f6a656b742e7a697063736571006c746f74616c5f6368756e6b73026562797465734300ff10"),
            "ProjectImportUploadChunkRequest wire drift"
        );
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }
}
