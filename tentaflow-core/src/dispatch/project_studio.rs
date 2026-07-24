// =============================================================================
// Plik: dispatch/project_studio.rs
// Opis: Handlery binarnego API Project Studio ("Projekty") — rejestr projektów,
//       członkowie i granty tworzenia, źródła wiedzy (chunkowany upload +
//       joby ingestu), pliki źródeł, przeszukiwanie bazy wiedzy, przegląd i
//       aktywność, prywatne czaty per użytkownik oraz ustawienia/tagi.
//       Streaming ingestu żyje w stream_handlers.rs; stream czatu przyjdzie
//       z seedem flow ps-chat.
// Przykład: ProjectStudioPayload::ProjectsListRequest → ProjectsListResponse.
// =============================================================================

use std::collections::{HashMap, HashSet};

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::project_studio::{
    ActivityEntry, ChatInfo, ChatMessageWire, CreatorGrantInfo, IngestJobWire, KbHit, MemberInfo,
    MemberInputWire, OverviewKpis, ProjectAgentBinding, ProjectInfo, ProjectSettings,
    ProjectStudioPayload, SourceFileInfo, SourceInfo, TagInfo, UserRefWire,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::project_studio::models::{
    ActivityRecord, IngestJobRecord, ProjectRecord, ProjectRole, SourceListItem,
};
use crate::project_studio::{activity, ingest, project_db, repository};
use crate::services::rbac::OrgContext;
use crate::services::vector::error::VectorError;
use tentaflow_sdk_spec::{FieldValue, Filter};

const PERM_READ: &str = "project_studio.read";
const PERM_ADMIN: &str = "project_studio.admin";

const VALID_TEMPLATES: &[&str] = &["tests", "docs", "tests_docs", "custom"];
const VALID_MODULES: &[&str] = &["knowledge", "tests", "docs", "chat", "tasks"];
/// Roles grantable through the wire (owner exists only via create/transfer).
const GRANTABLE_ROLES: &[&str] = &["manager", "editor", "tester", "viewer"];
/// Agent functions accepted in settings (F1 UI exposes only 'chat').
const AGENT_FUNCTIONS: &[&str] = &[
    "chat",
    "generator_manual",
    "generator_ui",
    "generator_api",
    "generator_unit",
    "generator_perf",
    "security",
    "documentalist",
    "critic",
    "supervisor",
];

const PREVIEW_MAX_BYTES: u32 = 256 * 1024;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project_studio.read permission required",
        ));
    }
    Ok(org)
}

fn require_admin(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_ADMIN) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project_studio.admin permission required",
        ));
    }
    Ok(org)
}

fn is_admin(org: &OrgContext) -> bool {
    org.has(PERM_ADMIN)
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "project studio database error");
    ProtocolError::internal("project studio database error")
}

/// Maps a `UNIQUE` constraint clash to BadRequest (duplicate name), anything
/// else to the generic internal error.
fn map_unique(scope: &str, message: &str, error: anyhow::Error) -> ProtocolError {
    if error.to_string().contains("UNIQUE") {
        ProtocolError::bad_request(message)
    } else {
        db_error(scope, error)
    }
}

fn not_found() -> ProtocolError {
    ProtocolError::not_found("project not found")
}

/// Loads the project (org-scoped) and enforces the role gate. Non-members get
/// NotFound so a project's existence never leaks. `project_studio.admin`
/// overrides ONLY the viewer tier (inspection outside membership) and the
/// owner tier (archive/delete/orphan takeover) — content mutations
/// (tester/editor/manager) always require real membership.
///
/// Returns the project record and the caller's membership role (`None` when
/// the admin override applied).
fn require_project(
    org: &OrgContext,
    project_id: &str,
    min: ProjectRole,
) -> Result<(ProjectRecord, Option<ProjectRole>), ProtocolError> {
    project_db::validate_project_id(project_id)
        .map_err(|_| ProtocolError::bad_request("invalid project_id"))?;
    let record = repository::get_project(&org.org_id, project_id)
        .map_err(|e| db_error("get_project", e))?
        .ok_or_else(not_found)?;
    let role = repository::effective_role(project_id, &org.user_id)
        .map_err(|e| db_error("member_role", e))?;
    match role {
        Some(role) if role >= min => Ok((record, Some(role))),
        Some(_) if matches!(min, ProjectRole::Owner) && is_admin(org) => Ok((record, role)),
        Some(_) => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("requires project role '{}' or higher", min.slug()),
        )),
        None if is_admin(org) && matches!(min, ProjectRole::Viewer | ProjectRole::Owner) => {
            Ok((record, None))
        }
        None if is_admin(org) => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "requires project membership",
        )),
        None => Err(not_found()),
    }
}

fn parse_role(role: &str) -> Result<ProjectRole, ProtocolError> {
    ProjectRole::from_slug(role)
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown role '{role}'")))
}

/// Archived projects are read-only: every mutation handler short-circuits
/// here right after the role gate. Only reads, unarchive (ProjectArchive)
/// and ProjectDelete skip this check.
fn require_active(record: &ProjectRecord) -> Result<(), ProtocolError> {
    if record.status == "archived" {
        return Err(ProtocolError::bad_request("project is archived"));
    }
    Ok(())
}

/// Polls the given ingest jobs until each reaches a terminal status (250 ms
/// interval, 30 s shared deadline). Cancellation is cooperative — the delete
/// paths must not rip files/vectors out from under a job that is still
/// writing, so they block here (bounded) instead of racing it. Jobs queued on
/// the ingest semaphore also finish terminally on cancel, so they resolve
/// within one poll interval.
async fn wait_for_jobs_terminal(
    pool: &crate::db::DbPool,
    job_ids: &[String],
) -> Result<(), ProtocolError> {
    if job_ids.is_empty() {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    for job_id in job_ids {
        loop {
            let running = matches!(
                repository::get_ingest_job(pool, job_id),
                Ok(Some(job)) if job.status == "running"
            );
            if !running {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ProtocolError::internal("ingest job did not stop in time"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    Ok(())
}

fn open_project_pool(project_id: &str) -> Result<crate::db::DbPool, ProtocolError> {
    project_db::open(project_id).map_err(|e| db_error("project_db.open", e))
}

fn ps(body: ProjectStudioPayload) -> MessageBody {
    MessageBody::ProjectStudioBody(body)
}

// =============================================================================
// Wire mapping helpers
// =============================================================================

fn parse_modules_json(modules_json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(modules_json).unwrap_or_default()
}

fn job_to_wire(job: &IngestJobRecord) -> IngestJobWire {
    IngestJobWire {
        job_id: job.job_id.clone(),
        source_id: job.source_id.clone(),
        status: job.status.clone(),
        files_total: job.files_total,
        files_done: job.files_done,
        chunks_done: job.chunks_done,
        error: if job.error.is_empty() {
            None
        } else {
            Some(job.error.clone())
        },
        started_at: job.started_at.clone(),
        finished_at: job.finished_at.clone(),
    }
}

fn source_to_wire(item: SourceListItem, names: &HashMap<String, (String, String)>) -> SourceInfo {
    let r = item.record;
    SourceInfo {
        source_id: r.source_id,
        kind: r.kind,
        name: r.name,
        status: r.status,
        config_json: r.config_json,
        error: if r.error.is_empty() {
            None
        } else {
            Some(r.error)
        },
        file_count: item.file_count,
        chunk_count: item.chunk_count,
        created_by_name: names
            .get(&r.created_by)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| r.created_by.clone()),
        created_by: r.created_by,
        created_at: r.created_at,
        updated_at: r.updated_at,
        last_job: item.last_job.as_ref().map(job_to_wire),
    }
}

fn activity_to_wire(
    entries: Vec<ActivityRecord>,
    names: &HashMap<String, (String, String)>,
) -> Vec<ActivityEntry> {
    entries
        .into_iter()
        .map(|e| ActivityEntry {
            id: e.id,
            actor_name: names
                .get(&e.actor_user_id)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| e.actor_user_id.clone()),
            actor_user_id: e.actor_user_id,
            actor_kind: e.actor_kind,
            action: e.action,
            object_type: e.object_type,
            object_id: e.object_id,
            details_json: e.details_json,
            created_at: e.created_at,
        })
        .collect()
}

/// Builds the full `ProjectInfo` for one record (list + detail views).
fn project_info(
    record: &ProjectRecord,
    my_role: Option<String>,
    owner_names: &HashMap<String, (String, String)>,
) -> Result<ProjectInfo, ProtocolError> {
    let member_count =
        repository::member_count(&record.project_id).map_err(|e| db_error("member_count", e))?;
    let (source_count, sources_ready) = repository::read_source_counts(&record.dir_path);
    Ok(ProjectInfo {
        project_id: record.project_id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        status: record.status.clone(),
        template: record.template.clone(),
        modules: parse_modules_json(&record.modules_json),
        owner_user_id: record.owner_user_id.clone(),
        owner_name: owner_names
            .get(&record.owner_user_id)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| record.owner_user_id.clone()),
        member_count,
        source_count,
        sources_ready,
        my_role,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    })
}

// =============================================================================
// Dispatcher
// =============================================================================

#[handler(variant = "ProjectStudioBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn project_studio_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ProjectStudioBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected ProjectStudioBody")),
    };

    use ProjectStudioPayload as P;
    match payload {
        P::ProjectsListRequest { include_archived } => projects_list_v1(ctx, *include_archived),
        P::ProjectCreateRequest {
            name,
            description,
            template,
            modules,
            members,
        } => project_create_v1(ctx, name, description, template, modules, members),
        P::ProjectGetRequest { project_id } => project_get_v1(ctx, project_id),
        P::ProjectUpdateRequest {
            project_id,
            name,
            description,
        } => project_update_v1(ctx, project_id, name, description),
        P::ProjectArchiveRequest {
            project_id,
            archived,
        } => project_archive_v1(ctx, project_id, *archived),
        P::ProjectDeleteRequest { project_id } => project_delete_v1(ctx, project_id).await,
        P::MembersListRequest { project_id } => members_list_v1(ctx, project_id),
        P::MemberCandidatesRequest {
            project_id,
            query,
            limit,
        } => member_candidates_v1(ctx, project_id.as_deref(), query, *limit),
        P::MembersAddRequest {
            project_id,
            members,
        } => members_add_v1(ctx, project_id, members),
        P::MemberRoleSetRequest {
            project_id,
            user_id,
            role,
        } => member_role_set_v1(ctx, project_id, user_id, role),
        P::MemberRemoveRequest {
            project_id,
            user_id,
        } => member_remove_v1(ctx, project_id, user_id),
        P::OwnershipTransferRequest {
            project_id,
            new_owner_user_id,
        } => ownership_transfer_v1(ctx, project_id, new_owner_user_id),
        P::CreatorGrantsListRequest => creator_grants_list_v1(ctx),
        P::CreatorGrantSetRequest { user_id, granted } => {
            creator_grant_set_v1(ctx, user_id, *granted)
        }
        P::SourcesListRequest { project_id } => sources_list_v1(ctx, project_id),
        P::SourceUploadChunkRequest {
            project_id,
            upload_id,
            filename,
            mime,
            seq,
            total_chunks,
            bytes,
        } => {
            source_upload_chunk_v1(
                ctx,
                project_id,
                upload_id,
                filename,
                mime,
                *seq,
                *total_chunks,
                bytes,
            )
            .await
        }
        P::SourceCreateRequest {
            project_id,
            kind,
            name,
            config_json,
            file_refs,
        } => source_create_v1(ctx, project_id, kind, name, config_json, file_refs),
        P::SourceUpdateRequest {
            project_id,
            source_id,
            name,
            config_json,
        } => source_update_v1(ctx, project_id, source_id, name, config_json),
        P::SourceDeleteRequest {
            project_id,
            source_id,
        } => source_delete_v1(ctx, project_id, source_id).await,
        P::SourceReingestRequest {
            project_id,
            source_id,
            file_id,
        } => source_reingest_v1(ctx, project_id, source_id, file_id.as_deref()),
        P::IngestCancelRequest { project_id, job_id } => ingest_cancel_v1(ctx, project_id, job_id),
        P::IngestStatusRequest { project_id, job_id } => ingest_status_v1(ctx, project_id, job_id),
        P::SourceFilesListRequest {
            project_id,
            source_id,
            offset,
            limit,
            filter,
        } => source_files_list_v1(ctx, project_id, source_id, *offset, *limit, filter),
        P::SourceFileDeleteRequest {
            project_id,
            file_id,
        } => source_file_delete_v1(ctx, project_id, file_id).await,
        P::SourceFilePreviewRequest {
            project_id,
            file_id,
            max_bytes,
        } => source_file_preview_v1(ctx, project_id, file_id, *max_bytes).await,
        P::KbSearchRequest {
            project_id,
            query,
            source_ids,
            limit,
        } => kb_search_v1(ctx, project_id, query, source_ids, *limit).await,
        P::OverviewRequest { project_id } => overview_v1(ctx, project_id),
        P::ActivityListRequest {
            project_id,
            before_id,
            limit,
        } => activity_list_v1(ctx, project_id, *before_id, *limit),
        P::ChatsListRequest { project_id } => chats_list_v1(ctx, project_id),
        P::ChatCreateRequest { project_id, title } => chat_create_v1(ctx, project_id, title),
        P::ChatRenameRequest {
            project_id,
            chat_id,
            title,
        } => chat_rename_v1(ctx, project_id, chat_id, title),
        P::ChatDeleteRequest {
            project_id,
            chat_id,
        } => chat_delete_v1(ctx, project_id, chat_id),
        P::ChatHistoryRequest {
            project_id,
            chat_id,
            before_message_id,
            limit,
        } => chat_history_v1(
            ctx,
            project_id,
            chat_id,
            before_message_id.as_deref(),
            *limit,
        ),
        P::SettingsGetRequest { project_id } => settings_get_v1(ctx, project_id),
        P::SettingsSaveRequest {
            project_id,
            name,
            description,
            agents_json,
        } => settings_save_v1(
            ctx,
            project_id,
            name.as_deref(),
            description.as_deref(),
            agents_json.as_deref(),
        ),
        P::TagSaveRequest {
            project_id,
            tag_id,
            name,
        } => tag_save_v1(ctx, project_id, tag_id.as_deref(), name),
        P::TagDeleteRequest { project_id, tag_id } => tag_delete_v1(ctx, project_id, tag_id),
        // Stream requests are served by dedicated stream handlers over
        // subscribe, never by the request/response path.
        P::IngestStreamRequest { .. } | P::ChatStreamRequest { .. } => Err(
            ProtocolError::bad_request("use streaming subscribe for this variant"),
        ),
        P::ProjectsListResponse { .. }
        | P::ProjectCreateResponse { .. }
        | P::ProjectGetResponse { .. }
        | P::ProjectUpdateResult { .. }
        | P::ProjectArchiveResult { .. }
        | P::ProjectDeleteResult { .. }
        | P::MembersListResponse { .. }
        | P::MemberCandidatesResponse { .. }
        | P::MembersAddResponse { .. }
        | P::MemberRoleSetResult { .. }
        | P::MemberRemoveResult { .. }
        | P::OwnershipTransferResult { .. }
        | P::CreatorGrantsListResponse { .. }
        | P::CreatorGrantSetResult { .. }
        | P::SourcesListResponse { .. }
        | P::SourceUploadChunkResponse { .. }
        | P::SourceCreateResponse { .. }
        | P::SourceUpdateResponse { .. }
        | P::SourceDeleteResult { .. }
        | P::SourceReingestResponse { .. }
        | P::IngestCancelResult { .. }
        | P::IngestStatusResponse { .. }
        | P::SourceFilesListResponse { .. }
        | P::SourceFileDeleteResult { .. }
        | P::SourceFilePreviewResponse { .. }
        | P::KbSearchResponse { .. }
        | P::OverviewResponse { .. }
        | P::ActivityListResponse { .. }
        | P::ChatsListResponse { .. }
        | P::ChatCreateResponse { .. }
        | P::ChatRenameResult { .. }
        | P::ChatDeleteResult { .. }
        | P::ChatHistoryResponse { .. }
        | P::SettingsGetResponse { .. }
        | P::SettingsSaveResult { .. }
        | P::TagSaveResponse { .. }
        | P::TagDeleteResult { .. }
        | P::IngestStreamChunk { .. }
        | P::IngestStreamEnd { .. }
        | P::ChatStreamChunk { .. }
        | P::ChatStreamEnd { .. } => Err(ProtocolError::bad_request(
            "variant is not a valid project studio request",
        )),
        // F2 variants (cases/suites/runs/tasks/generations/notifications/
        // reports) get real handlers in the F2 backend step; a wildcard keeps
        // this match compiling until then without listing 84 unhandled arms.
        _ => Err(ProtocolError::bad_request(
            "project studio variant not handled yet",
        )),
    }
}

macro_rules! register_project_studio_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_project_studio_dispatch,
            }
        }
    };
}

register_project_studio_variant!(
    "ProjectStudioProjectsListRequest",
    "tentaflow_ws_handler_ps_projects_list"
);
register_project_studio_variant!(
    "ProjectStudioProjectCreateRequest",
    "tentaflow_ws_handler_ps_project_create"
);
register_project_studio_variant!(
    "ProjectStudioProjectGetRequest",
    "tentaflow_ws_handler_ps_project_get"
);
register_project_studio_variant!(
    "ProjectStudioProjectUpdateRequest",
    "tentaflow_ws_handler_ps_project_update"
);
register_project_studio_variant!(
    "ProjectStudioProjectArchiveRequest",
    "tentaflow_ws_handler_ps_project_archive"
);
register_project_studio_variant!(
    "ProjectStudioProjectDeleteRequest",
    "tentaflow_ws_handler_ps_project_delete"
);
register_project_studio_variant!(
    "ProjectStudioMembersListRequest",
    "tentaflow_ws_handler_ps_members_list"
);
register_project_studio_variant!(
    "ProjectStudioMemberCandidatesRequest",
    "tentaflow_ws_handler_ps_member_candidates"
);
register_project_studio_variant!(
    "ProjectStudioMembersAddRequest",
    "tentaflow_ws_handler_ps_members_add"
);
register_project_studio_variant!(
    "ProjectStudioMemberRoleSetRequest",
    "tentaflow_ws_handler_ps_member_role_set"
);
register_project_studio_variant!(
    "ProjectStudioMemberRemoveRequest",
    "tentaflow_ws_handler_ps_member_remove"
);
register_project_studio_variant!(
    "ProjectStudioOwnershipTransferRequest",
    "tentaflow_ws_handler_ps_ownership_transfer"
);
register_project_studio_variant!(
    "ProjectStudioCreatorGrantsListRequest",
    "tentaflow_ws_handler_ps_creator_grants_list"
);
register_project_studio_variant!(
    "ProjectStudioCreatorGrantSetRequest",
    "tentaflow_ws_handler_ps_creator_grant_set"
);
register_project_studio_variant!(
    "ProjectStudioSourcesListRequest",
    "tentaflow_ws_handler_ps_sources_list"
);
register_project_studio_variant!(
    "ProjectStudioSourceUploadChunkRequest",
    "tentaflow_ws_handler_ps_source_upload_chunk"
);
register_project_studio_variant!(
    "ProjectStudioSourceCreateRequest",
    "tentaflow_ws_handler_ps_source_create"
);
register_project_studio_variant!(
    "ProjectStudioSourceUpdateRequest",
    "tentaflow_ws_handler_ps_source_update"
);
register_project_studio_variant!(
    "ProjectStudioSourceDeleteRequest",
    "tentaflow_ws_handler_ps_source_delete"
);
register_project_studio_variant!(
    "ProjectStudioSourceReingestRequest",
    "tentaflow_ws_handler_ps_source_reingest"
);
register_project_studio_variant!(
    "ProjectStudioIngestCancelRequest",
    "tentaflow_ws_handler_ps_ingest_cancel"
);
register_project_studio_variant!(
    "ProjectStudioIngestStatusRequest",
    "tentaflow_ws_handler_ps_ingest_status"
);
register_project_studio_variant!(
    "ProjectStudioSourceFilesListRequest",
    "tentaflow_ws_handler_ps_source_files_list"
);
register_project_studio_variant!(
    "ProjectStudioSourceFileDeleteRequest",
    "tentaflow_ws_handler_ps_source_file_delete"
);
register_project_studio_variant!(
    "ProjectStudioSourceFilePreviewRequest",
    "tentaflow_ws_handler_ps_source_file_preview"
);
register_project_studio_variant!(
    "ProjectStudioKbSearchRequest",
    "tentaflow_ws_handler_ps_kb_search"
);
register_project_studio_variant!(
    "ProjectStudioOverviewRequest",
    "tentaflow_ws_handler_ps_overview"
);
register_project_studio_variant!(
    "ProjectStudioActivityListRequest",
    "tentaflow_ws_handler_ps_activity_list"
);
register_project_studio_variant!(
    "ProjectStudioChatsListRequest",
    "tentaflow_ws_handler_ps_chats_list"
);
register_project_studio_variant!(
    "ProjectStudioChatCreateRequest",
    "tentaflow_ws_handler_ps_chat_create"
);
register_project_studio_variant!(
    "ProjectStudioChatRenameRequest",
    "tentaflow_ws_handler_ps_chat_rename"
);
register_project_studio_variant!(
    "ProjectStudioChatDeleteRequest",
    "tentaflow_ws_handler_ps_chat_delete"
);
register_project_studio_variant!(
    "ProjectStudioChatHistoryRequest",
    "tentaflow_ws_handler_ps_chat_history"
);
register_project_studio_variant!(
    "ProjectStudioSettingsGetRequest",
    "tentaflow_ws_handler_ps_settings_get"
);
register_project_studio_variant!(
    "ProjectStudioSettingsSaveRequest",
    "tentaflow_ws_handler_ps_settings_save"
);
register_project_studio_variant!(
    "ProjectStudioTagSaveRequest",
    "tentaflow_ws_handler_ps_tag_save"
);
register_project_studio_variant!(
    "ProjectStudioTagDeleteRequest",
    "tentaflow_ws_handler_ps_tag_delete"
);

// =============================================================================
// Registry: list / create / get / update / archive / delete
// =============================================================================

fn projects_list_v1(
    ctx: &HandlerContext,
    include_archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let admin = is_admin(org);
    let records = repository::list_projects(&org.org_id, include_archived)
        .map_err(|e| db_error("projects_list", e))?;
    let my_roles =
        repository::member_roles_for_user(&org.user_id).map_err(|e| db_error("member_roles", e))?;

    let visible: Vec<&ProjectRecord> = records
        .iter()
        .filter(|r| admin || my_roles.contains_key(&r.project_id))
        .collect();
    let owner_ids: Vec<String> = visible.iter().map(|r| r.owner_user_id.clone()).collect();
    let names = repository::resolve_user_refs(&owner_ids);

    let mut projects = Vec::with_capacity(visible.len());
    for record in visible {
        let my_role = my_roles.get(&record.project_id).cloned();
        projects.push(project_info(record, my_role, &names)?);
    }

    let can_create = admin
        || repository::has_creator_grant(&org.user_id, &org.org_id)
            .map_err(|e| db_error("creator_grant", e))?;
    Ok(ps(ProjectStudioPayload::ProjectsListResponse {
        projects,
        can_create,
    }))
}

fn project_create_v1(
    ctx: &HandlerContext,
    name: &str,
    description: &str,
    template: &str,
    modules: &[String],
    members: &[MemberInputWire],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let can_create = is_admin(org)
        || repository::has_creator_grant(&org.user_id, &org.org_id)
            .map_err(|e| db_error("creator_grant", e))?;
    if !can_create {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project creation requires a creator grant",
        ));
    }

    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("project name is required"));
    }
    if !VALID_TEMPLATES.contains(&template) {
        return Err(ProtocolError::bad_request(format!(
            "unknown template '{template}'"
        )));
    }
    for module in modules {
        if !VALID_MODULES.contains(&module.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "unknown module '{module}'"
            )));
        }
    }
    let mut initial: Vec<(String, String)> = Vec::with_capacity(members.len());
    for m in members {
        if !GRANTABLE_ROLES.contains(&m.role.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "role '{}' cannot be granted",
                m.role
            )));
        }
        if !repository::is_org_member(&org.org_id, &m.user_id)
            .map_err(|e| db_error("is_org_member", e))?
        {
            return Err(ProtocolError::bad_request(format!(
                "user '{}' is not a member of this organization",
                m.user_id
            )));
        }
        initial.push((m.user_id.clone(), m.role.clone()));
    }

    let project_id = uuid::Uuid::new_v4().to_string();
    let dir = crate::project_studio::project_dir(&project_id);
    std::fs::create_dir_all(dir.join("files"))
        .map_err(|e| ProtocolError::internal(format!("project dir create: {e}")))?;
    if let Err(e) = project_db::open_pool_at(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(db_error("project_db.create", e));
    }

    let modules_json =
        serde_json::to_string(modules).unwrap_or_else(|_| "[\"knowledge\",\"chat\"]".to_string());
    if let Err(e) = repository::create_project(
        &project_id,
        &org.org_id,
        name,
        description.trim(),
        template,
        &modules_json,
        &org.user_id,
        &dir.to_string_lossy(),
        &initial,
    ) {
        // Registry insert failed (e.g. duplicate name) — the freshly created
        // directory would otherwise leak.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(map_unique(
            "project_create",
            "a project with this name already exists",
            e,
        ));
    }

    if let Ok(pool) = project_db::open(&project_id) {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "project.created",
            "project",
            &project_id,
            &serde_json::json!({ "name": name }).to_string(),
        );
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.project.created",
        &project_id,
        name,
    );

    Ok(ps(ProjectStudioPayload::ProjectCreateResponse {
        project_id,
    }))
}

fn project_get_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, my_role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.owner_user_id));
    let project = project_info(&record, my_role.map(|r| r.slug().to_string()), &names)?;
    Ok(ps(ProjectStudioPayload::ProjectGetResponse { project }))
}

fn project_update_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: &str,
    description: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("project name is required"));
    }
    let ok =
        repository::update_project_name_desc(&org.org_id, project_id, name, description.trim())
            .map_err(|e| {
                map_unique(
                    "project_update",
                    "a project with this name already exists",
                    e,
                )
            })?;
    if ok {
        if let Ok(pool) = project_db::open(project_id) {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "project.updated",
                "project",
                project_id,
                &serde_json::json!({ "name": name }).to_string(),
            );
        }
    }
    Ok(ps(ProjectStudioPayload::ProjectUpdateResult { ok }))
}

fn project_archive_v1(
    ctx: &HandlerContext,
    project_id: &str,
    archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Owner)?;
    // Record while the pool may still be open; archiving closes it afterwards.
    if let Ok(pool) = project_db::open(project_id) {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            if archived {
                "project.archived"
            } else {
                "project.unarchived"
            },
            "project",
            project_id,
            "{}",
        );
    }
    let ok = repository::set_project_archived(&org.org_id, project_id, archived)
        .map_err(|e| db_error("project_archive", e))?;
    if archived {
        project_db::close(project_id);
    }
    Ok(ps(ProjectStudioPayload::ProjectArchiveResult { ok }))
}

async fn project_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Owner)?;

    // 1. Stop running ingest jobs so nothing keeps writing into the files
    //    that are about to disappear, and WAIT until each reaches a terminal
    //    status — cancel is cooperative, deleting under a live job would race
    //    its writes. A failed open means the directory is already gone
    //    (retried delete) — nothing can be running then.
    if let Ok(pool) = project_db::open(project_id) {
        if let Ok(jobs) = repository::running_job_ids(&pool) {
            for job_id in &jobs {
                ingest::signal_cancel(job_id);
            }
            wait_for_jobs_terminal(&pool, &jobs).await?;
        }
    }
    // 2. Drop the cached pool (checkpoint + release the SQLite handle).
    project_db::close(project_id);
    // 3. Drop every vector namespace of the `ps-<id>` scope (registry rows in
    //    tentaflow.db + on-disk data inside the project dir).
    ingest::drop_project_namespaces(&ctx.state.db, &org.org_id, project_id)
        .map_err(|e| db_error("drop_namespaces", e))?;
    // 4. Remove the project directory (project.db, files/, vectors/).
    let dir = std::path::PathBuf::from(&record.dir_path);
    let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir))
        .await
        .map_err(|_| ProtocolError::internal("project dir removal task panicked"))?;
    if let Err(e) = removed {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(ProtocolError::internal(format!("project dir removal: {e}")));
        }
    }
    // 5. Central registry rows last — a crash above leaves the project
    //    visible (and the delete retryable) instead of orphaning data.
    repository::delete_project_rows(project_id).map_err(|e| db_error("project_delete", e))?;

    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.project.deleted",
        project_id,
        &record.name,
    );
    Ok(ps(ProjectStudioPayload::ProjectDeleteResult { ok: true }))
}

// =============================================================================
// Members
// =============================================================================

fn members_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let rows = repository::list_members(project_id).map_err(|e| db_error("members_list", e))?;
    let mut ids: Vec<String> = rows.iter().map(|m| m.user_id.clone()).collect();
    ids.extend(rows.iter().map(|m| m.invited_by.clone()));
    let names = repository::resolve_user_refs(&ids);
    let members = rows
        .into_iter()
        .map(|m| MemberInfo {
            display_name: names
                .get(&m.user_id)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| m.user_id.clone()),
            email: names
                .get(&m.user_id)
                .map(|(_, e)| e.clone())
                .unwrap_or_default(),
            invited_by_name: names
                .get(&m.invited_by)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| m.invited_by.clone()),
            user_id: m.user_id,
            role: m.role,
            invited_by: m.invited_by,
            created_at: m.created_at,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::MembersListResponse { members }))
}

fn member_candidates_v1(
    ctx: &HandlerContext,
    project_id: Option<&str>,
    query: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let exclude: HashSet<String> = match project_id {
        Some(project_id) => {
            // Invite modal — manager+ of the target project.
            let (_record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
            repository::list_members(project_id)
                .map_err(|e| db_error("members_list", e))?
                .into_iter()
                .map(|m| m.user_id)
                .collect()
        }
        None => {
            // Creation wizard — creator grant (or admin); the creator becomes
            // the owner, so exclude them from the pick list.
            let can_create = is_admin(org)
                || repository::has_creator_grant(&org.user_id, &org.org_id)
                    .map_err(|e| db_error("creator_grant", e))?;
            if !can_create {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "project creation requires a creator grant",
                ));
            }
            std::iter::once(org.user_id.clone()).collect()
        }
    };

    let limit = limit.clamp(1, 50);
    let rows =
        repository::list_org_user_candidates(&org.org_id, query, limit + exclude.len() as u32)
            .map_err(|e| db_error("candidates", e))?;
    let users = rows
        .into_iter()
        .filter(|(id, _, _)| !exclude.contains(id))
        .take(limit as usize)
        .map(|(user_id, display_name, email)| UserRefWire {
            user_id,
            display_name,
            email,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::MemberCandidatesResponse { users }))
}

fn members_add_v1(
    ctx: &HandlerContext,
    project_id: &str,
    members: &[MemberInputWire],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let acting = role.expect("manager gate never yields admin override");
    if members.is_empty() {
        return Err(ProtocolError::bad_request("no members to add"));
    }
    let mut to_add: Vec<(String, String)> = Vec::with_capacity(members.len());
    for m in members {
        if !GRANTABLE_ROLES.contains(&m.role.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "role '{}' cannot be granted",
                m.role
            )));
        }
        // A manager may only grant roles below manager; granting manager is
        // reserved for the owner.
        if m.role == "manager" && acting != ProjectRole::Owner {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "only the owner may grant the manager role",
            ));
        }
        if !repository::is_org_member(&org.org_id, &m.user_id)
            .map_err(|e| db_error("is_org_member", e))?
        {
            return Err(ProtocolError::bad_request(format!(
                "user '{}' is not a member of this organization",
                m.user_id
            )));
        }
        to_add.push((m.user_id.clone(), m.role.clone()));
    }
    let added = repository::add_members(project_id, &to_add, &org.user_id)
        .map_err(|e| db_error("members_add", e))?;
    if added > 0 {
        if let Ok(pool) = project_db::open(project_id) {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "member.added",
                "member",
                "",
                &serde_json::json!({ "count": added }).to_string(),
            );
        }
        activity::record_org_security(
            &ctx.state.db,
            &ctx.state.local_node_id,
            &org.user_id,
            "project_studio.member.added",
            project_id,
            &format!("{added} member(s)"),
        );
    }
    Ok(ps(ProjectStudioPayload::MembersAddResponse { added }))
}

fn member_role_set_v1(
    ctx: &HandlerContext,
    project_id: &str,
    user_id: &str,
    role: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, acting) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let acting = acting.expect("manager gate never yields admin override");
    let new_role = parse_role(role)?;
    if new_role == ProjectRole::Owner {
        return Err(ProtocolError::bad_request(
            "owner role is assigned only via ownership transfer",
        ));
    }
    let target = repository::effective_role(project_id, user_id)
        .map_err(|e| db_error("member_role", e))?
        .ok_or_else(|| ProtocolError::not_found("member not found"))?;
    if target == ProjectRole::Owner {
        return Err(ProtocolError::bad_request(
            "transfer ownership before changing the owner's role",
        ));
    }
    // A manager operates strictly below the manager tier: may neither touch
    // another manager nor promote anyone to manager.
    if acting != ProjectRole::Owner
        && (target >= ProjectRole::Manager || new_role >= ProjectRole::Manager)
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the owner may manage manager roles",
        ));
    }
    let ok = repository::set_member_role(project_id, user_id, new_role.slug())
        .map_err(|e| db_error("member_role_set", e))?;
    if ok {
        if let Ok(pool) = project_db::open(project_id) {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "member.role_changed",
                "member",
                user_id,
                &serde_json::json!({ "role": new_role.slug() }).to_string(),
            );
        }
        activity::record_org_security(
            &ctx.state.db,
            &ctx.state.local_node_id,
            &org.user_id,
            "project_studio.member.role_changed",
            project_id,
            &format!("{user_id} -> {}", new_role.slug()),
        );
    }
    Ok(ps(ProjectStudioPayload::MemberRoleSetResult { ok }))
}

fn member_remove_v1(
    ctx: &HandlerContext,
    project_id: &str,
    user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, acting) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let acting = acting.expect("manager gate never yields admin override");
    let target = repository::effective_role(project_id, user_id)
        .map_err(|e| db_error("member_role", e))?
        .ok_or_else(|| ProtocolError::not_found("member not found"))?;
    if target == ProjectRole::Owner {
        return Err(ProtocolError::bad_request(
            "the owner cannot be removed — transfer ownership first",
        ));
    }
    if acting != ProjectRole::Owner && target >= ProjectRole::Manager {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the owner may remove a manager",
        ));
    }
    let ok =
        repository::remove_member(project_id, user_id).map_err(|e| db_error("member_remove", e))?;
    if ok {
        if let Ok(pool) = project_db::open(project_id) {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "member.removed",
                "member",
                user_id,
                "{}",
            );
        }
        activity::record_org_security(
            &ctx.state.db,
            &ctx.state.local_node_id,
            &org.user_id,
            "project_studio.member.removed",
            project_id,
            user_id,
        );
    }
    Ok(ps(ProjectStudioPayload::MemberRemoveResult { ok }))
}

fn ownership_transfer_v1(
    ctx: &HandlerContext,
    project_id: &str,
    new_owner_user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Owner tier: the owner themselves, or an org admin taking over an
    // orphaned project.
    let (record, _role) = require_project(org, project_id, ProjectRole::Owner)?;
    require_active(&record)?;
    if new_owner_user_id == record.owner_user_id {
        return Err(ProtocolError::bad_request("user is already the owner"));
    }
    repository::effective_role(project_id, new_owner_user_id)
        .map_err(|e| db_error("member_role", e))?
        .ok_or_else(|| ProtocolError::bad_request("new owner must be a project member"))?;
    repository::transfer_ownership(project_id, &record.owner_user_id, new_owner_user_id)
        .map_err(|e| db_error("ownership_transfer", e))?;
    if let Ok(pool) = project_db::open(project_id) {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "member.ownership_transferred",
            "member",
            new_owner_user_id,
            &serde_json::json!({ "previous_owner": record.owner_user_id }).to_string(),
        );
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.ownership_transferred",
        project_id,
        new_owner_user_id,
    );
    Ok(ps(ProjectStudioPayload::OwnershipTransferResult {
        ok: true,
    }))
}

// =============================================================================
// Creator grants (admin)
// =============================================================================

fn creator_grants_list_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let rows =
        repository::list_creator_grants(&org.org_id).map_err(|e| db_error("grants_list", e))?;
    let ids: Vec<String> = rows.iter().map(|g| g.user_id.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let grants = rows
        .into_iter()
        .map(|g| CreatorGrantInfo {
            display_name: names
                .get(&g.user_id)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| g.user_id.clone()),
            user_id: g.user_id,
            granted_by: g.granted_by,
            created_at: g.created_at,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::CreatorGrantsListResponse {
        grants,
    }))
}

fn creator_grant_set_v1(
    ctx: &HandlerContext,
    user_id: &str,
    granted: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    if granted
        && !repository::is_org_member(&org.org_id, user_id)
            .map_err(|e| db_error("is_org_member", e))?
    {
        return Err(ProtocolError::bad_request(
            "user is not a member of this organization",
        ));
    }
    let ok = repository::set_creator_grant(user_id, &org.org_id, &org.user_id, granted)
        .map_err(|e| db_error("grant_set", e))?;
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        if granted {
            "project_studio.creator_grant.added"
        } else {
            "project_studio.creator_grant.removed"
        },
        user_id,
        "",
    );
    Ok(ps(ProjectStudioPayload::CreatorGrantSetResult { ok }))
}

// =============================================================================
// Knowledge sources
// =============================================================================

fn sources_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let items = repository::list_sources(&pool).map_err(|e| db_error("sources_list", e))?;
    let ids: Vec<String> = items.iter().map(|i| i.record.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let sources = items
        .into_iter()
        .map(|item| source_to_wire(item, &names))
        .collect();
    Ok(ps(ProjectStudioPayload::SourcesListResponse { sources }))
}

#[allow(clippy::too_many_arguments)]
async fn source_upload_chunk_v1(
    ctx: &HandlerContext,
    project_id: &str,
    upload_id: &str,
    filename: &str,
    mime: &str,
    seq: u32,
    total_chunks: u32,
    bytes: &[u8],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;

    let org_id = org.org_id.clone();
    let user_id = org.user_id.clone();
    let project_id_owned = project_id.to_string();
    let dir = std::path::PathBuf::from(&record.dir_path);
    let upload_id_owned = upload_id.to_string();
    let filename = filename.to_string();
    let mime = mime.to_string();
    let bytes = bytes.to_vec();
    let outcome = tokio::task::spawn_blocking(move || {
        ingest::accept_upload_chunk(
            &org_id,
            &user_id,
            &project_id_owned,
            &dir,
            &upload_id_owned,
            &filename,
            &mime,
            seq,
            total_chunks,
            &bytes,
        )
    })
    .await
    .map_err(|_| ProtocolError::internal("upload task panicked"))?
    .map_err(|e| ProtocolError::bad_request(format!("upload rejected: {e}")))?;

    let (received_chunks, received_bytes, file_ref) = match outcome {
        ingest::UploadOutcome::Buffered {
            received_chunks,
            received_bytes,
        } => (received_chunks, received_bytes, None),
        ingest::UploadOutcome::Finalized {
            sha256,
            received_chunks,
            size_bytes,
        } => (received_chunks, size_bytes, Some(sha256)),
    };
    Ok(ps(ProjectStudioPayload::SourceUploadChunkResponse {
        upload_id: upload_id.to_string(),
        received_chunks,
        received_bytes,
        file_ref,
    }))
}

/// Builds the work list for a job and spawns it. Shared by create / update /
/// reingest.
#[allow(clippy::too_many_arguments)]
fn spawn_ingest_job(
    ctx: &HandlerContext,
    org: &OrgContext,
    record: &ProjectRecord,
    pool: &crate::db::DbPool,
    source_id: &str,
    kind: &str,
    config_json: &str,
    only_file: Option<&str>,
) -> Result<String, ProtocolError> {
    let files = repository::files_for_ingest(pool, source_id, only_file)
        .map_err(|e| db_error("files_for_ingest", e))?;
    if files.is_empty() {
        return Err(ProtocolError::bad_request("source has no files to ingest"));
    }
    let url = if kind == "url" {
        serde_json::from_str::<serde_json::Value>(config_json)
            .ok()
            .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
    } else {
        None
    };
    let work: Vec<ingest::FileWork> = files
        .iter()
        .map(|f| ingest::FileWork {
            file_id: f.file_id.clone(),
            path: f.path.clone(),
            sha256: f.sha256.clone(),
            mime: f.mime.clone(),
            payload: match &url {
                Some(u) => ingest::WorkPayload::Url(u.clone()),
                None => ingest::WorkPayload::Blob,
            },
        })
        .collect();

    let job_id = uuid::Uuid::new_v4().to_string();
    repository::create_ingest_job(pool, &job_id, source_id, work.len() as u32, &org.user_id)
        .map_err(|e| db_error("create_job", e))?;
    ingest::start_job(ingest::IngestTask {
        core_db: ctx.state.db.clone(),
        router: ctx.state.router.clone(),
        project_pool: pool.clone(),
        org_id: org.org_id.clone(),
        project_id: record.project_id.clone(),
        dir_path: std::path::PathBuf::from(&record.dir_path),
        source_id: source_id.to_string(),
        job_id: job_id.clone(),
        files: work,
    });
    Ok(job_id)
}

fn source_create_v1(
    ctx: &HandlerContext,
    project_id: &str,
    kind: &str,
    name: &str,
    config_json: &str,
    file_refs: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    match kind {
        "document" | "url" => {}
        "git" | "zip" | "api_spec" => {
            return Err(ProtocolError::bad_request(format!(
                "source kind '{kind}' is available from Phase 3"
            )))
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown kind '{other}'"
            )))
        }
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("source name is required"));
    }
    let config: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {e}")))?;

    // Resolve/validate the payload BEFORE the source row exists — a bad
    // file_ref or url must not leave an empty orphaned source behind.
    let mut document_files: Vec<(String, ingest::FileMeta)> = Vec::with_capacity(file_refs.len());
    let mut source_url: Option<&str> = None;
    match kind {
        "document" => {
            if file_refs.is_empty() {
                return Err(ProtocolError::bad_request(
                    "document source requires at least one uploaded file_ref",
                ));
            }
            for sha in file_refs {
                let meta = ingest::finalized_meta(&org.org_id, &org.user_id, project_id, sha)
                    .ok_or_else(|| {
                        ProtocolError::bad_request(format!(
                            "unknown file_ref '{sha}' (upload expired?)"
                        ))
                    })?;
                document_files.push((sha.clone(), meta));
            }
        }
        "url" => {
            source_url = Some(
                config
                    .get("url")
                    .and_then(|u| u.as_str())
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                    .ok_or_else(|| {
                        ProtocolError::bad_request("url source requires config_json {\"url\": ...}")
                    })?,
            );
        }
        _ => unreachable!("kind validated above"),
    }

    let pool = open_project_pool(project_id)?;
    let source_id = uuid::Uuid::new_v4().to_string();
    repository::create_source(&pool, &source_id, kind, name, config_json, &org.user_id)
        .map_err(|e| db_error("source_create", e))?;

    for (sha, meta) in &document_files {
        repository::upsert_source_file(
            &pool,
            &source_id,
            &meta.filename,
            sha,
            meta.size_bytes,
            &meta.mime,
        )
        .map_err(|e| db_error("source_file", e))?;
    }
    if let Some(url) = source_url {
        repository::upsert_source_file(&pool, &source_id, url, "", 0, "text/html")
            .map_err(|e| db_error("source_file", e))?;
    }

    let job_id = spawn_ingest_job(
        ctx,
        org,
        &record,
        &pool,
        &source_id,
        kind,
        config_json,
        None,
    )?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.created",
        "source",
        &source_id,
        &serde_json::json!({ "name": name, "kind": kind }).to_string(),
    );
    let _ = repository::touch_project(project_id);
    Ok(ps(ProjectStudioPayload::SourceCreateResponse {
        source_id,
        job_id,
    }))
}

fn source_update_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
    name: &str,
    config_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("source name is required"));
    }
    let config: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {e}")))?;

    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;

    let mut job_id = None;
    if source.kind == "url" {
        let new_url = config
            .get("url")
            .and_then(|u| u.as_str())
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .ok_or_else(|| {
                ProtocolError::bad_request("url source requires config_json {\"url\": ...}")
            })?;
        let old_url = serde_json::from_str::<serde_json::Value>(&source.config_json)
            .ok()
            .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()));
        if old_url.as_deref() != Some(new_url) {
            // URL changed: old page rows + their vectors are stale.
            let files = repository::files_for_ingest(&pool, source_id, None)
                .map_err(|e| db_error("files_for_ingest", e))?;
            for f in &files {
                ingest::delete_file_vectors(&ctx.state.db, &org.org_id, project_id, &f.file_id)
                    .map_err(|e| db_error("delete_vectors", e))?;
                let _ = repository::delete_source_file_row(&pool, &f.file_id);
            }
            repository::upsert_source_file(&pool, source_id, new_url, "", 0, "text/html")
                .map_err(|e| db_error("source_file", e))?;
            repository::update_source_meta(&pool, source_id, name, config_json)
                .map_err(|e| db_error("source_update", e))?;
            job_id = Some(spawn_ingest_job(
                ctx,
                org,
                &record,
                &pool,
                source_id,
                "url",
                config_json,
                None,
            )?);
        } else {
            repository::update_source_meta(&pool, source_id, name, config_json)
                .map_err(|e| db_error("source_update", e))?;
        }
    } else {
        repository::update_source_meta(&pool, source_id, name, config_json)
            .map_err(|e| db_error("source_update", e))?;
    }

    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.updated",
        "source",
        source_id,
        &serde_json::json!({ "name": name }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::SourceUpdateResponse {
        source_id: source_id.to_string(),
        job_id,
    }))
}

async fn source_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;

    // Stop any running job of this source and wait for it to reach a
    // terminal status before ripping its data out — cancel is cooperative
    // and a still-running job would keep writing files/vectors mid-delete.
    let mut source_jobs: Vec<String> = Vec::new();
    if let Ok(jobs) = repository::running_job_ids(&pool) {
        for job_id in jobs {
            if let Ok(Some(job)) = repository::get_ingest_job(&pool, &job_id) {
                if job.source_id == source_id {
                    ingest::signal_cancel(&job_id);
                    source_jobs.push(job_id);
                }
            }
        }
    }
    wait_for_jobs_terminal(&pool, &source_jobs).await?;

    let files = repository::files_for_ingest(&pool, source_id, None)
        .map_err(|e| db_error("files_for_ingest", e))?;
    // Cleanup-then-delete: vectors first, rows second, unreferenced blobs last.
    {
        let core_db = ctx.state.db.clone();
        let org_id = org.org_id.clone();
        let project_id_owned = project_id.to_string();
        let file_ids: Vec<String> = files.iter().map(|f| f.file_id.clone()).collect();
        tokio::task::spawn_blocking(move || {
            for file_id in &file_ids {
                ingest::delete_file_vectors(&core_db, &org_id, &project_id_owned, file_id)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| ProtocolError::internal("vector cleanup task panicked"))?
        .map_err(|e| db_error("delete_vectors", e))?;
    }
    let ok = repository::delete_source_rows(&pool, source_id)
        .map_err(|e| db_error("source_delete", e))?;
    for f in &files {
        if f.sha256.is_empty() {
            continue;
        }
        let refs = repository::sha_ref_count(&pool, &f.sha256)
            .map_err(|e| db_error("sha_ref_count", e))?;
        if refs == 0 {
            let blob = std::path::Path::new(&record.dir_path)
                .join("files")
                .join(&f.sha256);
            let _ = std::fs::remove_file(blob);
        }
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.deleted",
        "source",
        source_id,
        "{}",
    );
    Ok(ps(ProjectStudioPayload::SourceDeleteResult { ok }))
}

fn source_reingest_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
    file_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    let job_id = spawn_ingest_job(
        ctx,
        org,
        &record,
        &pool,
        source_id,
        &source.kind,
        &source.config_json,
        file_id,
    )?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.reingested",
        "source",
        source_id,
        &serde_json::json!({ "file_id": file_id }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::SourceReingestResponse { job_id }))
}

fn ingest_cancel_v1(
    ctx: &HandlerContext,
    project_id: &str,
    job_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    // Job must belong to THIS project's database — the registry is
    // process-global, so an unchecked id would let one project cancel
    // another's job.
    let job = repository::get_ingest_job(&pool, job_id)
        .map_err(|e| db_error("get_job", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    let ok = if job.finished_at.is_some() {
        false
    } else {
        ingest::signal_cancel(job_id)
    };
    Ok(ps(ProjectStudioPayload::IngestCancelResult { ok }))
}

fn ingest_status_v1(
    ctx: &HandlerContext,
    project_id: &str,
    job_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let job = repository::get_ingest_job(&pool, job_id)
        .map_err(|e| db_error("get_job", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    Ok(ps(ProjectStudioPayload::IngestStatusResponse {
        job: job_to_wire(&job),
    }))
}

// =============================================================================
// Source files
// =============================================================================

fn source_files_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
    offset: u32,
    limit: u32,
    filter: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 500);
    let (rows, total) = repository::list_source_files(&pool, source_id, offset, limit, filter)
        .map_err(|e| db_error("files_list", e))?;
    let files = rows
        .into_iter()
        .map(|f| SourceFileInfo {
            file_id: f.file_id,
            source_id: f.source_id,
            path: f.path,
            size_bytes: f.size_bytes,
            mime: f.mime,
            status: f.status,
            error: if f.error.is_empty() {
                None
            } else {
                Some(f.error)
            },
            chunk_count: f.chunk_count,
            updated_at: f.updated_at,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::SourceFilesListResponse {
        files,
        total,
    }))
}

async fn source_file_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    file_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let file = repository::get_source_file(&pool, file_id)
        .map_err(|e| db_error("get_file", e))?
        .ok_or_else(|| ProtocolError::not_found("file not found"))?;

    {
        let core_db = ctx.state.db.clone();
        let org_id = org.org_id.clone();
        let project_id_owned = project_id.to_string();
        let file_id_owned = file_id.to_string();
        tokio::task::spawn_blocking(move || {
            ingest::delete_file_vectors(&core_db, &org_id, &project_id_owned, &file_id_owned)
        })
        .await
        .map_err(|_| ProtocolError::internal("vector cleanup task panicked"))?
        .map_err(|e| db_error("delete_vectors", e))?;
    }
    let ok = repository::delete_source_file_row(&pool, file_id)
        .map_err(|e| db_error("file_delete", e))?;
    if ok && !file.sha256.is_empty() {
        let refs = repository::sha_ref_count(&pool, &file.sha256)
            .map_err(|e| db_error("sha_ref_count", e))?;
        if refs == 0 {
            let blob = std::path::Path::new(&record.dir_path)
                .join("files")
                .join(&file.sha256);
            let _ = std::fs::remove_file(blob);
        }
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "file.deleted",
        "file",
        file_id,
        &serde_json::json!({ "path": file.path }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::SourceFileDeleteResult { ok }))
}

async fn source_file_preview_v1(
    ctx: &HandlerContext,
    project_id: &str,
    file_id: &str,
    max_bytes: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let file = repository::get_source_file(&pool, file_id)
        .map_err(|e| db_error("get_file", e))?
        .ok_or_else(|| ProtocolError::not_found("file not found"))?;
    if file.sha256.is_empty() {
        return Err(ProtocolError::bad_request(
            "this file has no stored content to preview",
        ));
    }

    let cap = max_bytes.clamp(1, PREVIEW_MAX_BYTES) as usize;
    let blob = std::path::Path::new(&record.dir_path)
        .join("files")
        .join(&file.sha256);
    let mime = file.mime.clone();
    let path = file.path.clone();
    let size = file.size_bytes;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<(String, bool)> {
        use std::io::Read;
        let mut f = std::fs::File::open(&blob)?;
        let mut buf = vec![0u8; cap];
        let mut read = 0usize;
        loop {
            let n = f.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
            if read == cap {
                break;
            }
        }
        buf.truncate(read);
        if !crate::project_studio::ingest::is_text_preview(&path, &mime, &buf) {
            anyhow::bail!("preview available only for text files");
        }
        let content = String::from_utf8_lossy(&buf).into_owned();
        Ok((content, size > read as u64))
    })
    .await
    .map_err(|_| ProtocolError::internal("preview task panicked"))?;

    match result {
        Ok((content, truncated)) => Ok(ps(ProjectStudioPayload::SourceFilePreviewResponse {
            content,
            truncated,
            mime: file.mime,
        })),
        Err(e) => Err(ProtocolError::bad_request(e.to_string())),
    }
}

// =============================================================================
// Knowledge-base search
// =============================================================================

async fn kb_search_v1(
    ctx: &HandlerContext,
    project_id: &str,
    query: &str,
    source_ids: &[String],
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let query = query.trim();
    if query.is_empty() {
        return Err(ProtocolError::bad_request("query is required"));
    }
    let limit = limit.clamp(1, 50) as usize;

    let vectors = ingest::embed_texts(&ctx.state.router, vec![query.to_string()])
        .await
        .map_err(|e| ProtocolError::internal(format!("query embedding: {e}")))?;
    let query_vec = vectors
        .into_iter()
        .next()
        .ok_or_else(|| ProtocolError::internal("query embedding empty"))?;

    let mgr = crate::services::vector_namespace_manager(&ctx.state.db);
    let scope = ingest::vector_scope(project_id);
    let backend = match mgr.get(&org.org_id, &scope, ingest::VECTOR_NAMESPACE) {
        Ok(b) => b,
        Err(VectorError::NamespaceNotFound { .. }) => {
            return Ok(ps(ProjectStudioPayload::KbSearchResponse { hits: vec![] }))
        }
        Err(e) => return Err(ProtocolError::internal(format!("vector namespace: {e}"))),
    };
    let filter = if source_ids.is_empty() {
        None
    } else {
        Some(Filter::In(
            "source_id".to_string(),
            source_ids
                .iter()
                .map(|s| FieldValue::Str(s.clone()))
                .collect(),
        ))
    };
    let output_fields: Vec<String> = [
        "doc_id",
        "chunk_index",
        "text",
        "source_id",
        "path",
        "location",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let raw_hits = backend
        .search(&query_vec, limit, filter.as_ref(), &output_fields)
        .map_err(|e| ProtocolError::internal(format!("vector search: {e}")))?;

    // Source name/kind live in project.db — build a lookup once.
    let pool = open_project_pool(project_id)?;
    let sources = repository::list_sources(&pool).map_err(|e| db_error("sources_list", e))?;
    let source_meta: HashMap<String, (String, String)> = sources
        .into_iter()
        .map(|s| (s.record.source_id.clone(), (s.record.name, s.record.kind)))
        .collect();

    let hits = raw_hits
        .into_iter()
        .map(|hit| {
            let mut fields: HashMap<String, String> = HashMap::new();
            let mut chunk_index: u32 = 0;
            for f in hit.fields {
                match f.value {
                    FieldValue::Str(s) => {
                        fields.insert(f.name, s);
                    }
                    FieldValue::Int(i) if f.name == "chunk_index" => {
                        chunk_index = i.max(0) as u32;
                    }
                    _ => {}
                }
            }
            let source_id = fields.remove("source_id").unwrap_or_default();
            let (source_name, source_kind) = source_meta
                .get(&source_id)
                .cloned()
                .unwrap_or_else(|| (source_id.clone(), String::new()));
            let text = fields.remove("text").unwrap_or_default();
            let snippet: String = text.chars().take(400).collect();
            let file_path = fields.remove("path").unwrap_or_default();
            let location = fields.remove("location").unwrap_or_default();
            let file_id = fields.remove("doc_id").unwrap_or_default();
            let metadata_json = serde_json::json!({
                "source_id": source_id,
                "file_id": file_id,
                "path": file_path,
                "chunk_index": chunk_index,
                "location": location,
            })
            .to_string();
            KbHit {
                source_id,
                source_name,
                source_kind,
                file_id,
                file_path,
                chunk_index,
                score: hit.score,
                snippet,
                location,
                metadata_json,
            }
        })
        .collect();
    Ok(ps(ProjectStudioPayload::KbSearchResponse { hits }))
}

// =============================================================================
// Overview + activity
// =============================================================================

fn overview_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let kpis = repository::project_kpis(&pool).map_err(|e| db_error("kpis", e))?;
    let member_count =
        repository::member_count(project_id).map_err(|e| db_error("member_count", e))?;
    let my_chat_count =
        repository::count_chats(project_id, &org.user_id).map_err(|e| db_error("chat_count", e))?;
    let (entries, _has_more) =
        repository::list_activity(&pool, None, 20).map_err(|e| db_error("activity", e))?;
    let ids: Vec<String> = entries.iter().map(|e| e.actor_user_id.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    Ok(ps(ProjectStudioPayload::OverviewResponse {
        kpis: OverviewKpis {
            sources_total: kpis.sources_total,
            sources_ready: kpis.sources_ready,
            files_total: kpis.files_total,
            chunks_total: kpis.chunks_total,
            member_count,
            open_ingest_jobs: kpis.open_ingest_jobs,
            my_chat_count,
            // F2 objects (cases/suites/runs/tasks/generations) are not yet
            // queried here — the F2 backend step computes these for real.
            cases_total: 0,
            cases_approved: 0,
            suites_total: 0,
            runs_open: 0,
            my_run_items_pending: 0,
            tasks_open: 0,
            defects_open: 0,
            generations_running: 0,
        },
        activity: activity_to_wire(entries, &names),
    }))
}

fn activity_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    before_id: Option<i64>,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    let (entries, has_more) = repository::list_activity(&pool, before_id, limit)
        .map_err(|e| db_error("activity_list", e))?;
    let ids: Vec<String> = entries.iter().map(|e| e.actor_user_id.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    Ok(ps(ProjectStudioPayload::ActivityListResponse {
        entries: activity_to_wire(entries, &names),
        has_more,
    }))
}

// =============================================================================
// Chats — private per user: every repository call filters by the caller
// =============================================================================

fn chat_to_wire(chat: crate::project_studio::models::ChatRecord) -> ChatInfo {
    ChatInfo {
        chat_id: chat.chat_id,
        title: chat.title,
        last_message_preview: String::new(),
        created_at: chat.created_at,
        updated_at: chat.updated_at,
    }
}

fn chats_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let rows =
        repository::list_chats(project_id, &org.user_id).map_err(|e| db_error("chats_list", e))?;
    // Previews come from conversation_messages in the CORE db by session_id.
    let mut chats = Vec::with_capacity(rows.len());
    for chat in rows {
        let preview = last_message_preview(ctx, &chat.session_id);
        let mut wire = chat_to_wire(chat);
        wire.last_message_preview = preview;
        chats.push(wire);
    }
    Ok(ps(ProjectStudioPayload::ChatsListResponse { chats }))
}

fn last_message_preview(ctx: &HandlerContext, session_id: &str) -> String {
    let Ok(conn) = ctx.state.db.read() else {
        return String::new();
    };
    conn.query_row(
        "SELECT COALESCE(content, '') FROM conversation_messages \
         WHERE session_id = ?1 AND role IN ('user','assistant') \
         ORDER BY seq DESC LIMIT 1",
        rusqlite::params![session_id],
        |row| row.get::<_, String>(0),
    )
    .map(|s| s.chars().take(120).collect())
    .unwrap_or_default()
}

fn chat_create_v1(
    ctx: &HandlerContext,
    project_id: &str,
    title: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_project(org, project_id, ProjectRole::Viewer)?;
    require_active(&record)?;
    // Chats are personal rows keyed by the caller — an org admin inspecting a
    // foreign project (role None) must not create content in it.
    if role.is_none() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "requires project membership",
        ));
    }
    let title = title.trim();
    let title = if title.is_empty() { "Nowy czat" } else { title };
    let chat = repository::create_chat(project_id, &org.user_id, title)
        .map_err(|e| db_error("chat_create", e))?;
    Ok(ps(ProjectStudioPayload::ChatCreateResponse {
        chat: chat_to_wire(chat),
    }))
}

fn chat_rename_v1(
    ctx: &HandlerContext,
    project_id: &str,
    chat_id: &str,
    title: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    require_active(&record)?;
    let title = title.trim();
    if title.is_empty() {
        return Err(ProtocolError::bad_request("title is required"));
    }
    let ok = repository::rename_chat(project_id, chat_id, &org.user_id, title)
        .map_err(|e| db_error("chat_rename", e))?;
    Ok(ps(ProjectStudioPayload::ChatRenameResult { ok }))
}

fn chat_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    chat_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    require_active(&record)?;
    let ok = repository::delete_chat(project_id, chat_id, &org.user_id)
        .map_err(|e| db_error("chat_delete", e))?;
    Ok(ps(ProjectStudioPayload::ChatDeleteResult { ok }))
}

fn chat_history_v1(
    ctx: &HandlerContext,
    project_id: &str,
    chat_id: &str,
    before_message_id: Option<&str>,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    // Ownership check is part of the lookup: another user's chat_id yields
    // NotFound, never someone else's history.
    let chat = repository::get_chat(project_id, chat_id, &org.user_id)
        .map_err(|e| db_error("get_chat", e))?
        .ok_or_else(|| ProtocolError::not_found("chat not found"))?;

    let limit = limit.clamp(1, 200);
    let before: Option<i64> = match before_message_id {
        Some(raw) => Some(
            raw.parse::<i64>()
                .map_err(|_| ProtocolError::bad_request("invalid before_message_id"))?,
        ),
        None => None,
    };
    let conn = ctx
        .state
        .db
        .read()
        .map_err(|e| ProtocolError::internal(format!("core db read: {e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, role, COALESCE(content, ''), COALESCE(citations_json, ''), created_at \
             FROM conversation_messages \
             WHERE session_id = ?1 AND role IN ('user','assistant') \
               AND (?2 IS NULL OR id < ?2) \
             ORDER BY id DESC LIMIT ?3",
        )
        .map_err(|e| ProtocolError::internal(format!("history query: {e}")))?;
    let rows = stmt
        .query_map(
            rusqlite::params![chat.session_id, before, (limit as i64) + 1],
            |row| {
                Ok(ChatMessageWire {
                    message_id: row.get::<_, i64>(0)?.to_string(),
                    role: row.get(1)?,
                    content: row.get(2)?,
                    citations_json: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .map_err(|e| ProtocolError::internal(format!("history query: {e}")))?;
    let mut messages: Vec<ChatMessageWire> = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| ProtocolError::internal(format!("history rows: {e}")))?;
    let has_more = messages.len() as u32 > limit;
    messages.truncate(limit as usize);
    // Newest-first from SQL → chronological for the UI.
    messages.reverse();
    Ok(ps(ProjectStudioPayload::ChatHistoryResponse {
        messages,
        has_more,
    }))
}

// =============================================================================
// Settings + tags
// =============================================================================

fn settings_get_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;

    let agents_map: HashMap<String, String> = repository::get_setting(&pool, "agents")
        .map_err(|e| db_error("settings_get", e))?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut agents = Vec::new();
    for function in AGENT_FUNCTIONS {
        let agent_id = agents_map.get(*function).cloned().unwrap_or_default();
        let (agent_name, model_label) = if agent_id.is_empty() {
            (String::new(), String::new())
        } else {
            repository::resolve_agent_label(&agent_id).unwrap_or_default()
        };
        agents.push(ProjectAgentBinding {
            function: function.to_string(),
            agent_id,
            agent_name,
            model_label,
        });
    }

    let tags = repository::list_tags(&pool)
        .map_err(|e| db_error("tags", e))?
        .into_iter()
        .map(|t| TagInfo {
            tag_id: t.tag_id,
            name: t.name,
            usage_count: 0,
        })
        .collect();

    Ok(ps(ProjectStudioPayload::SettingsGetResponse {
        settings: ProjectSettings {
            name: record.name,
            description: record.description,
            modules: parse_modules_json(&record.modules_json),
            agents,
            tags,
        },
    }))
}

fn settings_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    agents_json: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;

    if name.is_some() || description.is_some() {
        let new_name = name.map(str::trim).unwrap_or(&record.name);
        if new_name.is_empty() || new_name.len() > 200 {
            return Err(ProtocolError::bad_request("project name is required"));
        }
        let new_desc = description.map(str::trim).unwrap_or(&record.description);
        repository::update_project_name_desc(&org.org_id, project_id, new_name, new_desc).map_err(
            |e| {
                map_unique(
                    "settings_save",
                    "a project with this name already exists",
                    e,
                )
            },
        )?;
    }
    if let Some(raw) = agents_json {
        // Wire format is the array of bindings from the technical design
        // ([{"function": ..., "agent_id": ...}]); storage stays the canonical
        // function→agent_id map that SettingsGet reads back.
        #[derive(serde::Deserialize)]
        struct AgentBindingInput {
            function: String,
            #[serde(default)]
            agent_id: String,
        }
        let bindings: Vec<AgentBindingInput> = serde_json::from_str(raw)
            .map_err(|e| ProtocolError::bad_request(format!("invalid agents_json: {e}")))?;
        let mut map: HashMap<String, String> = HashMap::with_capacity(bindings.len());
        for binding in bindings {
            if !AGENT_FUNCTIONS.contains(&binding.function.as_str()) {
                return Err(ProtocolError::bad_request(format!(
                    "unknown agent function '{}'",
                    binding.function
                )));
            }
            if map.insert(binding.function.clone(), binding.agent_id).is_some() {
                return Err(ProtocolError::bad_request(format!(
                    "duplicate agent function '{}'",
                    binding.function
                )));
            }
        }
        let canonical = serde_json::to_string(&map)
            .map_err(|e| ProtocolError::internal(format!("agents serialize: {e}")))?;
        repository::set_setting(&pool, "agents", &canonical)
            .map_err(|e| db_error("settings_save", e))?;
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "settings.saved",
        "settings",
        "",
        "{}",
    );
    Ok(ps(ProjectStudioPayload::SettingsSaveResult { ok: true }))
}

fn tag_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    tag_id: Option<&str>,
    name: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ProtocolError::bad_request("tag name is required"));
    }
    let pool = open_project_pool(project_id)?;
    let tag_id = repository::upsert_tag(&pool, tag_id, name, &org.user_id)
        .map_err(|e| map_unique("tag_save", "a tag with this name already exists", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "tag.saved",
        "tag",
        &tag_id,
        &serde_json::json!({ "name": name }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::TagSaveResponse { tag_id }))
}

fn tag_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    tag_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let ok = repository::delete_tag(&pool, tag_id).map_err(|e| db_error("tag_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "tag.deleted",
            "tag",
            tag_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::TagDeleteResult { ok }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owner-gate refuses a plain member, admits owner; non-member maps to
    /// NotFound (existence must not leak) — exercised end-to-end against a
    /// temp central DB in `require_project_role_hierarchy_and_not_found`.
    #[test]
    fn require_project_role_hierarchy_and_not_found() {
        // Initialise the central registry in a tempdir (OnceLock — first test
        // to run wins; unique ids keep tests independent).
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));

        let project_id = format!("gate-{}", uuid::Uuid::new_v4());
        repository::create_project(
            &project_id,
            "org-t",
            &format!("Projekt {project_id}"),
            "",
            "custom",
            "[\"knowledge\"]",
            "owner-1",
            "/tmp/none",
            &[
                ("manager-1".to_string(), "manager".to_string()),
                ("editor-1".to_string(), "editor".to_string()),
                ("tester-1".to_string(), "tester".to_string()),
                ("viewer-1".to_string(), "viewer".to_string()),
            ],
        )
        .expect("create project");

        let org = |user: &str, admin: bool| OrgContext {
            user_id: user.to_string(),
            org_id: "org-t".to_string(),
            role_id: "role-x".to_string(),
            permissions: if admin {
                [PERM_READ.to_string(), PERM_ADMIN.to_string()]
                    .into_iter()
                    .collect()
            } else {
                [PERM_READ.to_string()].into_iter().collect()
            },
        };

        // Hierarchy: editor passes editor gate, tester does not.
        assert!(require_project(&org("editor-1", false), &project_id, ProjectRole::Editor).is_ok());
        let denied = require_project(&org("tester-1", false), &project_id, ProjectRole::Editor)
            .expect_err("tester below editor");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        // Manager passes editor + manager, fails owner.
        assert!(
            require_project(&org("manager-1", false), &project_id, ProjectRole::Manager).is_ok()
        );
        let denied = require_project(&org("manager-1", false), &project_id, ProjectRole::Owner)
            .expect_err("manager is not owner");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        // Owner passes everything.
        assert!(require_project(&org("owner-1", false), &project_id, ProjectRole::Owner).is_ok());
        // Viewer gate admits every member.
        assert!(require_project(&org("viewer-1", false), &project_id, ProjectRole::Viewer).is_ok());

        // Non-member → NotFound (not PolicyDenied) so existence does not leak.
        let err = require_project(&org("stranger", false), &project_id, ProjectRole::Viewer)
            .expect_err("stranger");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
        // Admin override: viewer + owner tiers only; editor tier still denied.
        assert!(require_project(&org("stranger", true), &project_id, ProjectRole::Viewer).is_ok());
        assert!(require_project(&org("stranger", true), &project_id, ProjectRole::Owner).is_ok());
        let err = require_project(&org("stranger", true), &project_id, ProjectRole::Editor)
            .expect_err("admin has no content-mutation override");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        // Wrong org → NotFound even for a member.
        let mut foreign = org("owner-1", false);
        foreign.org_id = "org-other".to_string();
        let err =
            require_project(&foreign, &project_id, ProjectRole::Viewer).expect_err("foreign org");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    /// Archived projects are read-only: `require_active` (called by every
    /// mutation handler after the role gate) rejects with BadRequest and
    /// unarchiving restores mutability.
    #[test]
    fn archived_project_rejects_mutations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));

        let project_id = format!("arch-{}", uuid::Uuid::new_v4());
        repository::create_project(
            &project_id,
            "org-a",
            &format!("Projekt {project_id}"),
            "",
            "custom",
            "[\"knowledge\"]",
            "owner-a",
            "/tmp/none",
            &[],
        )
        .expect("create project");

        assert!(repository::set_project_archived("org-a", &project_id, true).expect("archive"));
        let record = repository::get_project("org-a", &project_id)
            .expect("get")
            .expect("record");
        let err = require_active(&record).expect_err("archived is read-only");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert_eq!(err.message, "project is archived");

        assert!(repository::set_project_archived("org-a", &project_id, false).expect("unarchive"));
        let record = repository::get_project("org-a", &project_id)
            .expect("get")
            .expect("record");
        assert!(require_active(&record).is_ok());
    }
}
