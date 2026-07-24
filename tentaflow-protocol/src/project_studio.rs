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
    // ================================================================
    // Append-only past this point (F2: test_cases, suites, runs,
    // generations, documents, tasks, notifications). Never insert above:
    // ciborium encodes variants by index, inserting or reordering breaks
    // older peers on the wire.
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
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }
}
