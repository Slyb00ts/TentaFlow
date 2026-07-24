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
    /// 'manual' (automated run types arrive in later phases).
    pub run_type: String,
    /// Reserved for environments (F3+); always '' in F2.
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
    },
    ReportQueryResponse {
        rows_json: String,
    },
    // ================================================================
    // Append-only past this point (F3+: environments, coded cases,
    // automated runs, schedules). Never insert above: reordering existing
    // variants breaks the wire contract with older peers, so new variants
    // go strictly at the end.
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
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }
}
