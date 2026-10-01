// =============================================================================
// Plik: dispatch/project_studio.rs
// Opis: Handlery binarnego API Project Studio ("Projekty") — rejestr projektów,
//       członkowie i granty tworzenia, źródła wiedzy (chunkowany upload +
//       joby ingestu), pliki źródeł, przeszukiwanie bazy wiedzy, przegląd i
//       aktywność, prywatne czaty per użytkownik oraz ustawienia/tagi.
//       Streaming ingestu i stream czatu żyją w stream_handlers.rs; czat
//       projektu jedzie zaseedowaną powłoką `core:rag-query`.
// Przykład: ProjectStudioPayload::ProjectsListRequest → ProjectsListResponse.
// =============================================================================

use std::collections::{HashMap, HashSet};

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::project_studio::access::{
    ProjectAccessPayload, ProjectAccessWire, ProjectArea, ProjectFunctionWire,
    ProjectPermissionLevel,
};
use tentaflow_protocol::project_studio::{
    ActivityEntry, ArchiveInventoryWire, ArtifactRef, AttachmentOwnerKind, AttachmentUsageFileWire,
    AttachmentWire, BuildProfileWire, CaseVersionInfo, ChatInfo, ChatMessageWire, CreatorGrantInfo,
    CsvImportError, EnvApprovalItem, EnvironmentInfo, GenerationRunInfo, IngestJobWire, KbHit,
    MemberInfo, MemberInputWire, MlLinkInfo, MlProjectSummaryWire, MlRoleMapEntry,
    MlSyncOutcomeWire, MyWorkEntry, NotificationWire, OverviewKpis, PerfStatsWire,
    PerfTimelinePoint, ProjectAgentBinding, ProjectInfo, ProjectSettings, ProjectStudioPayload,
    RunAssignmentWire, RunItemWire, RunStepWire, RunnerInfo, RunnerToolchain, ScheduleInfo,
    ScheduleRunWire, SourceFileInfo, SourceInfo, SuiteCaseRef, SuiteInfo, TagInfo,
    TaskAttachmentUsageWire, TaskCommentWire, TaskDetail, TaskEventWire, TaskInfo, TaskLinkWire,
    TaskStatusDurationWire, TaskTypeWire, TestCaseDetail, TestCaseInfo, TestRunInfo,
    TestRunItemAutoWire, UserRefWire,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::project_studio::models::{
    source_write_area, ActivityRecord, CaseListItem, EnvironmentRecord, GenerationRunRecord,
    IngestJobRecord, MemberInput, ProjectRecord, RunCounts, RunItemRecord, RunRecord,
    RunStepRecord, SourceListItem, TaskCommentRecord, TaskRecord,
};
use crate::project_studio::{
    activity, api_spec, archive, auto_runs, build_profiles, environments, generation, git_source,
    ingest, media, ml_link, notifications, project_db, reports, repository, runs, schedules, tasks,
    tests as ps_tests, zip_source,
};
use crate::services::rbac::OrgContext;
use crate::services::vector::error::VectorError;
use tentaflow_sdk_spec::{FieldValue, Filter};

const PERM_READ: &str = "project_studio.read";
const PERM_ADMIN: &str = "project_studio.admin";

use crate::project_studio::{VALID_MODULES, VALID_TEMPLATES};
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

const PACKAGE_ID: &str = "projekty";

// App availability and project membership are separate grants.
pub(crate) fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    super::app_gate::require_app_permission(ctx, PACKAGE_ID, PERM_READ)?;
    Ok(org)
}

fn require_admin(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    super::app_gate::require_app_permission(ctx, PACKAGE_ID, PERM_ADMIN)?;
    Ok(org)
}

/// The admin-override tier of `require_project` (inspection outside
/// membership, orphan takeover). Decided by the matrix — org admins pass via
/// the checker's admin bypass, and the grant is delegable to non-admins.
pub(crate) fn is_admin(ctx: &HandlerContext) -> bool {
    super::app_gate::require_app_permission(ctx, PACKAGE_ID, PERM_ADMIN).is_ok()
}

/// Boolean read gate for the stream guards (they refuse with a uniform end
/// frame instead of a protocol error, so they need a predicate, not a Result).
pub(crate) fn has_read(ctx: &HandlerContext) -> bool {
    super::app_gate::require_app_permission(ctx, PACKAGE_ID, PERM_READ).is_ok()
}

fn db_error(scope: &str, error: impl std::fmt::Display) -> ProtocolError {
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

pub(crate) fn require_project_access(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
) -> Result<(ProjectRecord, ProjectAccessWire), ProtocolError> {
    project_db::validate_project_id(project_id)
        .map_err(|_| ProtocolError::bad_request("invalid project_id"))?;
    let record = repository::get_project(&org.org_id, project_id)
        .map_err(|e| db_error("get_project", e))?
        .ok_or_else(not_found)?;
    let access = repository::project_access(&record, &org.user_id, is_admin(ctx))
        .map_err(|e| db_error("project_access", e))?;
    if !access.has_access {
        return Err(not_found());
    }
    Ok((record, access))
}

pub(crate) fn require_project(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    area: ProjectArea,
    minimum: ProjectPermissionLevel,
) -> Result<(ProjectRecord, ProjectAccessWire), ProtocolError> {
    let (record, access) = require_project_access(ctx, org, project_id)?;
    if !access.allows(area, minimum) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project area access denied",
        ));
    }
    Ok((record, access))
}

fn require_project_admin(access: &ProjectAccessWire) -> Result<(), ProtocolError> {
    if !access.can_manage_members {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project administration required",
        ));
    }
    Ok(())
}

fn task_access(access: &ProjectAccessWire, minimum: ProjectPermissionLevel) -> bool {
    access.allows(ProjectArea::Tasks, minimum) || access.allows(ProjectArea::Board, minimum)
}

pub(crate) fn require_task_access(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    minimum: ProjectPermissionLevel,
) -> Result<(ProjectRecord, ProjectAccessWire), ProtocolError> {
    let (record, access) = require_project_access(ctx, org, project_id)?;
    if !task_access(&access, minimum) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project task access denied",
        ));
    }
    Ok((record, access))
}

fn require_owner(access: &ProjectAccessWire) -> Result<(), ProtocolError> {
    if !access.is_owner && !access.app_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project ownership required",
        ));
    }
    Ok(())
}

fn member_inputs(
    org: &OrgContext,
    members: &[MemberInputWire],
    catalogue: &[ProjectFunctionWire],
) -> Result<Vec<MemberInput>, ProtocolError> {
    if members.len() > 200 {
        return Err(ProtocolError::bad_request(
            "members must contain at most 200 users",
        ));
    }
    let mut seen = HashSet::new();
    members
        .iter()
        .map(|member| {
            if !member.role.is_empty() {
                return Err(ProtocolError::bad_request("project roles are unsupported"));
            }
            if !seen.insert(&member.user_id) {
                return Err(ProtocolError::bad_request("duplicate member user_id"));
            }
            if !repository::is_org_member(&org.org_id, &member.user_id)
                .map_err(|e| db_error("is_org_member", e))?
            {
                return Err(ProtocolError::bad_request(
                    "user is not a member of this organization",
                ));
            }
            crate::project_studio::models::validate_member_input(
                &MemberInput {
                    user_id: member.user_id.clone(),
                    functions: member.functions.clone(),
                    project_admin: member.project_admin,
                    expires_at: member.expires_at.clone(),
                },
                catalogue,
                chrono::Utc::now(),
            )
            .map_err(|e| ProtocolError::bad_request(e.to_string()))
        })
        .collect()
}

fn record_access_mutation(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    action: &str,
    object_id: &str,
    details: &str,
) {
    if let Ok(pool) = project_db::open(project_id) {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            action,
            "member",
            object_id,
            details,
        );
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        &format!("project_studio.{action}"),
        project_id,
        details,
    );
}

fn access_dispatch(
    ctx: &HandlerContext,
    payload: &ProjectAccessPayload,
) -> Result<MessageBody, ProtocolError> {
    use ProjectAccessPayload as A;
    match payload {
        A::CatalogueGetRequest { project_id } => {
            let org = require_read(ctx)?;
            require_project_access(ctx, org, project_id)?;
            let functions = repository::list_functions(project_id)
                .map_err(|e| db_error("function_catalogue", e))?;
            Ok(ps(ProjectStudioPayload::Access(A::CatalogueGetResponse {
                functions,
            })))
        }
        A::FunctionSaveRequest {
            project_id,
            function,
        } => function_save(ctx, project_id, function),
        A::FunctionDeleteRequest {
            project_id,
            function_id,
        } => function_delete(ctx, project_id, function_id),
        A::MemberAccessSetRequest {
            project_id,
            user_id,
            functions,
            project_admin,
            expires_at,
        } => member_access_set(
            ctx,
            project_id,
            user_id,
            functions,
            *project_admin,
            expires_at.as_deref(),
        ),
        _ => Err(ProtocolError::bad_request(
            "expected project access request",
        )),
    }
}

fn function_save(
    ctx: &HandlerContext,
    project_id: &str,
    function: &ProjectFunctionWire,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_project_admin(&access)?;
    require_active(&record)?;
    let catalogue =
        repository::list_functions(project_id).map_err(|e| db_error("function_catalogue", e))?;
    let ok = repository::save_function(project_id, function)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if ok {
        let before = catalogue
            .iter()
            .find(|old| old.function_id == function.function_id);
        let after = repository::list_functions(project_id)
            .map_err(|e| db_error("function_catalogue", e))?
            .into_iter()
            .find(|saved| saved.function_id == function.function_id);
        record_access_mutation(
            ctx,
            org,
            project_id,
            "function.saved",
            &function.function_id,
            &serde_json::json!({ "before": before, "after": after }).to_string(),
        );
        spawn_ml_permission_sync(project_id);
    }
    Ok(ps(ProjectStudioPayload::Access(
        ProjectAccessPayload::FunctionSaveResult { ok },
    )))
}

fn function_delete(
    ctx: &HandlerContext,
    project_id: &str,
    function_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_project_admin(&access)?;
    require_active(&record)?;
    let before = repository::list_functions(project_id)
        .map_err(|e| db_error("function_catalogue", e))?
        .into_iter()
        .find(|function| function.function_id == function_id);
    let ok = repository::delete_function(project_id, function_id)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if ok {
        record_access_mutation(
            ctx,
            org,
            project_id,
            "function.deleted",
            function_id,
            &serde_json::json!({ "before": before }).to_string(),
        );
        spawn_ml_permission_sync(project_id);
    }
    Ok(ps(ProjectStudioPayload::Access(
        ProjectAccessPayload::FunctionDeleteResult { ok },
    )))
}

/// Archived projects are read-only: every mutation handler short-circuits
/// here right after the access gate. Only reads, unarchive (ProjectArchive)
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

/// Validates a client-supplied module list into the canonical registry form.
fn normalize_modules(modules: &[String]) -> Result<Vec<String>, ProtocolError> {
    let mut out: Vec<String> = Vec::with_capacity(modules.len() + 1);
    for module in modules {
        if !VALID_MODULES.contains(&module.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "unknown module '{module}'"
            )));
        }
        if !out.iter().any(|m| m == module) {
            out.push(module.clone());
        }
    }
    Ok(out)
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
    access: ProjectAccessWire,
    owner_names: &HashMap<String, (String, String)>,
) -> Result<ProjectInfo, ProtocolError> {
    let member_count =
        repository::member_count(&record.project_id).map_err(|e| db_error("member_count", e))?;
    let (source_count, sources_ready) =
        if access.allows(ProjectArea::Knowledge, ProjectPermissionLevel::Read) {
            repository::read_source_counts(&record.dir_path)
        } else {
            (0, 0)
        };
    Ok(ProjectInfo {
        project_id: record.project_id.clone(),
        key_prefix: record.key_prefix.clone(),
        key_prefix_locked: tasks::project_key_prefix_locked(&open_project_pool(
            &record.project_id,
        )?)
        .map_err(|e| db_error("key_prefix_locked", e))?,
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
        my_role: None,
        access,
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
        P::Access(access) => access_dispatch(ctx, access),
        P::ProjectsListRequest { include_archived } => projects_list_v1(ctx, *include_archived),
        P::ProjectCreateRequest {
            name,
            description,
            template,
            modules,
            key_prefix,
            members,
        } => project_create_v1(
            ctx,
            name,
            description,
            template,
            modules,
            key_prefix,
            members,
        ),
        P::ProjectGetRequest { project_id } => project_get_v1(ctx, project_id),
        P::ProjectUpdateRequest {
            project_id,
            name,
            description,
            key_prefix,
        } => project_update_v1(ctx, project_id, name, description, key_prefix.as_deref()),
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
        } => {
            let _ = (project_id, user_id, role);
            Err(ProtocolError::bad_request(
                "project roles are unsupported; use member access",
            ))
        }
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
        } => source_create_v1(ctx, project_id, kind, name, config_json, file_refs).await,
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
            modules,
            graph_extraction,
            key_prefix,
        } => settings_save_v1(
            ctx,
            project_id,
            name.as_deref(),
            description.as_deref(),
            agents_json.as_deref(),
            modules.as_deref(),
            *graph_extraction,
            key_prefix.as_deref(),
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
        // ---- F2: manual tests, runs, tasks, generation, notifications ----
        P::CasesListRequest {
            project_id,
            kind,
            status,
            priority,
            tag_id,
            origin,
            search,
            offset,
            limit,
        } => cases_list_v1(
            ctx, project_id, kind, status, priority, tag_id, origin, search, *offset, *limit,
        ),
        P::CaseGetRequest {
            project_id,
            case_id,
            include_versions,
        } => case_get_v1(ctx, project_id, case_id, *include_versions),
        P::CaseSaveRequest {
            project_id,
            case_id,
            kind,
            title,
            priority,
            content_json,
            tag_ids,
            linked_source_ids,
            attachments_json,
            expected_version,
            change_note,
        } => case_save_v1(
            ctx,
            project_id,
            case_id.as_deref(),
            kind,
            title,
            priority,
            content_json,
            tag_ids,
            linked_source_ids,
            attachments_json,
            *expected_version,
            change_note,
        ),
        P::CaseStatusSetRequest {
            project_id,
            case_id,
            status,
            reason,
        } => case_status_set_v1(ctx, project_id, case_id, status, reason),
        P::CasesBulkStatusRequest {
            project_id,
            case_ids,
            status,
            reason,
        } => cases_bulk_status_v1(ctx, project_id, case_ids, status, reason),
        P::CaseDuplicateRequest {
            project_id,
            case_id,
        } => case_duplicate_v1(ctx, project_id, case_id),
        P::CaseDeleteRequest {
            project_id,
            case_id,
        } => case_delete_v1(ctx, project_id, case_id),
        P::CaseVersionGetRequest {
            project_id,
            case_id,
            version,
        } => case_version_get_v1(ctx, project_id, case_id, *version),
        P::CaseRestoreVersionRequest {
            project_id,
            case_id,
            version,
            expected_version,
        } => case_restore_version_v1(ctx, project_id, case_id, *version, *expected_version),
        P::CasesImportCsvRequest {
            project_id,
            csv_text,
            dry_run,
        } => cases_import_csv_v1(ctx, project_id, csv_text, *dry_run),
        P::AttachmentGetRequest {
            project_id,
            owner_kind,
            owner_id,
            step_index,
            sha256,
            offset,
            max_bytes,
            preview,
        } => {
            let owner = media::AttachmentOwner {
                kind: owner_kind.clone(),
                id: owner_id.clone(),
                step_index: *step_index,
            };
            attachment_get_v1(
                ctx, project_id, &owner, sha256, *offset, *max_bytes, *preview,
            )
            .await
        }
        P::AttachmentUsageRequest {
            project_id,
            offset,
            limit,
        } => attachment_usage(ctx, project_id, *offset, *limit).await,
        P::AttachmentUploadStatusRequest {
            project_id,
            upload_id,
        } => attachment_upload_status(ctx, project_id, upload_id),
        P::AttachmentUploadChunkRequest {
            project_id,
            upload_id,
            filename,
            mime,
            sha256,
            total_size,
            offset,
            bytes,
        } => {
            attachment_upload_chunk(
                ctx,
                project_id,
                upload_id,
                filename,
                mime,
                sha256,
                *total_size,
                *offset,
                bytes,
            )
            .await
        }
        P::AttachmentUploadCancelRequest {
            project_id,
            upload_id,
        } => attachment_upload_cancel(ctx, project_id, upload_id),
        P::AttachmentPreviewRequest {
            project_id,
            owner_kind,
            owner_id,
            step_index,
            sha256,
            retry,
        } => {
            let owner = media::AttachmentOwner {
                kind: owner_kind.clone(),
                id: owner_id.clone(),
                step_index: *step_index,
            };
            attachment_preview(ctx, project_id, &owner, sha256, *retry)
        }
        P::SuitesListRequest { project_id } => suites_list_v1(ctx, project_id),
        P::SuiteGetRequest {
            project_id,
            suite_id,
        } => suite_get_v1(ctx, project_id, suite_id),
        P::SuiteSaveRequest {
            project_id,
            suite_id,
            name,
            description,
            case_ids,
        } => suite_save_v1(
            ctx,
            project_id,
            suite_id.as_deref(),
            name,
            description,
            case_ids,
        ),
        P::SuiteDeleteRequest {
            project_id,
            suite_id,
        } => suite_delete_v1(ctx, project_id, suite_id),
        P::RunsListRequest {
            project_id,
            status,
            run_type,
            offset,
            limit,
        } => runs_list_v1(ctx, project_id, status, run_type, *offset, *limit),
        P::RunCreateRequest {
            project_id,
            name,
            suite_id,
            case_ids,
            from_failed_run_id,
            env_note,
            assignment_mode,
            single_assignee,
            assignments,
        } => run_create_v1(
            ctx,
            project_id,
            name,
            suite_id,
            case_ids,
            from_failed_run_id,
            env_note,
            assignment_mode,
            single_assignee,
            assignments,
        ),
        P::RunGetRequest { project_id, run_id } => run_get_v1(ctx, project_id, run_id),
        P::RunCloseRequest {
            project_id,
            run_id,
            cancelled,
        } => run_close_v1(ctx, project_id, run_id, *cancelled),
        P::RunDeleteRequest { project_id, run_id } => run_delete_v1(ctx, project_id, run_id),
        P::RunItemClaimRequest {
            project_id,
            run_id,
            item_id,
        } => run_item_claim_v1(ctx, project_id, run_id, item_id.as_deref()),
        P::RunItemReleaseRequest {
            project_id,
            item_id,
        } => run_item_release_v1(ctx, project_id, item_id),
        P::RunItemGetRequest {
            project_id,
            item_id,
        } => run_item_get_v1(ctx, project_id, item_id),
        P::RunStepSetRequest {
            project_id,
            item_id,
            step_index,
            status,
            note,
            attachments_json,
        } => run_step_set_v1(
            ctx,
            project_id,
            item_id,
            *step_index,
            status,
            note,
            attachments_json,
        ),
        P::RunItemFinishRequest {
            project_id,
            item_id,
            status,
            result_note,
            tester_config,
            duration_secs,
            attachments_json,
        } => run_item_finish_v1(
            ctx,
            project_id,
            item_id,
            status,
            result_note,
            tester_config,
            *duration_secs,
            attachments_json,
        ),
        P::MyTestWorkRequest => my_test_work_v1(ctx),
        P::TasksListRequest {
            project_id,
            task_type,
            status,
            assigned_to,
            search,
            offset,
            limit,
            severity,
            include_archived,
        } => tasks_list_v1(
            ctx,
            project_id,
            task_type,
            status,
            assigned_to,
            search,
            *offset,
            *limit,
            severity,
            *include_archived,
        ),
        P::TaskGetRequest {
            project_id,
            task_id,
        } => task_get_v1(ctx, project_id, task_id),
        P::TaskSaveRequest {
            project_id,
            task_id,
            task_type,
            title,
            description_md,
            severity,
            priority,
            status,
            assigned_to,
            due_date,
            parent_task_id,
            links_json,
            attachments_json,
        } => task_save_v1(
            ctx,
            project_id,
            task_id.as_deref(),
            task_type,
            title,
            description_md,
            severity,
            priority,
            status,
            assigned_to,
            due_date,
            parent_task_id.as_deref(),
            links_json,
            attachments_json,
        ),
        P::TaskHandoverRequest {
            project_id,
            task_id,
            assigned_to,
            note_md,
            mention_user_ids,
        } => task_handover(
            ctx,
            project_id,
            task_id,
            assigned_to,
            note_md,
            mention_user_ids,
        ),
        P::TaskArchiveRequest {
            project_id,
            task_id,
            archived,
        } => task_archive(ctx, project_id, task_id, *archived),
        P::TaskTypesListRequest { project_id } => task_types_list(ctx, project_id),
        P::TaskTypeSaveRequest {
            project_id,
            type_id,
            name,
            description,
            sort_order,
            active,
        } => task_type_save(
            ctx,
            project_id,
            type_id,
            name,
            description,
            *sort_order,
            *active,
        ),
        P::TaskEventsRequest {
            project_id,
            task_id,
            before_id,
            limit,
        } => task_events(ctx, project_id, task_id, *before_id, *limit),
        P::TaskLinksListRequest {
            project_id,
            task_id,
        } => task_links_list(ctx, project_id, task_id),
        P::TaskLinkSaveRequest {
            project_id,
            source_task_id,
            target_task_id,
            kind,
            lag_days,
        } => task_link_save(
            ctx,
            project_id,
            source_task_id,
            target_task_id,
            kind,
            *lag_days,
        ),
        P::TaskLinkDeleteRequest {
            project_id,
            link_id,
        } => task_link_delete(ctx, project_id, *link_id),
        P::TaskDeleteRequest {
            project_id,
            task_id,
        } => task_delete_v1(ctx, project_id, task_id),
        P::TaskCommentAddRequest {
            project_id,
            task_id,
            body_md,
            mention_user_ids,
        } => task_comment_add_v1(ctx, project_id, task_id, body_md, mention_user_ids),
        P::TaskCommentEditRequest {
            project_id,
            comment_id,
            body_md,
            mention_user_ids,
        } => task_comment_edit_v1(ctx, project_id, comment_id, body_md, mention_user_ids),
        P::TaskCommentDeleteRequest {
            project_id,
            comment_id,
        } => task_comment_delete_v1(ctx, project_id, comment_id),
        P::GenerationStartRequest {
            project_id,
            kind,
            source_ids,
            requested_count,
            instructions,
            agent_id,
        } => {
            generation_start_v1(
                ctx,
                project_id,
                kind,
                source_ids,
                *requested_count,
                instructions,
                agent_id.as_deref(),
            )
            .await
        }
        P::GenerationsListRequest { project_id } => generations_list_v1(ctx, project_id),
        P::GenerationGetRequest { project_id, gen_id } => {
            generation_get_v1(ctx, project_id, gen_id)
        }
        P::GenerationCancelRequest { project_id, gen_id } => {
            generation_cancel_v1(ctx, project_id, gen_id)
        }
        P::GenerationReviewRequest {
            project_id,
            gen_id,
            accept_case_ids,
            reject_case_ids,
        } => generation_review_v1(ctx, project_id, gen_id, accept_case_ids, reject_case_ids),
        P::GenerationDeleteRequest { project_id, gen_id } => {
            generation_delete_v1(ctx, project_id, gen_id)
        }
        P::NotificationsListRequest {
            only_unread,
            before_id,
            limit,
        } => notifications_list_v1(ctx, *only_unread, before_id.as_deref(), *limit),
        P::NotificationsMarkReadRequest { notification_ids } => {
            notifications_mark_read_v1(ctx, notification_ids)
        }
        P::ReportQueryRequest {
            project_id,
            report,
            from_date,
            to_date,
            suite_id,
            // Consumed by the F4 report kinds (perf_compare, tester_activity);
            // bound explicitly so a further field addition still fails to compile.
            run_ids,
        } => report_query_v1(
            ctx, project_id, report, from_date, to_date, suite_id, run_ids,
        ),
        // ---- F3: environments, build profiles, automated runs, code sources ----
        P::EnvironmentsListRequest { project_id } => environments_list_v1(ctx, project_id),
        P::EnvironmentSaveRequest {
            project_id,
            environment_id,
            name,
            env_type,
            base_url,
            auth_type,
            secret,
            extra_headers_json,
            host_allowlist,
            justification,
        } => {
            environment_save_v1(
                ctx,
                project_id,
                environment_id.as_deref(),
                name,
                env_type,
                base_url,
                auth_type,
                secret.as_deref(),
                extra_headers_json,
                host_allowlist,
                justification,
            )
            .await
        }
        P::EnvironmentDeleteRequest {
            project_id,
            environment_id,
        } => environment_delete_v1(ctx, project_id, environment_id),
        P::EnvApprovalsListRequest => env_approvals_list_v1(ctx),
        P::EnvApprovalDecideRequest {
            project_id,
            environment_id,
            approve,
            reason,
        } => env_approval_decide_v1(ctx, project_id, environment_id, *approve, reason),
        P::BuildProfileGetRequest {
            project_id,
            source_id,
        } => build_profile_get_v1(ctx, project_id, source_id),
        P::BuildProfileSaveRequest {
            project_id,
            source_id,
            toolchain,
            base_image,
            install_cmd,
            test_cmd,
            workdir,
        } => build_profile_save_v1(
            ctx,
            project_id,
            source_id,
            toolchain,
            base_image,
            install_cmd,
            test_cmd,
            workdir,
        ),
        P::RunnersListRequest { project_id } => runners_list_v1(ctx, project_id).await,
        P::RunStartAutoRequest {
            project_id,
            name,
            suite_id,
            case_ids,
            from_run_id,
            environment_id,
            runner_service_id,
            perf_profile_json,
        } => {
            run_start_auto_v1(
                ctx,
                project_id,
                name,
                suite_id,
                case_ids,
                from_run_id,
                environment_id,
                runner_service_id,
                perf_profile_json,
            )
            .await
        }
        P::RunAutoGetRequest { project_id, run_id } => run_auto_get_v1(ctx, project_id, run_id),
        P::RunAutoCancelRequest { project_id, run_id } => {
            run_auto_cancel_v1(ctx, project_id, run_id)
        }
        P::TryRunCancelRequest { project_id, try_id } => try_run_cancel_v1(ctx, project_id, try_id),
        P::SourceRefreshRequest {
            project_id,
            source_id,
        } => source_refresh_v1(ctx, project_id, source_id).await,
        P::ApiSpecEndpointsRequest {
            project_id,
            source_id,
        } => api_spec_endpoints_v1(ctx, project_id, source_id),
        P::SourceSecretSetRequest {
            project_id,
            source_id,
            token,
        } => source_secret_set_v1(ctx, project_id, source_id, token.as_deref()),
        P::RunArtifactGetRequest {
            project_id,
            artifact_id,
            max_bytes,
        } => run_artifact_get_v1(ctx, project_id, artifact_id, *max_bytes),
        // ---- F4: schedules, ML Studio links, kanban, export/import ----
        P::SchedulesListRequest { project_id } => schedules_list_v1(ctx, project_id),
        P::ScheduleSaveRequest {
            project_id,
            schedule_id,
            name,
            run_type,
            suite_id,
            case_ids,
            environment_id,
            runner_service_id,
            perf_profile_json,
            assignment_mode,
            assignees,
            schedule_kind,
            schedule_expr,
            timezone,
            enabled,
        } => schedule_save_v1(
            ctx,
            project_id,
            schedule_id.as_deref(),
            &ScheduleWire {
                name,
                run_type,
                suite_id,
                case_ids,
                environment_id,
                runner_service_id,
                perf_profile_json,
                assignment_mode,
                assignees,
                schedule_kind,
                schedule_expr,
                timezone,
                enabled: *enabled,
            },
        ),
        P::ScheduleDeleteRequest {
            project_id,
            schedule_id,
        } => schedule_delete_v1(ctx, project_id, schedule_id),
        P::ScheduleSetEnabledRequest {
            project_id,
            schedule_id,
            enabled,
        } => schedule_set_enabled_v1(ctx, project_id, schedule_id, *enabled),
        P::ScheduleRunNowRequest {
            project_id,
            schedule_id,
        } => schedule_run_now_v1(ctx, project_id, schedule_id).await,
        P::ScheduleRunsListRequest {
            project_id,
            schedule_id,
            limit,
        } => schedule_runs_list_v1(ctx, project_id, schedule_id, *limit),
        P::MlLinksListRequest { project_id } => ml_links_list_v1(ctx, project_id),
        P::MlProjectCreateFromProjectRequest {
            project_id,
            ml_name,
            project_type,
            role_map,
            sync_permissions,
            label,
        } => ml_project_create_from_project_v1(
            ctx,
            project_id,
            ml_name,
            project_type,
            role_map,
            *sync_permissions,
            label,
        ),
        P::MlProjectCandidatesRequest { project_id } => ml_project_candidates_v1(ctx, project_id),
        P::MlLinkAttachRequest {
            project_id,
            ml_project_id,
            label,
            sync_permissions,
            role_map,
        } => ml_link_attach_v1(
            ctx,
            project_id,
            ml_project_id,
            label,
            *sync_permissions,
            role_map,
        ),
        P::MlLinkUpdateRequest {
            project_id,
            link_id,
            label,
            sync_permissions,
            role_map,
        } => ml_link_update_v1(ctx, project_id, link_id, label, *sync_permissions, role_map),
        P::MlLinkDetachRequest {
            project_id,
            link_id,
            revoke_members,
        } => ml_link_detach_v1(ctx, project_id, link_id, *revoke_members),
        P::MlLinkSyncNowRequest {
            project_id,
            link_id,
        } => ml_link_sync_now_v1(ctx, project_id, link_id),
        P::TaskStatusSetRequest {
            project_id,
            task_id,
            status,
        } => task_status_set_v1(ctx, project_id, task_id, status),
        P::ProjectExportStartRequest {
            project_id,
            include_runs,
            include_vectors,
            include_user_names,
        } => project_export_start_v1(
            ctx,
            project_id,
            *include_runs,
            *include_vectors,
            *include_user_names,
        ),
        P::ProjectExportStatusRequest { project_id, job_id } => {
            project_export_status_v1(ctx, project_id, job_id)
        }
        P::ProjectImportUploadChunkRequest {
            upload_id,
            filename,
            seq,
            total_chunks,
            bytes,
        } => project_import_upload_chunk_v1(ctx, upload_id, filename, *seq, *total_chunks, bytes),
        P::ProjectImportPreviewRequest { upload_id } => project_import_preview_v1(ctx, upload_id),
        P::ProjectImportApplyRequest {
            upload_id,
            name_override,
            import_vectors,
            import_runs,
        } => project_import_apply_v1(ctx, upload_id, name_override, *import_vectors, *import_runs),
        P::ProjectImportStatusRequest { job_id } => project_import_status_v1(ctx, job_id),
        // F3/F4 stream-initiating requests are served by dedicated stream handlers.
        P::TryRunStartRequest { .. }
        | P::RunAutoStreamRequest { .. }
        | P::ArchiveStreamRequest { .. }
        | P::CodeAssistRequest { .. } => Err(ProtocolError::bad_request(
            "use streaming subscribe for this variant",
        )),
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
        | P::ChatStreamEnd { .. }
        | P::CasesListResponse { .. }
        | P::CaseGetResponse { .. }
        | P::CaseSaveResponse { .. }
        | P::CaseStatusSetResult { .. }
        | P::CasesBulkStatusResponse { .. }
        | P::CaseDuplicateResponse { .. }
        | P::CaseDeleteResult { .. }
        | P::CaseVersionGetResponse { .. }
        | P::CaseRestoreVersionResponse { .. }
        | P::CasesImportCsvResponse { .. }
        | P::AttachmentGetResponse { .. }
        | P::SuitesListResponse { .. }
        | P::SuiteGetResponse { .. }
        | P::SuiteSaveResponse { .. }
        | P::SuiteDeleteResult { .. }
        | P::RunsListResponse { .. }
        | P::RunCreateResponse { .. }
        | P::RunGetResponse { .. }
        | P::RunCloseResult { .. }
        | P::RunDeleteResult { .. }
        | P::RunItemClaimResponse { .. }
        | P::RunItemReleaseResult { .. }
        | P::RunItemGetResponse { .. }
        | P::RunStepSetResult { .. }
        | P::RunItemFinishResponse { .. }
        | P::MyTestWorkResponse { .. }
        | P::TasksListResponse { .. }
        | P::TaskGetResponse { .. }
        | P::TaskSaveResponse { .. }
        | P::TaskDeleteResult { .. }
        | P::TaskArchiveResult { .. }
        | P::TaskHandoverResult { .. }
        | P::TaskTypesListResponse { .. }
        | P::TaskTypeSaveResponse { .. }
        | P::TaskEventsResponse { .. }
        | P::TaskLinksListResponse { .. }
        | P::TaskLinkSaveResult { .. }
        | P::TaskLinkDeleteResult { .. }
        | P::AttachmentUploadStatusResponse { .. }
        | P::AttachmentUploadChunkResponse { .. }
        | P::AttachmentUploadCancelResult { .. }
        | P::AttachmentPreviewResponse { .. }
        | P::AttachmentUsageResponse { .. }
        | P::TaskCommentAddResponse { .. }
        | P::TaskCommentEditResult { .. }
        | P::TaskCommentDeleteResult { .. }
        | P::GenerationStartResponse { .. }
        | P::GenerationsListResponse { .. }
        | P::GenerationGetResponse { .. }
        | P::GenerationCancelResult { .. }
        | P::GenerationReviewResponse { .. }
        | P::GenerationDeleteResult { .. }
        | P::NotificationsListResponse { .. }
        | P::NotificationsMarkReadResult { .. }
        | P::ReportQueryResponse { .. }
        | P::EnvironmentsListResponse { .. }
        | P::EnvironmentSaveResponse { .. }
        | P::EnvironmentDeleteResult { .. }
        | P::EnvApprovalsListResponse { .. }
        | P::EnvApprovalDecideResult { .. }
        | P::BuildProfileGetResponse { .. }
        | P::BuildProfileSaveResponse { .. }
        | P::RunnersListResponse { .. }
        | P::RunStartAutoResponse { .. }
        | P::RunAutoGetResponse { .. }
        | P::RunAutoCancelResult { .. }
        | P::TryRunCancelResult { .. }
        | P::SourceRefreshResponse { .. }
        | P::ApiSpecEndpointsResponse { .. }
        | P::SourceSecretSetResult { .. }
        | P::RunArtifactGetResponse { .. }
        | P::RunAutoStreamChunk { .. }
        | P::RunAutoStreamEnd { .. }
        | P::TryRunStreamChunk { .. }
        | P::TryRunStreamEnd { .. }
        | P::CodeAssistStreamChunk { .. }
        | P::CodeAssistStreamEnd { .. }
        | P::SchedulesListResponse { .. }
        | P::ScheduleSaveResponse { .. }
        | P::ScheduleDeleteResult { .. }
        | P::ScheduleSetEnabledResult { .. }
        | P::ScheduleRunNowResponse { .. }
        | P::ScheduleRunsListResponse { .. }
        | P::MlLinksListResponse { .. }
        | P::MlProjectCreateFromProjectResponse { .. }
        | P::MlProjectCandidatesResponse { .. }
        | P::MlLinkAttachResponse { .. }
        | P::MlLinkUpdateResult { .. }
        | P::MlLinkDetachResult { .. }
        | P::MlLinkSyncNowResponse { .. }
        | P::TaskStatusSetResult { .. }
        | P::ProjectExportStartResponse { .. }
        | P::ProjectExportStatusResponse { .. }
        | P::ProjectImportUploadChunkResponse { .. }
        | P::ProjectImportPreviewResponse { .. }
        | P::ProjectImportApplyResponse { .. }
        | P::ProjectImportStatusResponse { .. }
        | P::ArchiveStreamChunk { .. }
        | P::ArchiveStreamEnd { .. } => Err(ProtocolError::bad_request(
            "variant is not a valid project studio request",
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
register_project_studio_variant!(
    "ProjectStudioCasesListRequest",
    "tentaflow_ws_handler_ps_cases_list"
);
register_project_studio_variant!(
    "ProjectStudioCaseGetRequest",
    "tentaflow_ws_handler_ps_case_get"
);
register_project_studio_variant!(
    "ProjectStudioCaseSaveRequest",
    "tentaflow_ws_handler_ps_case_save"
);
register_project_studio_variant!(
    "ProjectStudioCaseStatusSetRequest",
    "tentaflow_ws_handler_ps_case_status_set"
);
register_project_studio_variant!(
    "ProjectStudioCasesBulkStatusRequest",
    "tentaflow_ws_handler_ps_cases_bulk_status"
);
register_project_studio_variant!(
    "ProjectStudioCaseDuplicateRequest",
    "tentaflow_ws_handler_ps_case_duplicate"
);
register_project_studio_variant!(
    "ProjectStudioCaseDeleteRequest",
    "tentaflow_ws_handler_ps_case_delete"
);
register_project_studio_variant!(
    "ProjectStudioCaseVersionGetRequest",
    "tentaflow_ws_handler_ps_case_version_get"
);
register_project_studio_variant!(
    "ProjectStudioCaseRestoreVersionRequest",
    "tentaflow_ws_handler_ps_case_restore_version"
);
register_project_studio_variant!(
    "ProjectStudioCasesImportCsvRequest",
    "tentaflow_ws_handler_ps_cases_import_csv"
);
register_project_studio_variant!(
    "ProjectStudioAttachmentGetRequest",
    "tentaflow_ws_handler_ps_attachment_get"
);
register_project_studio_variant!(
    "ProjectStudioSuitesListRequest",
    "tentaflow_ws_handler_ps_suites_list"
);
register_project_studio_variant!(
    "ProjectStudioSuiteGetRequest",
    "tentaflow_ws_handler_ps_suite_get"
);
register_project_studio_variant!(
    "ProjectStudioSuiteSaveRequest",
    "tentaflow_ws_handler_ps_suite_save"
);
register_project_studio_variant!(
    "ProjectStudioSuiteDeleteRequest",
    "tentaflow_ws_handler_ps_suite_delete"
);
register_project_studio_variant!(
    "ProjectStudioRunsListRequest",
    "tentaflow_ws_handler_ps_runs_list"
);
register_project_studio_variant!(
    "ProjectStudioRunCreateRequest",
    "tentaflow_ws_handler_ps_run_create"
);
register_project_studio_variant!(
    "ProjectStudioRunGetRequest",
    "tentaflow_ws_handler_ps_run_get"
);
register_project_studio_variant!(
    "ProjectStudioRunCloseRequest",
    "tentaflow_ws_handler_ps_run_close"
);
register_project_studio_variant!(
    "ProjectStudioRunDeleteRequest",
    "tentaflow_ws_handler_ps_run_delete"
);
register_project_studio_variant!(
    "ProjectStudioRunItemClaimRequest",
    "tentaflow_ws_handler_ps_run_item_claim"
);
register_project_studio_variant!(
    "ProjectStudioRunItemReleaseRequest",
    "tentaflow_ws_handler_ps_run_item_release"
);
register_project_studio_variant!(
    "ProjectStudioRunItemGetRequest",
    "tentaflow_ws_handler_ps_run_item_get"
);
register_project_studio_variant!(
    "ProjectStudioRunStepSetRequest",
    "tentaflow_ws_handler_ps_run_step_set"
);
register_project_studio_variant!(
    "ProjectStudioRunItemFinishRequest",
    "tentaflow_ws_handler_ps_run_item_finish"
);
register_project_studio_variant!(
    "ProjectStudioMyTestWorkRequest",
    "tentaflow_ws_handler_ps_my_test_work"
);
register_project_studio_variant!(
    "ProjectStudioTasksListRequest",
    "tentaflow_ws_handler_ps_tasks_list"
);
register_project_studio_variant!(
    "ProjectStudioTaskGetRequest",
    "tentaflow_ws_handler_ps_task_get"
);
register_project_studio_variant!(
    "ProjectStudioTaskSaveRequest",
    "tentaflow_ws_handler_ps_task_save"
);
register_project_studio_variant!(
    "ProjectStudioTaskDeleteRequest",
    "tentaflow_ws_handler_ps_task_delete"
);
register_project_studio_variant!(
    "ProjectStudioTaskCommentAddRequest",
    "tentaflow_ws_handler_ps_task_comment_add"
);
register_project_studio_variant!(
    "ProjectStudioTaskCommentEditRequest",
    "tentaflow_ws_handler_ps_task_comment_edit"
);
register_project_studio_variant!(
    "ProjectStudioTaskCommentDeleteRequest",
    "tentaflow_ws_handler_ps_task_comment_delete"
);
register_project_studio_variant!(
    "ProjectStudioGenerationStartRequest",
    "tentaflow_ws_handler_ps_generation_start"
);
register_project_studio_variant!(
    "ProjectStudioGenerationsListRequest",
    "tentaflow_ws_handler_ps_generations_list"
);
register_project_studio_variant!(
    "ProjectStudioGenerationGetRequest",
    "tentaflow_ws_handler_ps_generation_get"
);
register_project_studio_variant!(
    "ProjectStudioGenerationCancelRequest",
    "tentaflow_ws_handler_ps_generation_cancel"
);
register_project_studio_variant!(
    "ProjectStudioGenerationReviewRequest",
    "tentaflow_ws_handler_ps_generation_review"
);
register_project_studio_variant!(
    "ProjectStudioGenerationDeleteRequest",
    "tentaflow_ws_handler_ps_generation_delete"
);
register_project_studio_variant!(
    "ProjectStudioNotificationsListRequest",
    "tentaflow_ws_handler_ps_notifications_list"
);
register_project_studio_variant!(
    "ProjectStudioNotificationsMarkReadRequest",
    "tentaflow_ws_handler_ps_notifications_mark_read"
);
register_project_studio_variant!(
    "ProjectStudioReportQueryRequest",
    "tentaflow_ws_handler_ps_report_query"
);
register_project_studio_variant!(
    "ProjectStudioEnvironmentsListRequest",
    "tentaflow_ws_handler_ps_environments_list"
);
register_project_studio_variant!(
    "ProjectStudioEnvironmentSaveRequest",
    "tentaflow_ws_handler_ps_environment_save"
);
register_project_studio_variant!(
    "ProjectStudioEnvironmentDeleteRequest",
    "tentaflow_ws_handler_ps_environment_delete"
);
register_project_studio_variant!(
    "ProjectStudioEnvApprovalsListRequest",
    "tentaflow_ws_handler_ps_env_approvals_list"
);
register_project_studio_variant!(
    "ProjectStudioEnvApprovalDecideRequest",
    "tentaflow_ws_handler_ps_env_approval_decide"
);
register_project_studio_variant!(
    "ProjectStudioBuildProfileGetRequest",
    "tentaflow_ws_handler_ps_build_profile_get"
);
register_project_studio_variant!(
    "ProjectStudioBuildProfileSaveRequest",
    "tentaflow_ws_handler_ps_build_profile_save"
);
register_project_studio_variant!(
    "ProjectStudioRunnersListRequest",
    "tentaflow_ws_handler_ps_runners_list"
);
register_project_studio_variant!(
    "ProjectStudioRunStartAutoRequest",
    "tentaflow_ws_handler_ps_run_start_auto"
);
register_project_studio_variant!(
    "ProjectStudioRunAutoGetRequest",
    "tentaflow_ws_handler_ps_run_auto_get"
);
register_project_studio_variant!(
    "ProjectStudioRunAutoCancelRequest",
    "tentaflow_ws_handler_ps_run_auto_cancel"
);
register_project_studio_variant!(
    "ProjectStudioTryRunCancelRequest",
    "tentaflow_ws_handler_ps_try_run_cancel"
);
register_project_studio_variant!(
    "ProjectStudioSourceRefreshRequest",
    "tentaflow_ws_handler_ps_source_refresh"
);
register_project_studio_variant!(
    "ProjectStudioApiSpecEndpointsRequest",
    "tentaflow_ws_handler_ps_api_spec_endpoints"
);
register_project_studio_variant!(
    "ProjectStudioSourceSecretSetRequest",
    "tentaflow_ws_handler_ps_source_secret_set"
);
register_project_studio_variant!(
    "ProjectStudioRunArtifactGetRequest",
    "tentaflow_ws_handler_ps_run_artifact_get"
);
register_project_studio_variant!(
    "ProjectStudioSchedulesListRequest",
    "tentaflow_ws_handler_ps_schedules_list"
);
register_project_studio_variant!(
    "ProjectStudioScheduleSaveRequest",
    "tentaflow_ws_handler_ps_schedule_save"
);
register_project_studio_variant!(
    "ProjectStudioScheduleDeleteRequest",
    "tentaflow_ws_handler_ps_schedule_delete"
);
register_project_studio_variant!(
    "ProjectStudioScheduleSetEnabledRequest",
    "tentaflow_ws_handler_ps_schedule_set_enabled"
);
register_project_studio_variant!(
    "ProjectStudioScheduleRunNowRequest",
    "tentaflow_ws_handler_ps_schedule_run_now"
);
register_project_studio_variant!(
    "ProjectStudioScheduleRunsListRequest",
    "tentaflow_ws_handler_ps_schedule_runs_list"
);
register_project_studio_variant!(
    "ProjectStudioMlLinksListRequest",
    "tentaflow_ws_handler_ps_ml_links_list"
);
register_project_studio_variant!(
    "ProjectStudioMlProjectCreateFromProjectRequest",
    "tentaflow_ws_handler_ps_ml_project_create_from_project"
);
register_project_studio_variant!(
    "ProjectStudioMlProjectCandidatesRequest",
    "tentaflow_ws_handler_ps_ml_project_candidates"
);
register_project_studio_variant!(
    "ProjectStudioMlLinkAttachRequest",
    "tentaflow_ws_handler_ps_ml_link_attach"
);
register_project_studio_variant!(
    "ProjectStudioMlLinkUpdateRequest",
    "tentaflow_ws_handler_ps_ml_link_update"
);
register_project_studio_variant!(
    "ProjectStudioMlLinkDetachRequest",
    "tentaflow_ws_handler_ps_ml_link_detach"
);
register_project_studio_variant!(
    "ProjectStudioMlLinkSyncNowRequest",
    "tentaflow_ws_handler_ps_ml_link_sync_now"
);
register_project_studio_variant!(
    "ProjectStudioTaskStatusSetRequest",
    "tentaflow_ws_handler_ps_task_status_set"
);
register_project_studio_variant!(
    "ProjectStudioProjectExportStartRequest",
    "tentaflow_ws_handler_ps_project_export_start"
);
register_project_studio_variant!(
    "ProjectStudioProjectExportStatusRequest",
    "tentaflow_ws_handler_ps_project_export_status"
);
register_project_studio_variant!(
    "ProjectStudioProjectImportUploadChunkRequest",
    "tentaflow_ws_handler_ps_project_import_upload_chunk"
);
register_project_studio_variant!(
    "ProjectStudioProjectImportPreviewRequest",
    "tentaflow_ws_handler_ps_project_import_preview"
);
register_project_studio_variant!(
    "ProjectStudioProjectImportApplyRequest",
    "tentaflow_ws_handler_ps_project_import_apply"
);
register_project_studio_variant!(
    "ProjectStudioProjectImportStatusRequest",
    "tentaflow_ws_handler_ps_project_import_status"
);

register_project_studio_variant!(
    "ProjectStudioCatalogueGetRequest",
    "tentaflow_ws_handler_ps_catalogue_get"
);
register_project_studio_variant!(
    "ProjectStudioFunctionSaveRequest",
    "tentaflow_ws_handler_ps_function_save"
);
register_project_studio_variant!(
    "ProjectStudioFunctionDeleteRequest",
    "tentaflow_ws_handler_ps_function_delete"
);
register_project_studio_variant!(
    "ProjectStudioMemberAccessSetRequest",
    "tentaflow_ws_handler_ps_member_access_set"
);

register_project_studio_variant!(
    "ProjectStudioTaskArchiveRequest",
    "tentaflow_ws_handler_ps_taskarchive"
);
register_project_studio_variant!(
    "ProjectStudioTaskTypesListRequest",
    "tentaflow_ws_handler_ps_tasktypeslist"
);
register_project_studio_variant!(
    "ProjectStudioTaskTypeSaveRequest",
    "tentaflow_ws_handler_ps_tasktypesave"
);
register_project_studio_variant!(
    "ProjectStudioTaskEventsRequest",
    "tentaflow_ws_handler_ps_taskevents"
);
register_project_studio_variant!(
    "ProjectStudioTaskLinksListRequest",
    "tentaflow_ws_handler_ps_tasklinkslist"
);
register_project_studio_variant!(
    "ProjectStudioTaskLinkSaveRequest",
    "tentaflow_ws_handler_ps_tasklinksave"
);
register_project_studio_variant!(
    "ProjectStudioTaskLinkDeleteRequest",
    "tentaflow_ws_handler_ps_tasklinkdelete"
);
register_project_studio_variant!(
    "ProjectStudioAttachmentUploadStatusRequest",
    "tentaflow_ws_handler_ps_attachmentuploadstatus"
);
register_project_studio_variant!(
    "ProjectStudioAttachmentUploadChunkRequest",
    "tentaflow_ws_handler_ps_attachmentuploadchunk"
);
register_project_studio_variant!(
    "ProjectStudioAttachmentUploadCancelRequest",
    "tentaflow_ws_handler_ps_attachmentuploadcancel"
);
register_project_studio_variant!(
    "ProjectStudioAttachmentPreviewRequest",
    "tentaflow_ws_handler_ps_attachmentpreview"
);

register_project_studio_variant!(
    "ProjectStudioAttachmentUsageRequest",
    "tentaflow_ws_handler_ps_attachment_usage"
);

register_project_studio_variant!(
    "ProjectStudioTaskHandoverRequest",
    "tentaflow_ws_handler_ps_task_handover"
);

// =============================================================================
// Registry: list / create / get / update / archive / delete
// =============================================================================

fn projects_list_v1(
    ctx: &HandlerContext,
    include_archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let admin = is_admin(ctx);
    let records = repository::list_projects(&org.org_id, include_archived)
        .map_err(|e| db_error("projects_list", e))?;
    let memberships = repository::member_accesses_for_user(&org.user_id)
        .map_err(|e| db_error("member_accesses", e))?;

    let visible: Vec<&ProjectRecord> = records
        .iter()
        .filter(|r| admin || memberships.contains_key(&r.project_id))
        .collect();
    let owner_ids: Vec<String> = visible.iter().map(|r| r.owner_user_id.clone()).collect();
    let names = repository::resolve_user_refs(&owner_ids);

    let mut projects = Vec::with_capacity(visible.len());
    for record in visible {
        let access = repository::project_access(record, &org.user_id, admin)
            .map_err(|e| db_error("project_access", e))?;
        projects.push(project_info(record, access, &names)?);
    }

    let can_create = admin
        || repository::has_creator_grant(&org.user_id, &org.org_id)
            .map_err(|e| db_error("creator_grant", e))?;
    Ok(ps(ProjectStudioPayload::ProjectsListResponse {
        projects,
        can_create,
        can_administer: admin,
    }))
}

fn project_create_v1(
    ctx: &HandlerContext,
    name: &str,
    description: &str,
    template: &str,
    modules: &[String],
    key_prefix: &str,
    members: &[MemberInputWire],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let can_create = is_admin(ctx)
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
    let catalogue = crate::project_studio::models::default_project_functions();
    let initial = member_inputs(org, members, &catalogue)?;

    let project_id = uuid::Uuid::new_v4().to_string();
    let dir = crate::project_studio::project_dir(&project_id);
    std::fs::create_dir_all(dir.join("files"))
        .map_err(|e| ProtocolError::internal(format!("project dir create: {e}")))?;
    if let Err(e) = project_db::open_pool_at(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(db_error("project_db.create", e));
    }

    let modules_json = serde_json::to_string(&normalize_modules(modules)?)
        .map_err(|e| ProtocolError::internal(format!("modules serialize: {e}")))?;
    if let Err(e) = repository::create_project(
        &project_id,
        &org.org_id,
        name,
        description.trim(),
        template,
        &modules_json,
        &org.user_id,
        &dir.to_string_lossy(),
        key_prefix,
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
    let (record, my_access) = require_project_access(ctx, org, project_id)?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.owner_user_id));
    let project = project_info(&record, my_access, &names)?;
    Ok(ps(ProjectStudioPayload::ProjectGetResponse { project }))
}

fn project_update_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: &str,
    description: &str,
    key_prefix: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("project name is required"));
    }
    let ok = repository::update_project_name_desc(
        &org.org_id,
        project_id,
        name,
        description.trim(),
        key_prefix,
    )
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
    let (_record, access) = require_project_access(ctx, org, project_id)?;
    require_owner(&access)?;
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
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_owner(&access)?;

    let content_pool = if std::path::Path::new(&record.dir_path)
        .join("project.db")
        .is_file()
    {
        Some(project_db::open(project_id).map_err(|e| db_error("project_storage", e))?)
    } else {
        let central =
            crate::project_studio::db::pool().map_err(|e| db_error("project_registry", e))?;
        let conn = central
            .read()
            .map_err(|e| db_error("project_registry", e))?;
        let mirrored: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_ml_grants WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .map_err(|e| db_error("project_ml_grants", e))?;
        if mirrored > 0 {
            return Err(ProtocolError::internal(
                "project storage missing; mirrored ML grants cannot be revoked",
            ));
        }
        None
    };
    if let Some(pool) = content_pool.as_ref() {
        let ids = repository::running_jobs(pool)
            .map_err(|e| db_error("running_jobs", e))?
            .into_iter()
            .map(|(job_id, _)| job_id)
            .collect::<Vec<_>>();
        for job_id in &ids {
            ingest::signal_cancel(job_id);
        }
        wait_for_jobs_terminal(pool, &ids).await?;
    }
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_owner(&access)?;
    if let Some(pool) = content_pool.as_ref() {
        for link in ml_link::list(pool).map_err(|e| db_error("ml_links", e))? {
            ml_link::detach(pool, project_id, &link, true).map_err(|e| db_error("ml_revoke", e))?;
        }
    }
    drop(content_pool);
    // 2. Drop the cached pool (checkpoint + release the SQLite handle).
    project_db::close(project_id);
    // 3. Drop every vector namespace of the `ps-<id>` scope (registry rows in
    //    tentaflow.db + on-disk data inside the project dir).
    ingest::drop_project_namespaces(&ctx.state.db, &org.org_id, project_id)
        .map_err(|e| db_error("drop_namespaces", e))?;
    // 4. Same for the knowledge graph: the registry rows live in tentaflow.db,
    //    so removing the project directory alone would leave them dangling.
    ingest::drop_project_graph(&ctx.state.db, &org.org_id, project_id)
        .map_err(|e| db_error("drop_graph", e))?;
    // 5. Remove the project directory (project.db, files/, vectors/, graph/).
    let dir = std::path::PathBuf::from(&record.dir_path);
    let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir))
        .await
        .map_err(|_| ProtocolError::internal("project dir removal task panicked"))?;
    if let Err(e) = removed {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(ProtocolError::internal(format!("project dir removal: {e}")));
        }
    }
    // 6. Central registry rows last — a crash above leaves the project
    //    visible (and the delete retryable) instead of orphaning data.
    repository::delete_project_rows(project_id).map_err(|e| db_error("project_delete", e))?;
    // The schedule loop reads the hint table, not `projects` — a leftover row
    // would keep pointing at a project that no longer exists.
    schedules::delete_hint(project_id);

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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    let rows = repository::list_members(project_id).map_err(|e| db_error("members_list", e))?;
    let mut ids: Vec<String> = rows.iter().map(|m| m.user_id.clone()).collect();
    ids.extend(rows.iter().map(|m| m.invited_by.clone()));
    let names = repository::resolve_user_refs(&ids);
    let members = rows
        .into_iter()
        .map(|m| -> Result<MemberInfo, ProtocolError> {
            Ok(MemberInfo {
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
                access: repository::project_access(&record, &m.user_id, false)
                    .map_err(|e| db_error("member_access", e))?,
                is_owner: record.owner_user_id == m.user_id,
                active: m.is_active(),
                functions: m.functions,
                project_admin: m.project_admin,
                expires_at: m.expires_at,
                user_id: m.user_id,
                role: String::new(),
                invited_by: m.invited_by,
                created_at: m.created_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
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
            let (_record, access) = require_project_access(ctx, org, project_id)?;
            require_project_admin(&access)?;
            repository::list_members(project_id)
                .map_err(|e| db_error("members_list", e))?
                .into_iter()
                .map(|m| m.user_id)
                .collect()
        }
        None => {
            // Creation wizard — creator grant (or admin); the creator becomes
            // the owner, so exclude them from the pick list.
            let can_create = is_admin(ctx)
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
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_project_admin(&access)?;
    require_active(&record)?;
    let catalogue =
        repository::list_functions(project_id).map_err(|e| db_error("function_catalogue", e))?;
    if members.is_empty() {
        return Err(ProtocolError::bad_request("no members to add"));
    }
    let to_add = member_inputs(org, members, &catalogue)?;
    let added = repository::add_members(project_id, &to_add, &org.user_id)
        .map_err(|e| db_error("members_add", e))?;
    if added > 0 {
        record_access_mutation(
            ctx,
            org,
            project_id,
            "member.added",
            "",
            &serde_json::json!({ "count": added }).to_string(),
        );
        spawn_ml_permission_sync(project_id);
    }
    Ok(ps(ProjectStudioPayload::MembersAddResponse { added }))
}

fn member_access_set(
    ctx: &HandlerContext,
    project_id: &str,
    user_id: &str,
    functions: &[String],
    project_admin: bool,
    expires_at: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_project_admin(&access)?;
    require_active(&record)?;
    let catalogue =
        repository::list_functions(project_id).map_err(|e| db_error("function_catalogue", e))?;
    let before = repository::member_access(project_id, user_id)
        .map_err(|e| db_error("member_access", e))?
        .ok_or_else(|| ProtocolError::not_found("member not found"))?;
    let input = crate::project_studio::models::validate_member_input(
        &MemberInput {
            user_id: user_id.to_string(),
            functions: functions.to_vec(),
            project_admin,
            expires_at: expires_at.map(str::to_string),
        },
        &catalogue,
        chrono::Utc::now(),
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let ok = repository::set_member_access(
        project_id,
        user_id,
        &input.functions,
        input.project_admin,
        input.expires_at.as_deref(),
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if ok {
        record_access_mutation(ctx, org, project_id, "member.access_changed", user_id,
            &serde_json::json!({
                "before": { "functions": before.functions, "project_admin": before.project_admin, "expires_at": before.expires_at },
                "after": { "functions": input.functions, "project_admin": input.project_admin, "expires_at": input.expires_at }
            }).to_string());
        spawn_ml_permission_sync(project_id);
    }
    Ok(ps(ProjectStudioPayload::Access(
        ProjectAccessPayload::MemberAccessSetResult { ok },
    )))
}

fn member_remove_v1(
    ctx: &HandlerContext,
    project_id: &str,
    user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_project_admin(&access)?;
    require_active(&record)?;
    repository::member_access(project_id, user_id)
        .map_err(|e| db_error("member_access", e))?
        .ok_or_else(|| ProtocolError::not_found("member not found"))?;
    use crate::services::org_structure::handover::{remove_project_member, MemberRemoval};
    let outcome = remove_project_member(&org.org_id, project_id, user_id, &org.user_id, "{}")
        .map_err(|e| ProtocolError::internal(e.to_string()))?;
    match outcome {
        MemberRemoval::Owner => Err(ProtocolError::bad_request(
            "transfer ownership before removing the owner",
        )),
        MemberRemoval::HoldsWork => Err(ProtocolError::bad_request(
            "member still holds work; use project removal handover",
        )),
        MemberRemoval::Missing => Ok(ps(ProjectStudioPayload::MemberRemoveResult { ok: false })),
        MemberRemoval::Removed => {
            activity::record_org_security(
                &ctx.state.db,
                &ctx.state.local_node_id,
                &org.user_id,
                "project_studio.member.removed",
                project_id,
                user_id,
            );
            Ok(ps(ProjectStudioPayload::MemberRemoveResult { ok: true }))
        }
    }
}

fn ownership_transfer_v1(
    ctx: &HandlerContext,
    project_id: &str,
    new_owner_user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Owner tier: the owner themselves, or an org admin taking over an
    // orphaned project.
    let (record, access) = require_project_access(ctx, org, project_id)?;
    require_owner(&access)?;
    require_active(&record)?;
    if new_owner_user_id == record.owner_user_id {
        return Err(ProtocolError::bad_request("user is already the owner"));
    }
    repository::member_access(project_id, new_owner_user_id)
        .map_err(|e| db_error("member_access", e))?
        .filter(|member| member.is_active())
        .ok_or_else(|| ProtocolError::bad_request("new owner must be an active project member"))?;
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
    spawn_ml_permission_sync(project_id);
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
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
    let record = require_upload_access(ctx, project_id)?;

    let org_id = org.org_id.clone();
    let user_id = org.user_id.clone();
    let project_id_owned = project_id.to_string();
    let dir = std::path::PathBuf::from(&record.dir_path);
    let upload_id_owned = upload_id.to_string();
    let filename = filename.to_string();
    let mime = mime.to_string();
    let bytes = bytes.to_vec();
    let context = ctx.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        ingest::accept_upload_chunk(&ingest::UploadChunk {
            org_id: &org_id,
            user_id: &user_id,
            project_id: &project_id_owned,
            dir_path: &dir,
            upload_id: &upload_id_owned,
            filename: &filename,
            mime: &mime,
            position: ingest::UploadPosition::Sequence { seq, total_chunks },
            bytes: &bytes,
            allowed: &|| {
                require_upload_access(&context, &project_id_owned)
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!(e.message))
            },
        })
    })
    .await
    .map_err(|_| ProtocolError::internal("upload task panicked"))?
    .map_err(|e| ProtocolError::bad_request(format!("upload rejected: {e}")))?;

    require_upload_access(ctx, project_id)?;
    let received_chunks = outcome.next_seq;
    let received_bytes = outcome.next_offset;
    let file_ref = outcome.complete.then_some(outcome.sha256);
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

async fn source_create_v1(
    ctx: &HandlerContext,
    project_id: &str,
    kind: &str,
    name: &str,
    config_json: &str,
    file_refs: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        source_write_area(kind),
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    match kind {
        "document" | "url" => {}
        // Code + spec sources own the whole create path (clone/extract/parse
        // before the row exists), so they branch out here.
        "git" | "zip" | "api_spec" => {
            return code_source_create(ctx, org, &record, kind, name, config_json, file_refs).await
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
                let meta = ingest::finalized_meta(
                    &org.org_id,
                    &org.user_id,
                    project_id,
                    std::path::Path::new(&record.dir_path),
                    sha,
                )
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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
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
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;

    // `config_json` is echoed back to every viewer by the source list, so a
    // credential must never land in it: for a git source the token is split off
    // into the encrypted column (same contract as create), for every other kind
    // the key is refused outright.
    let config_json = if source.kind == "git" {
        let (clean_config, token) = git_source::split_token(config_json)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
        // Address class is NOT decided here (it would block this handler on
        // DNS): `git_source::clone/refresh` refuse a private target at the
        // moment they would actually reach it.
        git_source::parse_config(&clean_config)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
        if !token.is_empty() {
            require_project(
                ctx,
                org,
                project_id,
                ProjectArea::Settings,
                ProjectPermissionLevel::Write,
            )?;
            let secret_enc = ctx
                .state
                .settings_cipher
                .encrypt(&token)
                .map_err(|e| db_error("source_secret_encrypt", e))?;
            repository::set_source_secret(&pool, source_id, &secret_enc)
                .map_err(|e| db_error("source_secret_set", e))?;
        }
        clean_config
    } else {
        if config.get("token").is_some() {
            return Err(ProtocolError::bad_request(
                "config_json must not carry a token for this source kind",
            ));
        }
        config_json.to_string()
    };
    let config_json = config_json.as_str();

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
                ingest::delete_file_graph(&ctx.state.db, &org.org_id, project_id, &f.file_id)
                    .map_err(|e| db_error("delete_graph", e))?;
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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;

    // Stop any running job of this source and wait for it to reach a
    // terminal status before ripping its data out — cancel is cooperative
    // and a still-running job would keep writing files/vectors mid-delete.
    let mut source_jobs: Vec<String> = Vec::new();
    if let Ok(jobs) = repository::running_jobs(&pool) {
        for (job_id, job_source_id) in jobs {
            if job_source_id == source_id {
                ingest::signal_cancel(&job_id);
                source_jobs.push(job_id);
            }
        }
    }
    wait_for_jobs_terminal(&pool, &source_jobs).await?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;

    let files = repository::files_for_ingest(&pool, source_id, None)
        .map_err(|e| db_error("files_for_ingest", e))?;
    // Cleanup-then-delete: derived stores first, rows second, unreferenced blobs
    // last.
    {
        let core_db = ctx.state.db.clone();
        let org_id = org.org_id.clone();
        let project_id_owned = project_id.to_string();
        let file_ids: Vec<String> = files.iter().map(|f| f.file_id.clone()).collect();
        tokio::task::spawn_blocking(move || {
            for file_id in &file_ids {
                ingest::delete_file_vectors(&core_db, &org_id, &project_id_owned, file_id)?;
                ingest::delete_file_graph(&core_db, &org_id, &project_id_owned, file_id)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| ProtocolError::internal("knowledge cleanup task panicked"))?
        .map_err(|e| db_error("delete_knowledge", e))?;
    }
    let ok = repository::delete_source_rows(&pool, source_id)
        .map_err(|e| db_error("source_delete", e))?;
    for f in &files {
        if f.sha256.is_empty() {
            continue;
        }
        let refs = repository::blob_ref_count(&pool, &f.sha256)
            .map_err(|e| db_error("blob_ref_count", e))?;
        if refs == 0 {
            let blob = std::path::Path::new(&record.dir_path)
                .join("files")
                .join(&f.sha256);
            let _ = std::fs::remove_file(blob);
        }
    }
    // Derived state of a code/spec source: the working tree in the cache, the
    // build recipe and the cached endpoint list.
    match source.kind.as_str() {
        "git" | "zip" => {
            git_source::remove_source_dir(project_id, source_id);
            let _ = build_profiles::delete_for_source(&pool, source_id);
        }
        "api_spec" => {
            let _ = repository::set_setting(&pool, &api_spec::endpoints_setting_key(source_id), "");
        }
        _ => {}
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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;
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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    // Job must belong to THIS project's database — the registry is
    // process-global, so an unchecked id would let one project cancel
    // another's job.
    let job = repository::get_ingest_job(&pool, job_id)
        .map_err(|e| db_error("get_job", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    let source = repository::get_source(&pool, &job.source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
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
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let file = repository::get_source_file(&pool, file_id)
        .map_err(|e| db_error("get_file", e))?
        .ok_or_else(|| ProtocolError::not_found("file not found"))?;
    let source = repository::get_source(&pool, &file.source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;

    {
        let core_db = ctx.state.db.clone();
        let org_id = org.org_id.clone();
        let project_id_owned = project_id.to_string();
        let file_id_owned = file_id.to_string();
        tokio::task::spawn_blocking(move || {
            ingest::delete_file_vectors(&core_db, &org_id, &project_id_owned, &file_id_owned)?;
            ingest::delete_file_graph(&core_db, &org_id, &project_id_owned, &file_id_owned)?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| ProtocolError::internal("knowledge cleanup task panicked"))?
        .map_err(|e| db_error("delete_knowledge", e))?;
    }
    let ok = repository::delete_source_file_row(&pool, file_id)
        .map_err(|e| db_error("file_delete", e))?;
    if ok && !file.sha256.is_empty() {
        let refs = repository::blob_ref_count(&pool, &file.sha256)
            .map_err(|e| db_error("blob_ref_count", e))?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
    let query = query.trim();
    if query.is_empty() {
        return Err(ProtocolError::bad_request("query is required"));
    }
    let limit = limit.clamp(1, 50) as usize;

    let vectors = ingest::embed_texts(&ctx.state.router, vec![query.to_string()])
        .await
        .map_err(|e| ProtocolError::internal(format!("query embedding: {e}")))?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
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

fn activity_visible(entry: &ActivityRecord, access: &ProjectAccessWire, user_id: &str) -> bool {
    let area = match entry.object_type.as_str() {
        "chat" => {
            return entry.actor_user_id == user_id
                && access.allows(ProjectArea::Chat, ProjectPermissionLevel::Read)
        }
        "task" | "task_comment" => return task_access(access, ProjectPermissionLevel::Read),
        "case" | "test_case" | "suite" | "run" | "run_item" | "run_step" | "generation"
        | "schedule" => ProjectArea::Tests,
        "environment" => ProjectArea::Environments,
        "build_profile" => ProjectArea::Repos,
        "source" | "file" | "ingest_job" | "ml_link" => ProjectArea::Knowledge,
        "project" | "settings" | "member" | "tag" => ProjectArea::Settings,
        _ => return false,
    };
    access.allows(area, ProjectPermissionLevel::Read)
}

fn overview_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, access) = require_project_access(ctx, org, project_id)?;
    let pool = open_project_pool(project_id)?;
    // Automated runs orphaned by a restart would otherwise be counted as open
    // forever — reconcile before reading the counters.
    auto_runs::reconcile_running(&pool);
    let kpis = repository::project_kpis(&pool).map_err(|e| db_error("kpis", e))?;
    let f2 =
        repository::project_f2_kpis(&pool, &org.user_id).map_err(|e| db_error("kpis_f2", e))?;
    let f3 = repository::project_f3_kpis(&pool).map_err(|e| db_error("kpis_f3", e))?;
    let f4 = schedules::project_f4_kpis(&pool).map_err(|e| db_error("kpis_f4", e))?;
    let member_count =
        repository::member_count(project_id).map_err(|e| db_error("member_count", e))?;
    let my_chat_count =
        repository::count_chats(project_id, &org.user_id).map_err(|e| db_error("chat_count", e))?;
    let (mut entries, _has_more) =
        repository::list_activity(&pool, None, 20).map_err(|e| db_error("activity", e))?;
    entries.retain(|entry| activity_visible(entry, &access, &org.user_id));
    let knowledge = access.allows(ProjectArea::Knowledge, ProjectPermissionLevel::Read);
    let tests = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Read);
    let tasks = task_access(&access, ProjectPermissionLevel::Read);
    let environments = access.allows(ProjectArea::Environments, ProjectPermissionLevel::Read);
    let ids: Vec<String> = entries.iter().map(|e| e.actor_user_id.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    Ok(ps(ProjectStudioPayload::OverviewResponse {
        kpis: OverviewKpis {
            sources_total: if knowledge { kpis.sources_total } else { 0 },
            sources_ready: if knowledge { kpis.sources_ready } else { 0 },
            files_total: if knowledge { kpis.files_total } else { 0 },
            chunks_total: if knowledge { kpis.chunks_total } else { 0 },
            member_count,
            open_ingest_jobs: if knowledge { kpis.open_ingest_jobs } else { 0 },
            my_chat_count: if access.allows(ProjectArea::Chat, ProjectPermissionLevel::Read) {
                my_chat_count
            } else {
                0
            },
            cases_total: if tests { f2.cases_total } else { 0 },
            cases_approved: if tests { f2.cases_approved } else { 0 },
            suites_total: if tests { f2.suites_total } else { 0 },
            runs_open: if tests { f2.runs_open } else { 0 },
            my_run_items_pending: if tests { f2.my_run_items_pending } else { 0 },
            tasks_open: if tasks { f2.tasks_open } else { 0 },
            defects_open: if tasks { f2.defects_open } else { 0 },
            generations_running: if tests { f2.generations_running } else { 0 },
            environments_approved: if environments {
                f3.environments_approved
            } else {
                0
            },
            environments_pending: if environments {
                f3.environments_pending
            } else {
                0
            },
            auto_runs_open: if tests { f3.auto_runs_open } else { 0 },
            schedules_enabled: if tests { f4.schedules_enabled } else { 0 },
            schedules_blocked: if tests { f4.schedules_blocked } else { 0 },
            ml_links: if knowledge { f4.ml_links } else { 0 },
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
    let (_record, access) = require_project_access(ctx, org, project_id)?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    let (mut entries, has_more) = repository::list_activity(&pool, before_id, limit)
        .map_err(|e| db_error("activity_list", e))?;
    entries.retain(|entry| activity_visible(entry, &access, &org.user_id));
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Chat,
        ProjectPermissionLevel::Read,
    )?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Chat,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Chat,
        ProjectPermissionLevel::Write,
    )?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Chat,
        ProjectPermissionLevel::Write,
    )?;
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
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Chat,
        ProjectPermissionLevel::Read,
    )?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Read,
    )?;
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
            key_prefix: record.key_prefix.clone(),
            key_prefix_locked: tasks::project_key_prefix_locked(&pool)
                .map_err(|e| db_error("key_prefix_locked", e))?,
            name: record.name,
            description: record.description,
            modules: parse_modules_json(&record.modules_json),
            agents,
            tags,
            graph_extraction: ingest::graph_extraction_enabled(&pool),
        },
    }))
}

fn settings_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    agents_json: Option<&str>,
    modules: Option<&[String]>,
    graph_extraction: Option<bool>,
    key_prefix: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;

    if name.is_some() || description.is_some() || key_prefix.is_some() {
        let new_name = name.map(str::trim).unwrap_or(&record.name);
        if new_name.is_empty() || new_name.len() > 200 {
            return Err(ProtocolError::bad_request("project name is required"));
        }
        let new_desc = description.map(str::trim).unwrap_or(&record.description);
        repository::update_project_name_desc(
            &org.org_id,
            project_id,
            new_name,
            new_desc,
            key_prefix,
        )
        .map_err(|e| {
            map_unique(
                "settings_save",
                "a project with this name already exists",
                e,
            )
        })?;
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
            if map
                .insert(binding.function.clone(), binding.agent_id)
                .is_some()
            {
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
    if let Some(enabled) = graph_extraction {
        // Recorded like the module toggle: enabling it changes what every future
        // ingest of this project SPENDS (one extra model pass per chunk batch),
        // so who turned it on has to be answerable from the activity log.
        repository::set_setting(
            &pool,
            ingest::GRAPH_EXTRACTION_SETTING,
            ingest::graph_extraction_value(enabled),
        )
        .map_err(|e| db_error("settings_save", e))?;
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "settings.graph_extraction_changed",
            "settings",
            "",
            &serde_json::json!({ "enabled": enabled }).to_string(),
        );
    }
    if let Some(requested) = modules {
        let normalized = normalize_modules(requested)?;
        // Disabling a module only hides its tab and handlers: the cases, runs,
        // tasks and chats it produced stay in `project.db`, so the toggle is
        // reversible and never destroys work. No cascade delete here.
        let modules_json = serde_json::to_string(&normalized)
            .map_err(|e| ProtocolError::internal(format!("modules serialize: {e}")))?;
        repository::update_project_modules(&org.org_id, project_id, &modules_json)
            .map_err(|e| db_error("settings_save", e))?;
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "settings.modules_changed",
            "settings",
            "",
            &serde_json::json!({ "modules": normalized }).to_string(),
        );
    } else {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "settings.saved",
            "settings",
            "",
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::SettingsSaveResult { ok: true }))
}

fn tag_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    tag_id: Option<&str>,
    name: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
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
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
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

// =============================================================================
// F2 wire mapping helpers
// =============================================================================

fn conflict() -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::Conflict,
        "version conflict: case was modified by someone else — reload and retry",
    )
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn attachments_from_json(raw: &str) -> Vec<AttachmentWire> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Validates and canonicalizes an attachments payload ('' = none). Every
/// entry must be a content hash of the project blob store.
fn normalize_attachments(
    ctx: &HandlerContext,
    record: &ProjectRecord,
    owner: Option<&media::AttachmentOwner>,
    raw: &str,
) -> Result<String, ProtocolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok("[]".to_string());
    }
    let list: Vec<AttachmentWire> = serde_json::from_str(raw)
        .map_err(|e| ProtocolError::bad_request(format!("invalid attachments_json: {e}")))?;
    let org = require_read(ctx)?;
    let dir = std::path::Path::new(&record.dir_path);
    let files = media::safe_directory(dir, "files")
        .map_err(|_| ProtocolError::bad_request("attachment storage is unavailable"))?;
    for entry in &list {
        if !is_sha256_hex(&entry.sha256) {
            return Err(ProtocolError::bad_request(
                "attachment sha256 must be 64 lowercase hex characters",
            ));
        }
        let staged = ingest::finalized_meta(
            &org.org_id,
            &org.user_id,
            &record.project_id,
            dir,
            &entry.sha256,
        );
        let retained = owner
            .and_then(|owner| {
                require_attachment(ctx, &record.project_id, owner, &entry.sha256).ok()
            })
            .map(|(_, attachment)| attachment);
        let authorized = match (staged, retained) {
            (Some(meta), _) => meta.size_bytes == entry.size_bytes && meta.mime == entry.mime,
            (None, Some(meta)) => meta.size_bytes == entry.size_bytes && meta.mime == entry.mime,
            (None, None) => false,
        };
        if !authorized {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "attachment must be your completed upload or a readable reference of this record",
            ));
        }
        let file = media::open_regular(&files.join(&entry.sha256))
            .map_err(|_| ProtocolError::bad_request("attachment original is unavailable"))?;
        if file
            .metadata()
            .map_err(|_| ProtocolError::bad_request("attachment metadata is unavailable"))?
            .len()
            != entry.size_bytes
        {
            return Err(ProtocolError::bad_request(
                "attachment size does not match its original",
            ));
        }
        if entry.name.trim().is_empty()
            || entry.name.chars().count() > 255
            || entry.name.chars().any(char::is_control)
        {
            return Err(ProtocolError::bad_request("invalid attachment filename"));
        }
    }
    serde_json::to_string(&list)
        .map_err(|e| ProtocolError::internal(format!("attachments serialize: {e}")))
}

/// Validates a links payload ('' = none): a JSON array of
/// `{kind:'case'|'run'|'run_item'|'step', id, label}` objects.
fn normalize_links(raw: &str) -> Result<String, ProtocolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok("[]".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| ProtocolError::bad_request(format!("invalid links_json: {e}")))?;
    let Some(entries) = value.as_array() else {
        return Err(ProtocolError::bad_request("links_json must be an array"));
    };
    for entry in entries {
        let kind = entry.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if !matches!(kind, "case" | "run" | "run_item" | "step") {
            return Err(ProtocolError::bad_request(format!(
                "unknown link kind '{kind}'"
            )));
        }
        if entry
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .is_empty()
        {
            return Err(ProtocolError::bad_request("link entry requires 'id'"));
        }
    }
    Ok(value.to_string())
}

fn display_name(names: &HashMap<String, (String, String)>, user_id: &str) -> String {
    if user_id.is_empty() {
        return String::new();
    }
    names
        .get(user_id)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| user_id.to_string())
}

fn case_to_wire(item: CaseListItem, names: &HashMap<String, (String, String)>) -> TestCaseInfo {
    let record = item.record;
    TestCaseInfo {
        created_by_name: display_name(names, &record.created_by),
        attachment_count: attachments_from_json(&record.attachments_json).len() as u32,
        linked_source_ids: serde_json::from_str(&record.linked_sources_json).unwrap_or_default(),
        case_id: record.case_id,
        kind: record.kind,
        title: record.title,
        priority: record.priority,
        status: record.status,
        status_reason: record.status_reason,
        review_state: record.review_state,
        origin: record.origin,
        generation_run_id: record.generation_run_id,
        language: record.language,
        current_version: record.current_version,
        tag_ids: item.tag_ids,
        last_result: item.last_result,
        created_by: record.created_by,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

/// F3 fields of a run header (environment name, runner binding, error-item
/// count, perf summary), loaded for a WHOLE page of runs at once. Doing it per
/// row cost three queries per list entry.
#[derive(Default)]
struct RunExtras {
    /// run_id -> (runner_service_id, perf_summary_json)
    meta: HashMap<String, (String, String)>,
    /// run_id -> items in status 'error'
    errored: HashMap<String, u32>,
    /// environment_id -> name
    env_names: HashMap<String, String>,
}

impl RunExtras {
    fn load(pool: &crate::db::DbPool, run_ids: &[String]) -> Self {
        let mut out = Self::default();
        if run_ids.is_empty() {
            return out;
        }
        let Ok(conn) = pool.read() else {
            return out;
        };
        let placeholders = std::iter::repeat_n("?", run_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        if let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT run_id, runner_service_id, perf_summary_json FROM auto_run_meta \
             WHERE run_id IN ({placeholders})"
        )) {
            if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(run_ids), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            }) {
                for (run_id, service_id, perf) in rows.flatten() {
                    out.meta.insert(run_id, (service_id, perf));
                }
            }
        }
        if let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT run_id, COUNT(*) FROM test_run_items WHERE status = 'error' \
             AND run_id IN ({placeholders}) GROUP BY run_id"
        )) {
            if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(run_ids), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                for (run_id, count) in rows.flatten() {
                    out.errored.insert(run_id, count.max(0) as u32);
                }
            }
        }
        // The environment table is per project and tiny — one full read beats a
        // lookup per run.
        if let Ok(mut stmt) = conn.prepare("SELECT environment_id, name FROM environments") {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                for (environment_id, name) in rows.flatten() {
                    out.env_names.insert(environment_id, name);
                }
            }
        }
        out
    }

    fn for_run(&self, record: &RunRecord) -> (String, String, u32, Option<String>) {
        let meta = self.meta.get(&record.run_id);
        let environment_name = self
            .env_names
            .get(&record.environment_id)
            .cloned()
            .unwrap_or_default();
        let perf_summary_json = meta
            .map(|(_, perf)| perf.clone())
            .filter(|s| !s.is_empty() && s != "[]");
        (
            environment_name,
            meta.map(|(service_id, _)| service_id.clone())
                .unwrap_or_default(),
            self.errored.get(&record.run_id).copied().unwrap_or(0),
            perf_summary_json,
        )
    }
}

fn run_to_wire(
    extras: &RunExtras,
    record: RunRecord,
    counts: RunCounts,
    suite_name: String,
    names: &HashMap<String, (String, String)>,
) -> TestRunInfo {
    let (environment_name, runner_service_id, errored, perf_summary_json) = extras.for_run(&record);
    TestRunInfo {
        created_by_name: display_name(names, &record.created_by),
        run_id: record.run_id,
        run_no: record.run_no,
        name: record.name,
        suite_id: record.suite_id,
        suite_name,
        run_type: record.run_type,
        environment_id: record.environment_id,
        env_note: record.env_note,
        assignment_mode: record.assignment_mode,
        status: record.status,
        total: counts.total,
        passed: counts.passed,
        failed: counts.failed,
        blocked: counts.blocked,
        skipped: counts.skipped,
        pending: counts.pending,
        in_progress: counts.in_progress,
        created_by: record.created_by,
        started_at: record.started_at,
        finished_at: record.finished_at,
        environment_name,
        runner_service_id,
        errored,
        perf_summary_json,
    }
}

fn item_to_wire(record: RunItemRecord, names: &HashMap<String, (String, String)>) -> RunItemWire {
    RunItemWire {
        assigned_to_name: display_name(names, &record.assigned_to),
        attachments: attachments_from_json(&record.attachments_json),
        item_id: record.item_id,
        run_id: record.run_id,
        case_id: record.case_id,
        case_title: record.case_title,
        case_version: record.case_version,
        position: record.position,
        assigned_to: record.assigned_to,
        status: record.status,
        result_note: record.result_note,
        tester_config: record.tester_config,
        duration_secs: record.duration_secs,
        steps_total: record.steps_total,
        steps_done: record.steps_done,
        claimed_at: record.claimed_at,
        finished_at: record.finished_at,
    }
}

fn step_to_wire(record: RunStepRecord) -> RunStepWire {
    RunStepWire {
        attachments: attachments_from_json(&record.attachments_json),
        step_index: record.step_index,
        action: record.action,
        expected: record.expected,
        status: record.status,
        note: record.note,
    }
}

fn task_to_wire(record: TaskRecord, names: &HashMap<String, (String, String)>) -> TaskInfo {
    TaskInfo {
        assigned_to_name: display_name(names, &record.assigned_to),
        created_by_name: display_name(names, &record.created_by),
        task_id: record.task_id,
        task_no: record.task_no,
        task_key: record.task_key,
        parent_task_id: record.parent_task_id,
        archived_at: record.archived_at,
        task_type: record.task_type,
        title: record.title,
        severity: record.severity,
        priority: record.priority,
        status: record.status,
        assigned_to: record.assigned_to,
        due_date: record.due_date,
        links_json: record.links_json,
        comment_count: record.comment_count,
        created_by: record.created_by,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn comment_to_wire(
    record: TaskCommentRecord,
    names: &HashMap<String, (String, String)>,
) -> TaskCommentWire {
    TaskCommentWire {
        mention_user_ids: serde_json::from_str(&record.mention_user_ids_json).unwrap_or_default(),
        author_name: display_name(names, &record.author_user_id),
        comment_id: record.comment_id,
        author_user_id: record.author_user_id,
        body_md: record.body_md,
        created_at: record.created_at,
        edited_at: record.edited_at,
    }
}

fn generation_to_wire(
    record: GenerationRunRecord,
    names: &HashMap<String, (String, String)>,
) -> GenerationRunInfo {
    let agent_name = repository::resolve_agent_label(&record.agent_id)
        .map(|(name, _)| name)
        .unwrap_or_default();
    GenerationRunInfo {
        started_by_name: display_name(names, &record.started_by),
        agent_name,
        source_ids: serde_json::from_str(&record.source_ids_json).unwrap_or_default(),
        gen_id: record.gen_id,
        kind: record.kind,
        status: record.status,
        agent_id: record.agent_id,
        agent_run_id: record.agent_run_id,
        instructions: record.instructions,
        requested_count: record.requested_count,
        max_cases: record.max_cases,
        cases_generated: record.cases_generated,
        cases_accepted: record.cases_accepted,
        cases_rejected: record.cases_rejected,
        error: if record.error.is_empty() {
            None
        } else {
            Some(record.error)
        },
        started_by: record.started_by,
        started_at: record.started_at,
        finished_at: record.finished_at,
    }
}

fn cases_to_wire(items: Vec<CaseListItem>) -> Vec<TestCaseInfo> {
    let ids: Vec<String> = items.iter().map(|i| i.record.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    items
        .into_iter()
        .map(|item| case_to_wire(item, &names))
        .collect()
}

// =============================================================================
// F2: manual test cases (T01, T02)
// =============================================================================

#[allow(clippy::too_many_arguments)]
fn cases_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    kind: &str,
    status: &str,
    priority: &str,
    tag_id: &str,
    origin: &str,
    search: &str,
    offset: u32,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    let filters = ps_tests::CaseFilters {
        kind,
        status,
        priority,
        tag_id,
        origin,
        search,
    };
    let (items, total) = ps_tests::list_cases(&pool, &filters, offset, limit)
        .map_err(|e| db_error("cases_list", e))?;
    Ok(ps(ProjectStudioPayload::CasesListResponse {
        cases: cases_to_wire(items),
        total,
    }))
}

fn case_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
    include_versions: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let item = ps_tests::get_case(&pool, case_id)
        .map_err(|e| db_error("case_get", e))?
        .ok_or_else(|| ProtocolError::not_found("case not found"))?;
    let content_json = item.record.content_json.clone();
    let attachments = attachments_from_json(&item.record.attachments_json);
    let versions = if include_versions {
        ps_tests::list_versions(&pool, case_id).map_err(|e| db_error("case_versions", e))?
    } else {
        Vec::new()
    };
    let mut ids: Vec<String> = versions.iter().map(|v| v.created_by.clone()).collect();
    ids.push(item.record.created_by.clone());
    let names = repository::resolve_user_refs(&ids);
    let versions = versions
        .into_iter()
        .map(|v| CaseVersionInfo {
            created_by_name: display_name(&names, &v.created_by),
            version: v.version,
            change_note: v.change_note,
            created_by: v.created_by,
            created_at: v.created_at,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::CaseGetResponse {
        detail: TestCaseDetail {
            info: case_to_wire(item, &names),
            content_json,
            attachments,
            versions,
        },
    }))
}

/// Shared field validation of a case save (create + edit). `content_json` is
/// checked against the contract of its kind — the same validator the agent
/// sink uses, so a hand-written and a generated case obey one rule set.
fn validate_case_fields(
    kind: &str,
    language: &str,
    title: &str,
    priority: &str,
    content_json: &str,
) -> Result<(), ProtocolError> {
    if !generation::GENERATION_KINDS.contains(&kind) {
        return Err(ProtocolError::bad_request(format!(
            "unknown case kind '{kind}'"
        )));
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ProtocolError::bad_request(
            "title must be 1..200 characters",
        ));
    }
    if !ps_tests::CASE_PRIORITIES.contains(&priority) {
        return Err(ProtocolError::bad_request(format!(
            "unknown priority '{priority}'"
        )));
    }
    generation::validate_case_content(kind, language, content_json)
        .map_err(ProtocolError::bad_request)?;
    Ok(())
}

/// Effective language of a case save: code kinds default to python, manual
/// cases keep the natural-language tag the project uses.
fn case_language(kind: &str, content_json: &str) -> String {
    if !generation::is_code_kind(kind) {
        return "pl".to_string();
    }
    serde_json::from_str::<serde_json::Value>(content_json)
        .ok()
        .and_then(|v| {
            v.get("language")
                .and_then(|l| l.as_str())
                .map(|l| l.trim().to_ascii_lowercase())
        })
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "python".to_string())
}

#[allow(clippy::too_many_arguments)]
fn case_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: Option<&str>,
    kind: &str,
    title: &str,
    priority: &str,
    content_json: &str,
    tag_ids: &[String],
    linked_source_ids: &[String],
    attachments_json: &str,
    expected_version: Option<u32>,
    change_note: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let language = case_language(kind, content_json);
    validate_case_fields(kind, &language, title, priority, content_json)?;
    let attachment_owner = case_id.map(|id| media::AttachmentOwner {
        kind: AttachmentOwnerKind::Case,
        id: id.into(),
        step_index: None,
    });
    let attachments_json =
        normalize_attachments(ctx, &record, attachment_owner.as_ref(), attachments_json)?;
    let pool = open_project_pool(project_id)?;
    let input = ps_tests::CaseContentInput {
        kind,
        title: title.trim(),
        priority,
        content_json,
        tag_ids,
        linked_source_ids,
        attachments_json: &attachments_json,
    };
    match case_id {
        None => {
            let case_id = ps_tests::create_case(&pool, &input, None, change_note, &org.user_id)
                .map_err(|e| db_error("case_create", e))?;
            repository::set_case_language(&pool, &case_id, &language)
                .map_err(|e| db_error("case_language", e))?;
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "case.created",
                "case",
                &case_id,
                &serde_json::json!({ "title": title.trim() }).to_string(),
            );
            Ok(ps(ProjectStudioPayload::CaseSaveResponse {
                case_id,
                version: 1,
            }))
        }
        Some(case_id) => {
            let expected = expected_version.ok_or_else(|| {
                ProtocolError::bad_request("expected_version is required when editing")
            })?;
            match ps_tests::update_case(&pool, case_id, expected, &input, change_note, &org.user_id)
                .map_err(|e| db_error("case_update", e))?
            {
                ps_tests::CaseUpdateOutcome::Saved(version) => {
                    repository::set_case_language(&pool, case_id, &language)
                        .map_err(|e| db_error("case_language", e))?;
                    activity::record(
                        &pool,
                        &org.user_id,
                        "user",
                        "case.updated",
                        "case",
                        case_id,
                        &serde_json::json!({ "version": version }).to_string(),
                    );
                    Ok(ps(ProjectStudioPayload::CaseSaveResponse {
                        case_id: case_id.to_string(),
                        version,
                    }))
                }
                ps_tests::CaseUpdateOutcome::Conflict => Err(conflict()),
                ps_tests::CaseUpdateOutcome::NotFound => {
                    Err(ProtocolError::not_found("case not found"))
                }
                ps_tests::CaseUpdateOutcome::NotEditable => Err(ProtocolError::bad_request(
                    "only draft/review cases are editable",
                )),
            }
        }
    }
}

/// Applies one status transition with the section-C role matrix. Shared by
/// the single and bulk handlers; returns Ok(false) when the transition is
/// disallowed for this case/caller (bulk skips, single errors).
fn apply_status_transition(
    pool: &crate::db::DbPool,
    access: &ProjectAccessWire,
    case_id: &str,
    target: &str,
    reason: &str,
) -> Result<Result<bool, &'static str>, ProtocolError> {
    let Some(item) = ps_tests::get_case(pool, case_id).map_err(|e| db_error("case_get", e))? else {
        return Ok(Err("case not found"));
    };
    let from = item.record.status.as_str();
    let Some((minimum, needs_reason)) = ps_tests::transition_requirement(from, target) else {
        return Ok(Err("transition not allowed"));
    };
    if !access.allows(ProjectArea::Tests, minimum) {
        return Ok(Err("requires test administration"));
    }
    if needs_reason && reason.trim().is_empty() {
        return Ok(Err("a reason is required for this downgrade"));
    }
    let ok = ps_tests::set_case_status(pool, case_id, from, target, reason.trim())
        .map_err(|e| db_error("case_status", e))?;
    Ok(Ok(ok))
}

fn case_status_set_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
    status: &str,
    reason: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    if !ps_tests::CASE_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    let pool = open_project_pool(project_id)?;
    let ok = match apply_status_transition(&pool, &access, case_id, status, reason)? {
        Ok(ok) => ok,
        Err("case not found") => return Err(ProtocolError::not_found("case not found")),
        Err("requires test administration") => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "this transition requires test administration",
            ))
        }
        Err(message) => return Err(ProtocolError::bad_request(message)),
    };
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "case.status_changed",
            "case",
            case_id,
            &serde_json::json!({ "status": status, "reason": reason.trim() }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::CaseStatusSetResult { ok }))
}

fn cases_bulk_status_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_ids: &[String],
    status: &str,
    reason: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    if !ps_tests::CASE_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    if case_ids.is_empty() || case_ids.len() > 200 {
        return Err(ProtocolError::bad_request(
            "case_ids must contain 1..200 ids",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let mut updated = 0u32;
    for case_id in case_ids {
        // Bulk semantics: cases the caller may not (or must not) transition
        // are skipped, the rest proceed — `updated` reports the real count.
        if let Ok(true) = apply_status_transition(&pool, &access, case_id, status, reason)? {
            updated += 1;
        }
    }
    if updated > 0 {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "case.status_changed",
            "case",
            "",
            &serde_json::json!({ "status": status, "count": updated }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::CasesBulkStatusResponse {
        updated,
    }))
}

fn case_duplicate_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let new_id = ps_tests::duplicate_case(&pool, case_id, &org.user_id)
        .map_err(|e| db_error("case_duplicate", e))?
        .ok_or_else(|| ProtocolError::not_found("case not found"))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "case.duplicated",
        "case",
        &new_id,
        &serde_json::json!({ "source_case_id": case_id }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::CaseDuplicateResponse {
        case_id: new_id,
    }))
}

fn case_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let item = ps_tests::get_case(&pool, case_id)
        .map_err(|e| db_error("case_get", e))?
        .ok_or_else(|| ProtocolError::not_found("case not found"))?;
    // Approved cases and cases referenced by run snapshots are never deleted —
    // deprecate instead (running executions must keep resolving their pins).
    if item.record.status == "approved" {
        return Err(ProtocolError::bad_request(
            "approved cases cannot be deleted — deprecate instead",
        ));
    }
    let refs =
        ps_tests::case_run_item_refs(&pool, case_id).map_err(|e| db_error("case_refs", e))?;
    if refs > 0 {
        return Err(ProtocolError::bad_request(
            "case is referenced by test runs — deprecate instead",
        ));
    }
    // Test writers may delete their own drafts; administrators also remove reviews.
    let allowed = if access.allows(ProjectArea::Tests, ProjectPermissionLevel::Admin) {
        matches!(item.record.status.as_str(), "draft" | "review")
    } else {
        item.record.status == "draft" && item.record.created_by == org.user_id
    };
    if !allowed {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "test writers may delete only their own drafts",
        ));
    }
    let ok = ps_tests::delete_case(&pool, case_id).map_err(|e| db_error("case_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "case.deleted",
            "case",
            case_id,
            &serde_json::json!({ "title": item.record.title }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::CaseDeleteResult { ok }))
}

fn case_version_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
    version: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    // Visibility gate first: versions of a pending case are as hidden as the
    // case itself.
    ps_tests::get_case(&pool, case_id)
        .map_err(|e| db_error("case_get", e))?
        .ok_or_else(|| ProtocolError::not_found("case not found"))?;
    let v = ps_tests::get_version(&pool, case_id, version)
        .map_err(|e| db_error("case_version", e))?
        .ok_or_else(|| ProtocolError::not_found("version not found"))?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&v.created_by));
    Ok(ps(ProjectStudioPayload::CaseVersionGetResponse {
        content_json: v.content_json,
        change_note: v.change_note,
        created_by_name: display_name(&names, &v.created_by),
        created_at: v.created_at,
    }))
}

fn case_restore_version_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
    version: u32,
    expected_version: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    match ps_tests::restore_version(&pool, case_id, version, expected_version, &org.user_id)
        .map_err(|e| db_error("case_restore", e))?
    {
        ps_tests::CaseUpdateOutcome::Saved(new_version) => {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "case.restored",
                "case",
                case_id,
                &serde_json::json!({ "from_version": version, "version": new_version }).to_string(),
            );
            Ok(ps(ProjectStudioPayload::CaseRestoreVersionResponse {
                case_id: case_id.to_string(),
                version: new_version,
            }))
        }
        ps_tests::CaseUpdateOutcome::Conflict => Err(conflict()),
        ps_tests::CaseUpdateOutcome::NotFound => {
            Err(ProtocolError::not_found("case or version not found"))
        }
        ps_tests::CaseUpdateOutcome::NotEditable => Err(ProtocolError::bad_request(
            "only draft/review cases are editable",
        )),
    }
}

fn cases_import_csv_v1(
    ctx: &HandlerContext,
    project_id: &str,
    csv_text: &str,
    dry_run: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    if csv_text.len() > ps_tests::CSV_MAX_BYTES {
        return Err(ProtocolError::bad_request("CSV exceeds the 2 MiB limit"));
    }
    let (rows, errors) = ps_tests::parse_csv(csv_text);
    let errors: Vec<CsvImportError> = errors
        .into_iter()
        .map(|(line, message)| CsvImportError { line, message })
        .collect();
    // All-or-nothing: any invalid row (or a dry run) writes nothing.
    if dry_run || !errors.is_empty() {
        return Ok(ps(ProjectStudioPayload::CasesImportCsvResponse {
            created: if errors.is_empty() {
                rows.len() as u32
            } else {
                0
            },
            errors,
        }));
    }
    let pool = open_project_pool(project_id)?;
    let created = ps_tests::import_cases(&pool, &rows, &org.user_id)
        .map_err(|e| db_error("csv_import", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "cases.imported",
        "case",
        "",
        &serde_json::json!({ "created": created }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::CasesImportCsvResponse {
        created,
        errors,
    }))
}

pub(crate) fn require_attachment(
    ctx: &HandlerContext,
    project_id: &str,
    owner: &media::AttachmentOwner,
    sha256: &str,
) -> Result<(ProjectRecord, AttachmentWire), ProtocolError> {
    let org = require_read(ctx)?;
    if !media::is_sha256(sha256) {
        return Err(ProtocolError::bad_request(
            "sha256 must be 64 lowercase hex characters",
        ));
    }
    if (owner.kind == AttachmentOwnerKind::RunStep) != owner.step_index.is_some() {
        return Err(ProtocolError::bad_request(
            "step_index is required only for run_step attachments",
        ));
    }
    let (record, _) = if owner.kind == AttachmentOwnerKind::Task {
        require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?
    } else {
        require_project(
            ctx,
            org,
            project_id,
            ProjectArea::Tests,
            ProjectPermissionLevel::Read,
        )?
    };
    let pool = open_project_pool(project_id)?;
    let attachment = if owner.kind == AttachmentOwnerKind::Task {
        tasks::task_attachment_metadata(&pool, &owner.id, sha256).map_err(|e| db_error("task_attachment", e))?
    } else {
        use rusqlite::OptionalExtension;
        let conn = pool.read().map_err(|e| db_error("attachment_reference", e))?;
        let raw: Option<String> = match owner.kind {
            AttachmentOwnerKind::Case => conn.query_row("SELECT attachments_json FROM test_cases WHERE case_id = ?1", [&owner.id], |row| row.get(0)).optional(),
            AttachmentOwnerKind::RunItem => conn.query_row("SELECT attachments_json FROM test_run_items WHERE item_id = ?1", [&owner.id], |row| row.get(0)).optional(),
            AttachmentOwnerKind::RunStep => conn.query_row("SELECT attachments_json FROM test_run_steps WHERE item_id = ?1 AND step_index = ?2", rusqlite::params![owner.id, owner.step_index], |row| row.get(0)).optional(),
            AttachmentOwnerKind::Task => unreachable!(),
        }.map_err(|e| db_error("attachment_reference", e))?;
        match raw {
            Some(raw) => serde_json::from_str::<Vec<AttachmentWire>>(&raw).map_err(|e| db_error("attachment_reference", e))?
                .into_iter().find(|entry| entry.sha256 == sha256),
            None => None,
        }
    }.ok_or_else(|| ProtocolError::not_found("attachment not found"))?;
    Ok((record, attachment))
}

async fn attachment_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    owner: &media::AttachmentOwner,
    sha256: &str,
    offset: u64,
    max_bytes: u32,
    preview: bool,
) -> Result<MessageBody, ProtocolError> {
    let (record, attachment) = require_attachment(ctx, project_id, owner, sha256)?;
    if max_bytes == 0 || max_bytes as usize > ingest::MAX_UPLOAD_CHUNK_BYTES {
        return Err(ProtocolError::bad_request("read length must be 1..4 MiB"));
    }
    let files = media::safe_directory(std::path::Path::new(&record.dir_path), "files")
        .map_err(|_| ProtocolError::not_found("attachment not found"))?;
    let path = if preview {
        media::preview_path(std::path::Path::new(&record.dir_path), sha256)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?
    } else {
        files.join(sha256)
    };
    let (bytes, total_size, eof) =
        tokio::task::spawn_blocking(move || media::read_range(&path, offset, max_bytes))
            .await
            .map_err(|_| ProtocolError::internal("attachment read worker failed"))?
            .map_err(|_| ProtocolError::not_found("attachment range not found"))?;
    require_attachment(ctx, project_id, owner, sha256)?;
    let mime = if preview {
        "video/mp4".to_string()
    } else {
        attachment.mime
    };
    let filename = if preview {
        format!("{}.mp4", attachment.name)
    } else {
        attachment.name
    };
    Ok(ps(ProjectStudioPayload::AttachmentGetResponse {
        bytes,
        total_size,
        mime,
        filename,
        eof,
    }))
}

fn require_upload_access(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<ProjectRecord, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    if !access.can_create_tasks
        && ![
            ProjectArea::Knowledge,
            ProjectArea::Repos,
            ProjectArea::Tests,
        ]
        .iter()
        .any(|area| access.allows(*area, ProjectPermissionLevel::Write))
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "attachment upload access denied",
        ));
    }
    require_active(&record)?;
    Ok(record)
}

fn upload_expiry(state: &ingest::UploadState) -> String {
    chrono::DateTime::from_timestamp_millis(state.expires_at_ms)
        .map(|time| time.to_rfc3339())
        .unwrap_or_default()
}

fn attachment_upload_status(
    ctx: &HandlerContext,
    project_id: &str,
    upload_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project_access(ctx, org, project_id)?;
    let state = ingest::upload_status(
        &org.org_id,
        &org.user_id,
        project_id,
        std::path::Path::new(&record.dir_path),
        upload_id,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?
    .ok_or_else(|| ProtocolError::not_found("upload not found"))?;
    let expires_at = upload_expiry(&state);
    Ok(ps(ProjectStudioPayload::AttachmentUploadStatusResponse {
        upload_id: state.upload_id,
        filename: state.filename,
        mime: state.mime,
        sha256: state.sha256,
        total_size: state.total_size,
        next_offset: state.next_offset,
        complete: state.complete,
        expires_at,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn attachment_upload_chunk(
    ctx: &HandlerContext,
    project_id: &str,
    upload_id: &str,
    filename: &str,
    mime: &str,
    sha256: &str,
    total_size: u64,
    offset: u64,
    bytes: &[u8],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let record = require_upload_access(ctx, project_id)?;
    let org_id = org.org_id.clone();
    let user_id = org.user_id.clone();
    let project = project_id.to_string();
    let upload_id = upload_id.to_string();
    let filename = filename.to_string();
    let mime = mime.to_string();
    let sha256 = sha256.to_string();
    let bytes = bytes.to_vec();
    let context = ctx.clone();
    let state = tokio::task::spawn_blocking(move || {
        require_upload_access(&context, &project).map_err(|e| anyhow::anyhow!(e.message))?;
        ingest::accept_upload_chunk(&ingest::UploadChunk {
            org_id: &org_id,
            user_id: &user_id,
            project_id: &project,
            dir_path: std::path::Path::new(&record.dir_path),
            upload_id: &upload_id,
            filename: &filename,
            mime: &mime,
            position: ingest::UploadPosition::Offset {
                offset,
                total_size,
                sha256: &sha256,
            },
            bytes: &bytes,
            allowed: &|| {
                require_upload_access(&context, &project)
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!(e.message))
            },
        })
    })
    .await
    .map_err(|_| ProtocolError::internal("upload worker failed"))?
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    require_upload_access(ctx, project_id)?;
    let expires_at = upload_expiry(&state);
    Ok(ps(ProjectStudioPayload::AttachmentUploadChunkResponse {
        upload_id: state.upload_id,
        filename: state.filename,
        mime: state.mime,
        sha256: state.sha256,
        total_size: state.total_size,
        next_offset: state.next_offset,
        complete: state.complete,
        expires_at,
    }))
}

fn attachment_upload_cancel(
    ctx: &HandlerContext,
    project_id: &str,
    upload_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project_access(ctx, org, project_id)?;
    let ok = ingest::cancel_upload(
        &org.org_id,
        &org.user_id,
        project_id,
        std::path::Path::new(&record.dir_path),
        upload_id,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(ps(ProjectStudioPayload::AttachmentUploadCancelResult {
        ok,
    }))
}

fn attachment_preview(
    ctx: &HandlerContext,
    project_id: &str,
    owner: &media::AttachmentOwner,
    sha: &str,
    retry: bool,
) -> Result<MessageBody, ProtocolError> {
    require_attachment(ctx, project_id, owner, sha)?;
    let state = media::request_preview(ctx, project_id, owner, sha, retry)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(ps(ProjectStudioPayload::AttachmentPreviewResponse {
        status: state.status,
        error: state.error,
        mime: "video/mp4".to_string(),
        total_size: state.total_size,
        duration_ms: state.duration_ms,
    }))
}

async fn attachment_usage(
    ctx: &HandlerContext,
    project_id: &str,
    offset: u32,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project_access(ctx, org, project_id)?;
    let tasks_allowed = task_access(&access, ProjectPermissionLevel::Read);
    let tests_allowed = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Read);
    if !tasks_allowed && !tests_allowed {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "attachment usage read access denied",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let dir = std::path::PathBuf::from(record.dir_path);
    let response = tokio::task::spawn_blocking(move || {
        scan_attachment_usage(
            &pool,
            &dir,
            tasks_allowed,
            tests_allowed,
            offset,
            limit.clamp(1, 100),
        )
    })
    .await
    .map_err(|_| ProtocolError::internal("attachment usage worker failed"))?
    .map_err(|e| db_error("attachment_usage", e))?;
    let (_, current) = require_project_access(ctx, org, project_id)?;
    if (tasks_allowed && !task_access(&current, ProjectPermissionLevel::Read))
        || (tests_allowed && !current.allows(ProjectArea::Tests, ProjectPermissionLevel::Read))
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "attachment usage access revoked",
        ));
    }
    Ok(ps(response))
}

fn scan_attachment_usage(
    pool: &crate::db::DbPool,
    dir: &std::path::Path,
    tasks_allowed: bool,
    tests_allowed: bool,
    offset: u32,
    limit: u32,
) -> anyhow::Result<ProjectStudioPayload> {
    let files_dir = media::safe_directory(dir, "files")?;
    let mut unique = HashMap::<String, Option<u64>>::new();
    let mut total_bytes = 0u64;
    let mut file_count = 0u64;
    let mut missing_files = 0u64;
    let mut largest = Vec::<AttachmentUsageFileWire>::new();
    let mut per_task = Vec::new();
    let mut total_tasks = 0u32;
    let mut add = |owner_kind: AttachmentOwnerKind,
                   owner_id: &str,
                   step_index: Option<u32>,
                   task_key: &str,
                   attachment: &AttachmentWire|
     -> u64 {
        if !media::is_sha256(&attachment.sha256) {
            return 0;
        }
        if let Some(size) = unique.get(&attachment.sha256) {
            return size.unwrap_or(0);
        }
        let size = media::open_regular(&files_dir.join(&attachment.sha256))
            .and_then(|file| Ok(file.metadata()?.len()))
            .ok();
        unique.insert(attachment.sha256.clone(), size);
        if let Some(size) = size {
            total_bytes = total_bytes.saturating_add(size);
            file_count += 1;
            largest.push(AttachmentUsageFileWire {
                owner_kind,
                owner_id: owner_id.into(),
                step_index,
                task_key: task_key.into(),
                filename: attachment.name.clone(),
                sha256: attachment.sha256.clone(),
                size_bytes: size,
            });
            largest.sort_by(|a, b| {
                b.size_bytes
                    .cmp(&a.size_bytes)
                    .then_with(|| a.sha256.cmp(&b.sha256))
            });
            largest.truncate(limit as usize);
            size
        } else {
            missing_files += 1;
            0
        }
    };
    if tasks_allowed {
        let mut last_id = String::new();
        loop {
            let batch: Vec<(String, String)> = {
                let conn = pool.read()?;
                let mut statement = conn.prepare("SELECT task_id,task_key FROM tasks WHERE task_id > ?1 ORDER BY task_id LIMIT 100")?;
                let rows = statement.query_map([&last_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if batch.is_empty() {
                break;
            }
            for (task_id, task_key) in &batch {
                let attachments = tasks::task_attachment_inventory(pool, task_id)?;
                if attachments.is_empty() {
                    continue;
                }
                let mut bytes = 0u64;
                let mut count = 0u64;
                for attachment in &attachments {
                    let size = add(
                        AttachmentOwnerKind::Task,
                        task_id,
                        None,
                        task_key,
                        attachment,
                    );
                    if media::open_regular(&files_dir.join(&attachment.sha256)).is_ok() {
                        count += 1;
                    }
                    bytes = bytes.saturating_add(size);
                }
                if total_tasks >= offset && per_task.len() < limit as usize {
                    per_task.push(TaskAttachmentUsageWire {
                        task_id: task_id.clone(),
                        task_key: task_key.clone(),
                        file_count: count,
                        total_bytes: bytes,
                    });
                }
                total_tasks = total_tasks.saturating_add(1);
            }
            last_id = batch
                .last()
                .ok_or_else(|| anyhow::anyhow!("empty attachment batch"))?
                .0
                .clone();
        }
    }
    if tests_allowed {
        for (kind, table, id_column, step_column) in [
            (AttachmentOwnerKind::Case, "test_cases", "case_id", "NULL"),
            (
                AttachmentOwnerKind::RunItem,
                "test_run_items",
                "item_id",
                "NULL",
            ),
            (
                AttachmentOwnerKind::RunStep,
                "test_run_steps",
                "item_id",
                "step_index",
            ),
        ] {
            let conn = pool.read()?;
            let mut statement = conn.prepare(&format!("SELECT {id_column},{step_column},attachments_json FROM {table} ORDER BY {id_column}"))?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<u32>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (id, index, raw) = row?;
                for attachment in serde_json::from_str::<Vec<AttachmentWire>>(&raw)? {
                    add(kind, &id, index, "", &attachment);
                }
            }
        }
    }
    let disk = crate::sync::storage_monitor::report_for_root(dir)?;
    let warning = matches!(
        disk.level,
        crate::sync::storage_monitor::StoragePressureLevel::Warning
            | crate::sync::storage_monitor::StoragePressureLevel::Critical
    );
    Ok(ProjectStudioPayload::AttachmentUsageResponse {
        total_bytes,
        file_count,
        missing_files,
        available_bytes: disk.available_bytes,
        warning,
        largest,
        per_task,
        total_tasks,
        has_more: total_tasks > offset.saturating_add(limit),
    })
}

// =============================================================================
// F2: test suites (T04)
// =============================================================================

fn suite_item_to_wire(
    extras: &RunExtras,
    item: crate::project_studio::tests::SuiteListItem,
) -> Result<SuiteInfo, ProtocolError> {
    let last_run = match item.last_run {
        Some((record, counts)) => {
            let suite_name = item.record.name.clone();
            let names = repository::resolve_user_refs(std::slice::from_ref(&record.created_by));
            Some(run_to_wire(extras, record, counts, suite_name, &names))
        }
        None => None,
    };
    Ok(SuiteInfo {
        suite_id: item.record.suite_id,
        name: item.record.name,
        description: item.record.description,
        case_count: item.case_count,
        has_deprecated: item.has_deprecated,
        last_run,
        created_at: item.record.created_at,
        updated_at: item.record.updated_at,
    })
}

fn suites_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let items = ps_tests::list_suites(&pool).map_err(|e| db_error("suites_list", e))?;
    let run_ids: Vec<String> = items
        .iter()
        .filter_map(|item| item.last_run.as_ref().map(|(r, _)| r.run_id.clone()))
        .collect();
    let extras = RunExtras::load(&pool, &run_ids);
    let mut suites = Vec::with_capacity(items.len());
    for item in items {
        suites.push(suite_item_to_wire(&extras, item)?);
    }
    Ok(ps(ProjectStudioPayload::SuitesListResponse { suites }))
}

fn suite_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    suite_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let item = ps_tests::get_suite(&pool, suite_id)
        .map_err(|e| db_error("suite_get", e))?
        .ok_or_else(|| ProtocolError::not_found("suite not found"))?;
    let cases = ps_tests::suite_case_rows(&pool, suite_id)
        .map_err(|e| db_error("suite_cases", e))?
        .into_iter()
        .map(|c| SuiteCaseRef {
            case_id: c.case_id,
            position: c.position,
            title: c.title,
            kind: c.kind,
            status: c.status,
            priority: c.priority,
        })
        .collect();
    let run_ids: Vec<String> = item
        .last_run
        .as_ref()
        .map(|(r, _)| vec![r.run_id.clone()])
        .unwrap_or_default();
    let extras = RunExtras::load(&pool, &run_ids);
    Ok(ps(ProjectStudioPayload::SuiteGetResponse {
        suite: suite_item_to_wire(&extras, item)?,
        cases,
    }))
}

fn suite_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    suite_id: Option<&str>,
    name: &str,
    description: &str,
    case_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ProtocolError::bad_request("suite name is required"));
    }
    if case_ids.len() > 500 {
        return Err(ProtocolError::bad_request(
            "a suite holds at most 500 cases",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let suite_id = ps_tests::save_suite(
        &pool,
        suite_id,
        name,
        description.trim(),
        case_ids,
        &org.user_id,
    )
    .map_err(|e| {
        if e.to_string().contains("unknown case") || e.to_string().contains("suite not found") {
            ProtocolError::bad_request(e.to_string())
        } else {
            map_unique("suite_save", "a suite with this name already exists", e)
        }
    })?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "suite.saved",
        "suite",
        &suite_id,
        &serde_json::json!({ "name": name, "cases": case_ids.len() }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::SuiteSaveResponse { suite_id }))
}

fn suite_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    suite_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let ok = ps_tests::delete_suite(&pool, suite_id).map_err(|e| db_error("suite_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "suite.deleted",
            "suite",
            suite_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::SuiteDeleteResult { ok }))
}

// =============================================================================
// F2: test runs + execution (T06-T09)
// =============================================================================

fn runs_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    status: &str,
    run_type: &str,
    offset: u32,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    let (rows, total) = runs::list_runs(&pool, status, run_type, offset, limit)
        .map_err(|e| db_error("runs_list", e))?;
    let suite_ids: Vec<String> = rows.iter().map(|(r, _)| r.suite_id.clone()).collect();
    let suite_names =
        ps_tests::suite_names(&pool, &suite_ids).map_err(|e| db_error("suite_names", e))?;
    let ids: Vec<String> = rows.iter().map(|(r, _)| r.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let run_ids: Vec<String> = rows.iter().map(|(r, _)| r.run_id.clone()).collect();
    let extras = RunExtras::load(&pool, &run_ids);
    let runs_wire = rows
        .into_iter()
        .map(|(record, counts)| {
            let suite_name = suite_names
                .get(&record.suite_id)
                .cloned()
                .unwrap_or_default();
            run_to_wire(&extras, record, counts, suite_name, &names)
        })
        .collect();
    Ok(ps(ProjectStudioPayload::RunsListResponse {
        runs: runs_wire,
        total,
    }))
}

/// Fans one bulk `run_item_assigned` notification per assignee (skip self —
/// risk F.7).
fn notify_run_assignees(
    org_id: &str,
    actor: &str,
    project_id: &str,
    run_id: &str,
    run_no: u32,
    run_name: &str,
    per_user: &HashMap<String, u32>,
) {
    for (user_id, count) in per_user {
        if user_id.is_empty() || user_id == actor {
            continue;
        }
        notifications::notify(
            org_id,
            user_id,
            project_id,
            "run_item_assigned",
            "Przydzielono Ci testy",
            &format!("{count} przypadków w przebiegu #{run_no} „{run_name}”"),
            &serde_json::json!({ "project_id": project_id, "run_id": run_id }).to_string(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_create_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: &str,
    suite_id: &str,
    case_ids: &[String],
    from_failed_run_id: &str,
    env_note: &str,
    assignment_mode: &str,
    single_assignee: &str,
    assignments: &[RunAssignmentWire],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ProtocolError::bad_request("run name is required"));
    }
    if !matches!(assignment_mode, "single" | "per_case" | "pool") {
        return Err(ProtocolError::bad_request(format!(
            "unknown assignment_mode '{assignment_mode}'"
        )));
    }
    // Exactly ONE case source (XOR).
    let sources = [
        !suite_id.is_empty(),
        !case_ids.is_empty(),
        !from_failed_run_id.is_empty(),
    ]
    .iter()
    .filter(|s| **s)
    .count();
    if sources != 1 {
        return Err(ProtocolError::bad_request(
            "provide exactly one of suite_id, case_ids or from_failed_run_id",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let selected_case_ids: Vec<String> = if !suite_id.is_empty() {
        ps_tests::get_suite(&pool, suite_id)
            .map_err(|e| db_error("suite_get", e))?
            .ok_or_else(|| ProtocolError::not_found("suite not found"))?;
        ps_tests::suite_case_rows(&pool, suite_id)
            .map_err(|e| db_error("suite_cases", e))?
            .into_iter()
            .filter(|c| c.status == "approved")
            .map(|c| c.case_id)
            .collect()
    } else if !case_ids.is_empty() {
        // Clients may repeat an id; a duplicate would trip the UNIQUE run-item
        // constraint, so keep the first occurrence only (order preserved).
        let mut seen = HashSet::new();
        case_ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .cloned()
            .collect()
    } else {
        runs::get_run(&pool, from_failed_run_id)
            .map_err(|e| db_error("run_get", e))?
            .ok_or_else(|| ProtocolError::not_found("source run not found"))?;
        runs::failed_case_ids(&pool, from_failed_run_id)
            .map_err(|e| db_error("failed_cases", e))?
            .into_iter()
            .filter(|case_id| {
                // Fresh versions only for cases that are STILL approved.
                matches!(
                    ps_tests::get_case(&pool, case_id),
                    Ok(Some(item)) if item.record.status == "approved"
                )
            })
            .collect()
    };
    if selected_case_ids.is_empty() {
        return Err(ProtocolError::bad_request(
            "no approved cases matched the selection",
        ));
    }
    if selected_case_ids.len() > 500 {
        return Err(ProtocolError::bad_request("a run holds at most 500 cases"));
    }
    let snapshots = runs::approved_case_snapshots(&pool, &selected_case_ids)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    // Assignment resolution + tester-membership validation.
    let require_tester_member = |user_id: &str| -> Result<(), ProtocolError> {
        let target = repository::project_access(&record, user_id, false)
            .map_err(|e| db_error("project_access", e))?;
        if !target.allows(ProjectArea::Tests, ProjectPermissionLevel::Write) {
            return Err(ProtocolError::bad_request(format!(
                "user '{user_id}' does not have test write access"
            )));
        }
        Ok(())
    };
    let assignees: Vec<String> = match assignment_mode {
        "single" => {
            if single_assignee.is_empty() {
                return Err(ProtocolError::bad_request(
                    "single mode requires single_assignee",
                ));
            }
            require_tester_member(single_assignee)?;
            vec![single_assignee.to_string(); snapshots.len()]
        }
        "per_case" => {
            let map: HashMap<&str, &str> = assignments
                .iter()
                .map(|a| (a.case_id.as_str(), a.user_id.as_str()))
                .collect();
            let mut out = Vec::with_capacity(snapshots.len());
            for snapshot in &snapshots {
                let Some(user_id) = map.get(snapshot.case_id.as_str()).filter(|u| !u.is_empty())
                else {
                    return Err(ProtocolError::bad_request(format!(
                        "per_case mode requires an assignment for case '{}'",
                        snapshot.case_id
                    )));
                };
                out.push(user_id.to_string());
            }
            for user_id in out.iter().collect::<std::collections::HashSet<_>>() {
                require_tester_member(user_id)?;
            }
            out
        }
        _ => vec![String::new(); snapshots.len()],
    };

    let (run_id, run_no) = runs::create_run(
        &pool,
        name,
        suite_id,
        env_note.trim(),
        assignment_mode,
        &snapshots,
        &assignees,
        &org.user_id,
    )
    .map_err(|e| db_error("run_create", e))?;

    activity::record(
        &pool,
        &org.user_id,
        "user",
        "run.created",
        "run",
        &run_id,
        &serde_json::json!({ "name": name, "run_no": run_no, "cases": snapshots.len() })
            .to_string(),
    );
    let mut per_user: HashMap<String, u32> = HashMap::new();
    for assignee in &assignees {
        if !assignee.is_empty() {
            *per_user.entry(assignee.clone()).or_default() += 1;
        }
    }
    notify_run_assignees(
        &org.org_id,
        &org.user_id,
        project_id,
        &run_id,
        run_no,
        name,
        &per_user,
    );
    let _ = repository::touch_project(project_id);
    Ok(ps(ProjectStudioPayload::RunCreateResponse {
        run_id,
        run_no,
    }))
}

fn load_run_wire(
    pool: &crate::db::DbPool,
    record: RunRecord,
    counts: RunCounts,
) -> Result<TestRunInfo, ProtocolError> {
    let suite_names = ps_tests::suite_names(pool, std::slice::from_ref(&record.suite_id))
        .map_err(|e| db_error("suite_names", e))?;
    let suite_name = suite_names
        .get(&record.suite_id)
        .cloned()
        .unwrap_or_default();
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.created_by));
    let extras = RunExtras::load(pool, std::slice::from_ref(&record.run_id));
    Ok(run_to_wire(&extras, record, counts, suite_name, &names))
}

fn run_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let (record, counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    let items = runs::list_run_items(&pool, run_id).map_err(|e| db_error("run_items", e))?;
    let ids: Vec<String> = items.iter().map(|i| i.assigned_to.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let items = items
        .into_iter()
        .map(|item| item_to_wire(item, &names))
        .collect();
    Ok(ps(ProjectStudioPayload::RunGetResponse {
        run: load_run_wire(&pool, record, counts)?,
        items,
    }))
}

fn run_close_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
    cancelled: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (run, _counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    // Manager+ OR the run's creator (section C).
    let is_manager = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Admin);
    if !is_manager && run.created_by != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "closing a run requires test administration or run ownership",
        ));
    }
    let ok = runs::close_run(&pool, run_id, cancelled, &org.user_id)
        .map_err(|e| db_error("run_close", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "run.closed",
            "run",
            run_id,
            &serde_json::json!({ "cancelled": cancelled }).to_string(),
        );
        // One bulk run_closed notification per participating tester.
        if let Ok(assignees) = runs::run_assignees(&pool, run_id) {
            for user_id in assignees {
                if user_id == org.user_id {
                    continue;
                }
                notifications::notify(
                    &org.org_id,
                    &user_id,
                    project_id,
                    "run_closed",
                    if cancelled {
                        "Przebieg testów anulowany"
                    } else {
                        "Przebieg testów zamknięty"
                    },
                    &format!("#{} „{}”", run.run_no, run.name),
                    &serde_json::json!({ "project_id": project_id, "run_id": run_id }).to_string(),
                );
            }
        }
    }
    Ok(ps(ProjectStudioPayload::RunCloseResult { ok }))
}

fn run_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (run, _counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    if run.status == "running" {
        return Err(ProtocolError::bad_request(
            "close or cancel the run before deleting it",
        ));
    }
    let ok = runs::delete_run(&pool, run_id).map_err(|e| db_error("run_delete", e))?;
    if ok {
        // Automated runs additionally own an artifact directory + runner
        // binding; deleting the run must not leave either behind.
        let _ = auto_runs::delete_run_artifacts(&pool, run_id);
        let _ = std::fs::remove_dir_all(auto_runs::run_artifact_dir(
            std::path::Path::new(&record.dir_path),
            run_id,
        ));
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "run.deleted",
            "run",
            run_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::RunDeleteResult { ok }))
}

fn run_item_claim_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
    item_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (run, _counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    if run.status != "running" {
        return Err(ProtocolError::bad_request("run is not running"));
    }
    let claimed = runs::claim_item(&pool, run_id, &org.user_id, item_id)
        .map_err(|e| db_error("item_claim", e))?;
    let item = match claimed {
        Some(item) => {
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "run_item.claimed",
                "run_item",
                &item.item_id,
                &serde_json::json!({ "case_id": item.case_id }).to_string(),
            );
            let names = repository::resolve_user_refs(std::slice::from_ref(&item.assigned_to));
            Some(item_to_wire(item, &names))
        }
        None => None,
    };
    Ok(ps(ProjectStudioPayload::RunItemClaimResponse { item }))
}

/// Loads an item + its run and enforces "own item (tester) or manager".
fn load_owned_item(
    org: &OrgContext,
    access: &ProjectAccessWire,
    pool: &crate::db::DbPool,
    item_id: &str,
) -> Result<(RunItemRecord, RunRecord), ProtocolError> {
    let item = runs::get_run_item(pool, item_id)
        .map_err(|e| db_error("item_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run item not found"))?;
    let (run, _counts) = runs::get_run(pool, &item.run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    let is_manager = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Admin);
    if !is_manager && item.assigned_to != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "this run item belongs to another tester",
        ));
    }
    Ok((item, run))
}

fn run_item_release_v1(
    ctx: &HandlerContext,
    project_id: &str,
    item_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (item, run) = load_owned_item(org, &access, &pool, item_id)?;
    if item.status != "in_progress" {
        return Err(ProtocolError::bad_request("item is not in progress"));
    }
    let ok = runs::release_item(&pool, item_id, run.assignment_mode == "pool")
        .map_err(|e| db_error("item_release", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "run_item.released",
            "run_item",
            item_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::RunItemReleaseResult { ok }))
}

fn run_item_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    item_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Read view: any member may inspect (viewer read-only, section C).
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let item = runs::get_run_item(&pool, item_id)
        .map_err(|e| db_error("item_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run item not found"))?;
    let steps = runs::list_item_steps(&pool, item_id)
        .map_err(|e| db_error("item_steps", e))?
        .into_iter()
        .map(step_to_wire)
        .collect();
    let (preconditions, test_data) =
        runs::item_pinned_content(&pool, &item.case_id, item.case_version)
            .map_err(|e| db_error("item_content", e))?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&item.assigned_to));
    Ok(ps(ProjectStudioPayload::RunItemGetResponse {
        item: item_to_wire(item, &names),
        steps,
        preconditions,
        test_data,
    }))
}

fn run_step_set_v1(
    ctx: &HandlerContext,
    project_id: &str,
    item_id: &str,
    step_index: u32,
    status: &str,
    note: &str,
    attachments_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    if !matches!(status, "" | "passed" | "failed" | "blocked" | "skipped") {
        return Err(ProtocolError::bad_request(format!(
            "unknown step status '{status}'"
        )));
    }
    if matches!(status, "failed" | "blocked") && note.trim().is_empty() {
        return Err(ProtocolError::bad_request(
            "failed/blocked steps require a note",
        ));
    }
    let attachment_owner = Some(media::AttachmentOwner {
        kind: AttachmentOwnerKind::RunStep,
        id: item_id.into(),
        step_index: Some(step_index),
    });
    let attachments_json =
        normalize_attachments(ctx, &record, attachment_owner.as_ref(), attachments_json)?;
    let pool = open_project_pool(project_id)?;
    // Step verdicts are strictly the executing tester's (no manager override —
    // a manager reassigns instead of forging results).
    let item = runs::get_run_item(&pool, item_id)
        .map_err(|e| db_error("item_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run item not found"))?;
    if item.assigned_to != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "this run item belongs to another tester",
        ));
    }
    if item.status != "in_progress" {
        return Err(ProtocolError::bad_request("item is not in progress"));
    }
    let ok = runs::set_step(
        &pool,
        item_id,
        step_index,
        status,
        note.trim(),
        &attachments_json,
    )
    .map_err(|e| db_error("step_set", e))?;
    if !ok {
        return Err(ProtocolError::not_found("step not found"));
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "run_step.set",
        "run_item",
        item_id,
        &serde_json::json!({ "step_index": step_index, "status": status }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::RunStepSetResult { ok }))
}

#[allow(clippy::too_many_arguments)]
fn run_item_finish_v1(
    ctx: &HandlerContext,
    project_id: &str,
    item_id: &str,
    status: &str,
    result_note: &str,
    tester_config: &str,
    duration_secs: u32,
    attachments_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let attachment_owner = Some(media::AttachmentOwner {
        kind: AttachmentOwnerKind::RunItem,
        id: item_id.into(),
        step_index: None,
    });
    let attachments_json =
        normalize_attachments(ctx, &record, attachment_owner.as_ref(), attachments_json)?;
    let pool = open_project_pool(project_id)?;
    let item = runs::get_run_item(&pool, item_id)
        .map_err(|e| db_error("item_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run item not found"))?;
    if item.assigned_to != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "this run item belongs to another tester",
        ));
    }
    if item.status != "in_progress" {
        return Err(ProtocolError::bad_request("item is not in progress"));
    }
    let final_status = if status.is_empty() {
        runs::derive_item_status(&pool, item_id).map_err(|e| db_error("derive_status", e))?
    } else {
        if !matches!(status, "passed" | "failed" | "blocked" | "skipped") {
            return Err(ProtocolError::bad_request(format!(
                "unknown item status '{status}'"
            )));
        }
        // An explicit override of the derived verdict must be justified.
        if result_note.trim().is_empty() {
            return Err(ProtocolError::bad_request(
                "an explicit status override requires result_note",
            ));
        }
        status.to_string()
    };
    let ok = runs::finish_item(
        &pool,
        item_id,
        &final_status,
        result_note.trim(),
        tester_config.trim(),
        duration_secs,
        &attachments_json,
    )
    .map_err(|e| db_error("item_finish", e))?;
    if !ok {
        return Err(ProtocolError::bad_request("item is not in progress"));
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "run_item.finished",
        "run_item",
        item_id,
        &serde_json::json!({ "status": final_status, "duration_secs": duration_secs }).to_string(),
    );
    let next_item = runs::next_claimable(&pool, &item.run_id, &org.user_id)
        .map_err(|e| db_error("next_claimable", e))?
        .map(|next| {
            let names = repository::resolve_user_refs(std::slice::from_ref(&next.assigned_to));
            item_to_wire(next, &names)
        });
    Ok(ps(ProjectStudioPayload::RunItemFinishResponse {
        ok,
        next_item,
    }))
}

fn my_test_work_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let memberships = repository::member_accesses_for_user(&org.user_id)
        .map_err(|e| db_error("member_accesses", e))?;
    let mut entries: Vec<MyWorkEntry> = Vec::new();
    for (project_id, _member) in memberships {
        let Some(record) = repository::get_project(&org.org_id, &project_id)
            .map_err(|e| db_error("get_project", e))?
        else {
            continue;
        };
        let access = repository::project_access(&record, &org.user_id, false)
            .map_err(|e| db_error("project_access", e))?;
        if !access.allows(ProjectArea::Tests, ProjectPermissionLevel::Write) {
            continue;
        }
        let Ok(pool) = project_db::open(&project_id) else {
            continue;
        };
        // One broken project database must not blank the whole cross-project list.
        let rows = match runs::my_work_rows(&pool, &org.user_id) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e, "my_test_work: skipping project");
                continue;
            }
        };
        for (run, items_pending, items_in_progress) in rows {
            entries.push(MyWorkEntry {
                project_id: project_id.clone(),
                project_name: record.name.clone(),
                run_id: run.run_id,
                run_no: run.run_no,
                run_name: run.name,
                items_pending,
                items_in_progress,
            });
        }
    }
    Ok(ps(ProjectStudioPayload::MyTestWorkResponse { entries }))
}

// =============================================================================
// F2: tasks + defects (Z01, Z02)
// =============================================================================

#[allow(clippy::too_many_arguments)]
fn tasks_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_type: &str,
    status: &str,
    assigned_to: &str,
    search: &str,
    offset: u32,
    limit: u32,
    severity: &str,
    include_archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) =
        require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    // "me" is a UI-level alias, not a stored user id — resolve it to the caller.
    let assigned_to = if assigned_to == "me" {
        org.user_id.as_str()
    } else {
        assigned_to
    };
    let filters = tasks::TaskFilters {
        task_type,
        status,
        assigned_to,
        search,
        severity,
        include_archived,
    };
    let (rows, total) =
        tasks::list_tasks(&pool, &filters, offset, limit).map_err(|e| db_error("tasks_list", e))?;
    let mut ids: Vec<String> = rows.iter().map(|t| t.created_by.clone()).collect();
    ids.extend(rows.iter().map(|t| t.assigned_to.clone()));
    let names = repository::resolve_user_refs(&ids);
    let tasks_wire = rows
        .into_iter()
        .map(|record| task_to_wire(record, &names))
        .collect();
    Ok(ps(ProjectStudioPayload::TasksListResponse {
        tasks: tasks_wire,
        total,
    }))
}

fn task_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) =
        require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?;
    let pool = open_project_pool(project_id)?;
    let record = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let (events, events_has_more) = tasks::list_task_events(&pool, task_id, None, 50)
        .map_err(|e| db_error("task_events", e))?;
    let task_links =
        tasks::list_task_links(&pool, task_id).map_err(|e| db_error("task_links", e))?;
    let status_durations =
        tasks::task_status_durations(&pool, task_id).map_err(|e| db_error("task_durations", e))?;
    let comments = tasks::list_comments(&pool, task_id).map_err(|e| db_error("comments", e))?;
    let mut ids: Vec<String> = comments.iter().map(|c| c.author_user_id.clone()).collect();
    ids.push(record.created_by.clone());
    ids.push(record.assigned_to.clone());
    let names = repository::resolve_user_refs(&ids);
    let description_md = record.description_md.clone();
    let attachments = attachments_from_json(&record.attachments_json);
    Ok(ps(ProjectStudioPayload::TaskGetResponse {
        detail: TaskDetail {
            handover_comment_id: tasks::latest_handover_comment_id(&pool, task_id)
                .map_err(|e| db_error("handover_comment", e))?,
            task_links: task_links.into_iter().map(task_link_to_wire).collect(),
            events: events.into_iter().map(task_event_to_wire).collect(),
            events_has_more,
            status_durations: status_durations
                .into_iter()
                .map(|entry| TaskStatusDurationWire {
                    status: entry.status,
                    entered_at: entry.entered_at,
                    left_at: entry.left_at,
                    seconds: entry.seconds,
                })
                .collect(),
            info: task_to_wire(record, &names),
            description_md,
            attachments,
            comments: comments
                .into_iter()
                .map(|c| comment_to_wire(c, &names))
                .collect(),
        },
    }))
}

fn task_mutation_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    if error.downcast_ref::<rusqlite::Error>().is_some()
        || error.downcast_ref::<crate::db::DbError>().is_some()
    {
        db_error(scope, error)
    } else {
        ProtocolError::bad_request(error.to_string())
    }
}

fn task_type_to_wire(record: crate::project_studio::models::TaskTypeRecord) -> TaskTypeWire {
    TaskTypeWire {
        type_id: record.type_id,
        name: record.name,
        description: record.description,
        sort_order: record.sort_order,
        built_in: record.built_in,
        active: record.active,
    }
}

fn task_event_to_wire(record: crate::project_studio::models::TaskEventRecord) -> TaskEventWire {
    TaskEventWire {
        event_id: record.event_id,
        task_id: record.task_id,
        at: record.at,
        actor_kind: record.actor_kind,
        actor_id: record.actor_id,
        kind: record.kind,
        before_json: record.before_json,
        after_json: record.after_json,
    }
}

fn task_link_to_wire(record: crate::project_studio::models::TaskLinkRecord) -> TaskLinkWire {
    TaskLinkWire {
        link_id: record.link_id,
        source_task_id: record.source_task_id,
        target_task_id: record.target_task_id,
        kind: record.kind,
        lag_days: record.lag_days,
        counterparty_task_key: record.other_task_key,
        counterparty_task_title: record.other_task_title,
    }
}

#[allow(clippy::too_many_arguments)]
fn task_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: Option<&str>,
    task_type: &str,
    title: &str,
    description_md: &str,
    severity: &str,
    priority: &str,
    status: &str,
    assigned_to: &str,
    due_date: &str,
    parent_task_id: Option<&str>,
    links_json: &str,
    attachments_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = if task_id.is_some() {
        require_project(
            ctx,
            org,
            project_id,
            ProjectArea::Tasks,
            ProjectPermissionLevel::Write,
        )?
    } else {
        let result = require_project_access(ctx, org, project_id)?;
        if !result.1.can_create_tasks {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "task creation is disabled",
            ));
        }
        result
    };
    require_active(&record)?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ProtocolError::bad_request(
            "title must be 1..200 characters",
        ));
    }
    if !tasks::TASK_PRIORITIES.contains(&priority) {
        return Err(ProtocolError::bad_request("unknown task priority"));
    }
    if !tasks::TASK_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request("unknown task status"));
    }
    let severity = severity.trim();
    if task_type == "defect" {
        if !tasks::TASK_SEVERITIES.contains(&severity) {
            return Err(ProtocolError::bad_request(
                "a defect requires severity (low|medium|high|critical)",
            ));
        }
    } else if !severity.is_empty() {
        return Err(ProtocolError::bad_request(
            "severity applies only to defects",
        ));
    }
    if !assigned_to.is_empty()
        && !repository::project_access(&record, assigned_to, false)
            .map_err(|e| db_error("member_access", e))?
            .allows(ProjectArea::Tasks, ProjectPermissionLevel::Write)
    {
        return Err(ProtocolError::bad_request(
            "assigned_to must have task write access",
        ));
    }
    let links_json = normalize_links(links_json)?;
    let attachment_owner = task_id.map(|id| media::AttachmentOwner {
        kind: AttachmentOwnerKind::Task,
        id: id.into(),
        step_index: None,
    });
    let attachments_json =
        normalize_attachments(ctx, &record, attachment_owner.as_ref(), attachments_json)?;
    let input = tasks::TaskInput {
        task_type,
        title,
        description_md,
        severity,
        priority,
        status,
        assigned_to,
        due_date: due_date.trim(),
        parent_task_id,
        links_json: &links_json,
        attachments_json: &attachments_json,
    };
    let pool = open_project_pool(project_id)?;
    let mutation = match task_id {
        None => tasks::create_task(&pool, &input, &org.user_id)
            .map_err(|e| task_mutation_error("task_create", e))?,
        Some(id) => tasks::update_task(&pool, id, &input, &org.user_id)
            .map_err(|e| task_mutation_error("task_update", e))?
            .ok_or_else(|| ProtocolError::not_found("task not found"))?,
    };
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            if task_id.is_none() {
                "task.created"
            } else {
                "task.updated"
            },
            "task",
            &mutation.task_id,
            &serde_json::json!({"task_key":mutation.task_key,"event_ids":mutation.event_ids})
                .to_string(),
        );
        notifications::notify_task_changes(ctx, project_id, &mutation);
    }
    Ok(ps(ProjectStudioPayload::TaskSaveResponse {
        task_id: mutation.task_id,
        task_no: mutation.task_no,
        task_key: mutation.task_key,
        event_ids: mutation.event_ids,
    }))
}

fn task_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let task = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    if !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Admin)
        && (task.created_by != org.user_id || task.comment_count != 0)
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "deleting requires task administration or authorship of an uncommented task",
        ));
    }
    let mutation = tasks::delete_task(&pool, task_id, &org.user_id)
        .map_err(|e| task_mutation_error("task_delete", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            if mutation.archived {
                "task.archived"
            } else {
                "task.deleted"
            },
            "task",
            task_id,
            &serde_json::json!({"task_key":task.task_key,"event_ids":mutation.event_ids})
                .to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskDeleteResult {
        ok: true,
        archived: mutation.archived,
    }))
}

fn task_archive(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let mutation = tasks::set_task_archived(&pool, task_id, archived, &org.user_id)
        .map_err(|e| task_mutation_error("task_archive", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            if archived {
                "task.archived"
            } else {
                "task.restored"
            },
            "task",
            task_id,
            &serde_json::json!({"event_ids":mutation.event_ids}).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskArchiveResult {
        ok: true,
        event_id: mutation.event_ids.first().copied(),
    }))
}

fn validate_mentions(
    ctx: &HandlerContext,
    project_id: &str,
    ids: &[String],
) -> Result<Vec<String>, ProtocolError> {
    let org = require_read(ctx)?;
    let mut users: Vec<String> = ids
        .iter()
        .filter(|id| *id != &org.user_id)
        .cloned()
        .collect();
    users.sort();
    users.dedup();
    for user in &users {
        if !notifications::task_reader(ctx, project_id, user) {
            return Err(ProtocolError::bad_request(
                "mentioned user does not have current task read access",
            ));
        }
    }
    Ok(users)
}

fn comment_body(body: &str) -> Result<&str, ProtocolError> {
    let body = body.trim();
    if body.is_empty() || body.chars().count() > 8000 {
        return Err(ProtocolError::bad_request(
            "comment must be 1..8000 characters",
        ));
    }
    Ok(body)
}

fn task_comment_add_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    body_md: &str,
    mentions: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let body = comment_body(body_md)?;
    let mentions = validate_mentions(ctx, project_id, mentions)?;
    let pool = open_project_pool(project_id)?;
    tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let mutation = tasks::add_comment(&pool, task_id, &org.user_id, body, &mentions)
        .map_err(|e| task_mutation_error("comment_add", e))?;
    notifications::notify_mentions(ctx, project_id, task_id, &mutation);
    let comment = mutation
        .comment
        .ok_or_else(|| ProtocolError::internal("comment mutation returned no comment"))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "task_comment.added",
        "task",
        task_id,
        &serde_json::json!({"event_id":mutation.event_id}).to_string(),
    );
    let names = repository::resolve_user_refs(std::slice::from_ref(&comment.author_user_id));
    Ok(ps(ProjectStudioPayload::TaskCommentAddResponse {
        comment: comment_to_wire(comment, &names),
    }))
}

fn task_comment_edit_v1(
    ctx: &HandlerContext,
    project_id: &str,
    comment_id: &str,
    body_md: &str,
    mentions: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let body = comment_body(body_md)?;
    let mentions = validate_mentions(ctx, project_id, mentions)?;
    let pool = open_project_pool(project_id)?;
    let mutation = tasks::edit_comment(&pool, comment_id, &org.user_id, body, &mentions)
        .map_err(|e| task_mutation_error("comment_edit", e))?
        .ok_or_else(|| ProtocolError::not_found("comment not found"))?;
    if mutation.changed {
        let task_id = &mutation
            .comment
            .as_ref()
            .ok_or_else(|| ProtocolError::internal("comment mutation returned no comment"))?
            .task_id;
        notifications::notify_mentions(ctx, project_id, task_id, &mutation);
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task_comment.edited",
            "task",
            task_id,
            &serde_json::json!({"event_id":mutation.event_id}).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskCommentEditResult { ok: true }))
}

fn task_comment_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    comment_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let comment = tasks::get_comment(&pool, comment_id)
        .map_err(|e| db_error("comment_get", e))?
        .ok_or_else(|| ProtocolError::not_found("comment not found"))?;
    if comment.author_user_id != org.user_id
        && !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Admin)
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the author or a task administrator may delete a comment",
        ));
    }
    let mutation = tasks::delete_comment(&pool, comment_id, &org.user_id)
        .map_err(|e| task_mutation_error("comment_delete", e))?
        .ok_or_else(|| ProtocolError::not_found("comment not found"))?;
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task_comment.deleted",
            "task",
            &comment.task_id,
            &serde_json::json!({"event_id":mutation.event_id}).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskCommentDeleteResult {
        ok: true,
    }))
}

fn task_handover(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    assigned_to: &str,
    note_md: &str,
    mention_user_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let task = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    if task.assigned_to != org.user_id
        && !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Write)
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "handing over requires task write access or current self-assignment",
        ));
    }
    if assigned_to.is_empty()
        || !repository::project_access(&record, assigned_to, false)
            .map_err(|e| db_error("handover_recipient", e))?
            .allows(ProjectArea::Tasks, ProjectPermissionLevel::Write)
    {
        return Err(ProtocolError::bad_request(
            "handover recipient must have current task write access",
        ));
    }
    let note = comment_body(note_md)?;
    let mentions = validate_mentions(ctx, project_id, mention_user_ids)?;
    let mutation = tasks::reassign_open(
        &pool,
        task_id,
        &task.assigned_to,
        assigned_to,
        &tasks::TaskHandoverInput {
            actor: &org.user_id,
            note_md: note,
            mention_user_ids: &mentions,
            direction: tasks::TaskHandoverDirection::Over,
            handover_id: None,
        },
    )
    .map_err(|e| task_mutation_error("task_handover", e))?
    .ok_or_else(|| {
        ProtocolError::bad_request("task is closed, archived or its assignee changed")
    })?;
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task.handed_over",
            "task",
            task_id,
            &serde_json::json!({"event_ids":mutation.event_ids}).to_string(),
        );
        notifications::notify_task_handover(
            &org.org_id,
            &org.user_id,
            project_id,
            &mutation,
            tasks::TaskHandoverDirection::Over,
            &|user| notifications::task_reader(ctx, project_id, user),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskHandoverResult {
        ok: true,
        event_ids: mutation.event_ids,
        comment_id: mutation.handover_comment_id,
    }))
}

fn task_types_list(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_, access) = require_project_access(ctx, org, project_id)?;
    if !access
        .enabled_modules
        .iter()
        .any(|module| module == "tasks")
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "task catalogue is disabled",
        ));
    }
    let types = tasks::list_task_types(&open_project_pool(project_id)?)
        .map_err(|e| db_error("task_types", e))?;
    Ok(ps(ProjectStudioPayload::TaskTypesListResponse {
        types: types.into_iter().map(task_type_to_wire).collect(),
    }))
}

fn task_type_save(
    ctx: &HandlerContext,
    project_id: &str,
    type_id: &str,
    name: &str,
    description: &str,
    sort_order: i32,
    active: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    let task_type = tasks::save_task_type(
        &open_project_pool(project_id)?,
        type_id,
        name,
        description,
        sort_order,
        active,
    )
    .map_err(|e| task_mutation_error("task_type_save", e))?;
    Ok(ps(ProjectStudioPayload::TaskTypeSaveResponse {
        task_type: task_type_to_wire(task_type),
    }))
}

fn task_events(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    before_id: Option<i64>,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?;
    let pool = open_project_pool(project_id)?;
    tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let (events, has_more) =
        tasks::list_task_events(&pool, task_id, before_id, limit.clamp(1, 100))
            .map_err(|e| db_error("task_events", e))?;
    Ok(ps(ProjectStudioPayload::TaskEventsResponse {
        events: events.into_iter().map(task_event_to_wire).collect(),
        has_more,
    }))
}

fn task_links_list(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_task_access(ctx, org, project_id, ProjectPermissionLevel::Read)?;
    let pool = open_project_pool(project_id)?;
    tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let links = tasks::list_task_links(&pool, task_id).map_err(|e| db_error("task_links", e))?;
    Ok(ps(ProjectStudioPayload::TaskLinksListResponse {
        links: links.into_iter().map(task_link_to_wire).collect(),
    }))
}

fn task_link_save(
    ctx: &HandlerContext,
    project_id: &str,
    source: &str,
    target: &str,
    kind: &str,
    lag: i32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let mutation = tasks::add_task_link(
        &open_project_pool(project_id)?,
        source,
        target,
        kind,
        lag,
        &org.user_id,
    )
    .map_err(|e| task_mutation_error("task_link_save", e))?;
    Ok(ps(ProjectStudioPayload::TaskLinkSaveResult {
        link: task_link_to_wire(mutation.link),
        source_event_id: mutation.source_event_id,
        target_event_id: mutation.target_event_id,
    }))
}

fn task_link_delete(
    ctx: &HandlerContext,
    project_id: &str,
    link_id: i64,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tasks,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let mutation = tasks::delete_task_link(&open_project_pool(project_id)?, link_id, &org.user_id)
        .map_err(|e| task_mutation_error("task_link_delete", e))?;
    Ok(ps(ProjectStudioPayload::TaskLinkDeleteResult {
        ok: mutation.is_some(),
        source_event_id: mutation.as_ref().map(|m| m.source_event_id),
        target_event_id: mutation.map(|m| m.target_event_id),
    }))
}

// =============================================================================
// F2: agent case generation (G01/T05)
// =============================================================================

async fn generation_start_v1(
    ctx: &HandlerContext,
    project_id: &str,
    kind: &str,
    source_ids: &[String],
    requested_count: u32,
    instructions: &str,
    agent_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
    if !generation::GENERATION_KINDS.contains(&kind) {
        return Err(ProtocolError::bad_request(format!(
            "unknown generation kind '{kind}'"
        )));
    }
    if source_ids.is_empty() {
        return Err(ProtocolError::bad_request("select at least one source"));
    }
    if instructions.chars().count() > generation::MAX_INSTRUCTIONS_CHARS {
        return Err(ProtocolError::bad_request(
            "instructions exceed 4000 characters",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let mut source_meta: Vec<(String, String, String)> = Vec::with_capacity(source_ids.len());
    for source_id in source_ids {
        let source = repository::get_source(&pool, source_id)
            .map_err(|e| db_error("get_source", e))?
            .ok_or_else(|| ProtocolError::bad_request(format!("unknown source '{source_id}'")))?;
        if source.status != "ready" {
            return Err(ProtocolError::bad_request(format!(
                "source '{}' is not ready (status '{}')",
                source.name, source.status
            )));
        }
        source_meta.push((source.source_id, source.name, source.kind));
    }
    let requested = if requested_count == 0 {
        generation::DEFAULT_REQUESTED_COUNT
    } else {
        requested_count
    };
    let max_cases = requested.clamp(1, generation::MAX_CASES_CAP);

    // Agent resolution: explicit request > the project's binding for the
    // kind's function > the seeded per-kind system agent.
    let function = generation::agent_function_for_kind(kind)
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown generation kind '{kind}'")))?;
    let resolved_agent_id = match agent_id.filter(|a| !a.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None => {
            let bound: Option<String> = repository::get_setting(&pool, "agents")
                .map_err(|e| db_error("settings_get", e))?
                .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
                .and_then(|map| map.get(function).cloned())
                .filter(|a| !a.is_empty());
            bound.unwrap_or_else(|| generation::default_agent_id_for_kind(kind).to_string())
        }
    };
    let agent = crate::db::repository::get_agent(&ctx.state.db, &resolved_agent_id)
        .map_err(|e| db_error("get_agent", e))?
        .ok_or_else(|| ProtocolError::bad_request("generator agent not found"))?;
    if !agent.is_enabled {
        return Err(ProtocolError::bad_request("generator agent is disabled"));
    }
    if !crate::agents::tool_in_allowlist(
        &agent.tools_json,
        crate::agents::CoreToolName::CaseSave.public_name(),
        None,
    ) {
        return Err(ProtocolError::bad_request(
            "the selected agent has no core.project_case_save in its tool allowlist",
        ));
    }
    let manager = crate::agents::agent_run_manager_global()
        .ok_or_else(|| ProtocolError::internal("agent run manager not initialized"))?;

    let gen_id = uuid::Uuid::new_v4().to_string();
    generation::insert_generation(
        &pool,
        &gen_id,
        kind,
        &agent.id,
        source_ids,
        instructions.trim(),
        requested,
        max_cases,
        &org.user_id,
    )
    .map_err(|e| db_error("generation_insert", e))?;

    // Unit generation gets the detected build recipe of the selected code
    // source, so the agent proposes a `build_profile_ref` that actually exists.
    let build_profile_hint = if kind == "unit" {
        source_ids.iter().find_map(|source_id| {
            let paths = repository::files_for_ingest(&pool, source_id, None)
                .ok()?
                .into_iter()
                .map(|f| f.path)
                .collect::<Vec<_>>();
            let proposal = build_profiles::detect_toolchain(&paths)?;
            Some(format!(
                "source_id={source_id} toolchain={} install_cmd={} test_cmd={} workdir={}",
                proposal.toolchain,
                proposal.install_cmd,
                proposal.test_cmd,
                if proposal.workdir.is_empty() {
                    "."
                } else {
                    &proposal.workdir
                }
            ))
        })
    } else {
        None
    };
    let prompt = generation::build_generation_prompt(&generation::GenerationPromptInput {
        project_name: &record.name,
        sources: &source_meta,
        instructions,
        max_cases,
        kind,
        build_profile_hint,
    });
    let principal = crate::agents::AgentPrincipal::new(
        Some(org.user_id.clone()),
        Some(org.org_id.clone()),
        crate::flow_engine::dispatcher::FlowOrigin::Project,
        crate::flow_engine::dispatcher::FlowActor::user(org.user_id.clone()),
    );
    let binding_meta = serde_json::json!({ "project_id": project_id, "gen_id": gen_id });
    let spawned = manager
        .spawn(
            &agent.id,
            &prompt,
            None,
            &principal,
            &[],
            &[(generation::GENERATION_META_KEY, binding_meta)],
            None,
            None,
        )
        .await;
    let agent_run_id = match spawned {
        Ok(run_id) => run_id,
        Err(e) => {
            // The row must not stay 'running' forever when nothing runs.
            let _ = finalize_failed_start(&pool, &gen_id);
            return Err(db_error("generation_spawn", e));
        }
    };
    generation::set_agent_run_id(&pool, &gen_id, &agent_run_id)
        .map_err(|e| db_error("generation_run_id", e))?;
    generation::spawn_watcher(
        ctx.state.db.clone(),
        org.org_id.clone(),
        project_id.to_string(),
        gen_id.clone(),
        agent_run_id.clone(),
    );
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "generation.started",
        "generation",
        &gen_id,
        &serde_json::json!({ "agent_id": agent.id, "max_cases": max_cases }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::GenerationStartResponse {
        gen_id,
        agent_run_id,
    }))
}

/// Marks a generation failed when the agent spawn itself failed (no watcher
/// exists yet for it).
fn finalize_failed_start(pool: &crate::db::DbPool, gen_id: &str) -> Result<(), ProtocolError> {
    let conn = pool
        .write()
        .map_err(|e| ProtocolError::internal(format!("project db write: {e}")))?;
    conn.execute(
        "UPDATE generation_runs SET status = 'failed', error = 'agent spawn failed', \
            finished_at = datetime('now') WHERE gen_id = ?1 AND status = 'running'",
        rusqlite::params![gen_id],
    )
    .map_err(|e| ProtocolError::internal(format!("generation finalize: {e}")))?;
    Ok(())
}

fn generations_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    generation::reconcile_running(&ctx.state.db, &pool, &org.org_id, project_id);
    let rows = generation::list_generations(&pool).map_err(|e| db_error("generations_list", e))?;
    let ids: Vec<String> = rows.iter().map(|g| g.started_by.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let generations = rows
        .into_iter()
        .map(|record| generation_to_wire(record, &names))
        .collect();
    Ok(ps(ProjectStudioPayload::GenerationsListResponse {
        generations,
    }))
}

fn generation_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    gen_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    generation::reconcile_running(&ctx.state.db, &pool, &org.org_id, project_id);
    let record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.started_by));
    let pending =
        generation::pending_cases(&pool, gen_id).map_err(|e| db_error("pending_cases", e))?;
    Ok(ps(ProjectStudioPayload::GenerationGetResponse {
        run: generation_to_wire(record, &names),
        pending_cases: cases_to_wire(pending),
    }))
}

fn generation_cancel_v1(
    ctx: &HandlerContext,
    project_id: &str,
    gen_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let generation_record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    let is_manager = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Admin);
    if !is_manager && generation_record.started_by != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "cancelling requires test administration or generation ownership",
        ));
    }
    if generation_record.status != "running" {
        return Err(ProtocolError::bad_request("generation is not running"));
    }
    // Cancel through the run manager (D.5) — the watcher observes the
    // terminal state and finalizes. When nothing is live (restart), lazy
    // reconcile settles the row from the persisted run status.
    let signalled = crate::agents::agent_run_manager_global()
        .map(|m| m.cancel(&generation_record.agent_run_id))
        .unwrap_or(false);
    if !signalled {
        generation::reconcile_running(&ctx.state.db, &pool, &org.org_id, project_id);
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "generation.cancelled",
        "generation",
        gen_id,
        "{}",
    );
    Ok(ps(ProjectStudioPayload::GenerationCancelResult {
        ok: true,
    }))
}

fn generation_review_v1(
    ctx: &HandlerContext,
    project_id: &str,
    gen_id: &str,
    accept_case_ids: &[String],
    reject_case_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    if accept_case_ids.is_empty() && reject_case_ids.is_empty() {
        return Err(ProtocolError::bad_request("nothing to review"));
    }
    let pool = open_project_pool(project_id)?;
    let generation_record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    if generation_record.status != "review" {
        return Err(ProtocolError::bad_request(
            "generation is not awaiting review",
        ));
    }
    let (accepted, rejected, run_status) =
        generation::review_generation(&pool, gen_id, accept_case_ids, reject_case_ids)
            .map_err(|e| db_error("generation_review", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "generation.reviewed",
        "generation",
        gen_id,
        &serde_json::json!({ "accepted": accepted, "rejected": rejected, "status": run_status })
            .to_string(),
    );
    Ok(ps(ProjectStudioPayload::GenerationReviewResponse {
        accepted,
        rejected,
        run_status,
    }))
}

fn generation_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    gen_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let generation_record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    if matches!(generation_record.status.as_str(), "running" | "review") {
        return Err(ProtocolError::bad_request(
            "only finished generations can be deleted",
        ));
    }
    let ok = generation::delete_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "generation.deleted",
            "generation",
            gen_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::GenerationDeleteResult { ok }))
}

// =============================================================================
// F2: notifications (G02) — central DB, always caller-scoped
// =============================================================================

fn notification_area(kind: &str) -> ProjectArea {
    if kind.starts_with("task_") {
        ProjectArea::Tasks
    } else if kind.starts_with("environment_") || kind.starts_with("env_") {
        ProjectArea::Environments
    } else if kind.starts_with("run_")
        || kind.starts_with("case_")
        || kind.starts_with("generation_")
        || kind.starts_with("schedule_")
    {
        ProjectArea::Tests
    } else {
        ProjectArea::Settings
    }
}

pub(crate) fn notification_visible(access: &ProjectAccessWire, kind: &str) -> bool {
    let area = notification_area(kind);
    if area == ProjectArea::Tasks {
        task_access(access, ProjectPermissionLevel::Read)
    } else {
        access.allows(area, ProjectPermissionLevel::Read)
    }
}

fn notifications_list_v1(
    ctx: &HandlerContext,
    only_unread: bool,
    before_id: Option<&str>,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let limit = limit.clamp(1, 100);
    let (rows, _unread_count, has_more) =
        notifications::list(&org.user_id, only_unread, before_id, limit)
            .map_err(|e| db_error("notifications_list", e))?;
    let mut access_by_project = HashMap::new();
    let mut visible = Vec::new();
    for row in rows {
        if !access_by_project.contains_key(&row.project_id) {
            let access = repository::get_project(&org.org_id, &row.project_id)
                .map_err(|e| db_error("notification_project", e))?
                .map(|record| repository::project_access(&record, &org.user_id, is_admin(ctx)))
                .transpose()
                .map_err(|e| db_error("notification_access", e))?;
            access_by_project.insert(row.project_id.clone(), access);
        }
        if access_by_project
            .get(&row.project_id)
            .and_then(Option::as_ref)
            .is_some_and(|access| notification_visible(access, &row.kind))
        {
            visible.push(row);
        }
    }
    let unread_groups: Vec<(String, String, u32)> = {
        let registry =
            crate::project_studio::db::pool().map_err(|e| db_error("notification_registry", e))?;
        let conn = registry
            .read()
            .map_err(|e| db_error("notification_unread", e))?;
        let mut stmt = conn.prepare("SELECT project_id, kind, COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL GROUP BY project_id, kind")
            .map_err(|e| db_error("notification_unread", e))?;
        let rows = stmt
            .query_map([&org.user_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| db_error("notification_unread", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("notification_unread", e))?
    };
    let mut unread_count = 0u32;
    for (project_id, kind, count) in unread_groups {
        if !access_by_project.contains_key(&project_id) {
            let access = repository::get_project(&org.org_id, &project_id)
                .map_err(|e| db_error("notification_project", e))?
                .map(|record| repository::project_access(&record, &org.user_id, is_admin(ctx)))
                .transpose()
                .map_err(|e| db_error("notification_access", e))?;
            access_by_project.insert(project_id.clone(), access);
        }
        if access_by_project
            .get(&project_id)
            .and_then(Option::as_ref)
            .is_some_and(|access| notification_visible(access, &kind))
        {
            unread_count = unread_count.saturating_add(count);
        }
    }
    let notifications_wire = visible
        .into_iter()
        .map(|n| NotificationWire {
            notification_id: n.notification_id,
            project_id: n.project_id,
            project_name: n.project_name,
            kind: n.kind,
            title: n.title,
            body: n.body,
            link_json: n.link_json,
            read_at: n.read_at,
            created_at: n.created_at,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::NotificationsListResponse {
        notifications: notifications_wire,
        unread_count,
        has_more,
    }))
}

fn notifications_mark_read_v1(
    ctx: &HandlerContext,
    notification_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    if notification_ids.len() > 500 {
        return Err(ProtocolError::bad_request("too many notification ids"));
    }
    notifications::mark_read(&org.user_id, notification_ids)
        .map_err(|e| db_error("notifications_mark_read", e))?;
    Ok(ps(ProjectStudioPayload::NotificationsMarkReadResult {
        ok: true,
    }))
}

// =============================================================================
// F2: reports (T14)
// =============================================================================

fn report_query_v1(
    ctx: &HandlerContext,
    project_id: &str,
    report: &str,
    from_date: &str,
    to_date: &str,
    suite_id: &str,
    run_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    if !reports::REPORT_KINDS.contains(&report) {
        return Err(ProtocolError::bad_request(format!(
            "unknown report '{report}'"
        )));
    }
    let pool = open_project_pool(project_id)?;
    let mut rows_json = reports::run_report(&pool, report, from_date, to_date, suite_id, run_ids)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // These rows carry raw user ids — enrich with display names here (the core
    // user directory is not reachable from the per-project SQL).
    if report == "tester_stats" || report == "tester_activity" {
        if let Ok(mut rows) = serde_json::from_str::<Vec<serde_json::Value>>(&rows_json) {
            let ids: Vec<String> = rows
                .iter()
                .filter_map(|r| r.get("user_id").and_then(|u| u.as_str()))
                .map(|s| s.to_string())
                .collect();
            let names = repository::resolve_user_refs(&ids);
            for row in &mut rows {
                let user_id = row
                    .get("user_id")
                    .and_then(|u| u.as_str())
                    .unwrap_or_default()
                    .to_string();
                row["display_name"] = serde_json::Value::String(display_name(&names, &user_id));
            }
            // Display names are added AFTER the report clamped itself, so the
            // bound is re-applied to the enriched payload.
            rows_json = reports::bounded_rows_json(rows);
        }
    }
    Ok(ps(ProjectStudioPayload::ReportQueryResponse { rows_json }))
}

// =============================================================================
// F3: code + spec sources (git / zip / api_spec)
// =============================================================================

/// Spawns an ingest job for an explicit set of already-recorded file rows
/// (the delta of a code-source refresh). `spawn_ingest_job` only knows "all
/// files" or "one file", which a delta re-index is neither.
fn spawn_tree_ingest_job(
    ctx: &HandlerContext,
    org: &OrgContext,
    record: &ProjectRecord,
    pool: &crate::db::DbPool,
    source_id: &str,
    file_ids: &[String],
) -> Result<String, ProtocolError> {
    let wanted: HashSet<&str> = file_ids.iter().map(|f| f.as_str()).collect();
    let work: Vec<ingest::FileWork> = repository::files_for_ingest(pool, source_id, None)
        .map_err(|e| db_error("files_for_ingest", e))?
        .into_iter()
        .filter(|f| wanted.contains(f.file_id.as_str()))
        .map(|f| ingest::FileWork {
            file_id: f.file_id,
            path: f.path,
            sha256: f.sha256,
            mime: f.mime,
            payload: ingest::WorkPayload::Blob,
        })
        .collect();
    if work.is_empty() {
        return Err(ProtocolError::bad_request("source has no files to ingest"));
    }
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

/// Refuses a git source whose host reaches a private/LAN/loopback address.
/// Cloning is a server-side fetch of an editor-supplied url, so without this an
/// editor could pull `http://10.0.0.5/git/secret.git` — or the cloud metadata
/// address — into the project knowledge base. Unlike a test environment a code
/// source has NO approval queue, so this is a hard refusal; a private
/// repository has to be mirrored somewhere publicly reachable first.
/// `git_source::clone/refresh` enforce the same rule at fetch time — this call
/// exists to turn it into a PolicyDenied with a security-audit entry.
async fn deny_private_repo_url(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    repo_url: &str,
) -> Result<(), ProtocolError> {
    let probe = repo_url.to_string();
    let is_private = tokio::task::spawn_blocking(move || git_source::repo_url_is_private(&probe))
        .await
        .map_err(|_| ProtocolError::internal("repo url classification task panicked"))?
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if !is_private {
        return Ok(());
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.git_source_private_address_denied",
        &format!("project:{project_id}"),
        &serde_json::json!({ "repo_url": repo_url }).to_string(),
    );
    Err(ProtocolError::new(
        ProtocolErrorCode::PolicyDenied,
        "the repository address is private (LAN/loopback) — use a publicly reachable \
         repository or ask an administrator to mirror it",
    ))
}

/// Materialises a code/spec source: clone / extract / parse into the project's
/// blob store, record the file rows and start the ingest job. Everything that
/// can fail happens BEFORE the source row exists, so a bad repo url or a
/// corrupt archive never leaves an orphaned source behind.
async fn code_source_create(
    ctx: &HandlerContext,
    org: &OrgContext,
    record: &ProjectRecord,
    kind: &str,
    name: &str,
    config_json: &str,
    file_refs: &[String],
) -> Result<MessageBody, ProtocolError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ProtocolError::bad_request("source name is required"));
    }
    let project_id = record.project_id.clone();
    let dir_path = std::path::PathBuf::from(&record.dir_path);
    let source_id = uuid::Uuid::new_v4().to_string();
    // Config persisted on the row + the encrypted token that never enters it.
    let mut stored_config = config_json.to_string();
    let mut secret_enc = String::new();
    let mut spec_endpoints = None;

    // Materialise first, then persist. `prepared` is the file list that will
    // become `source_files` rows.
    let prepared: Vec<ingest::CollectedFile> = match kind {
        "git" => {
            let (clean_config, token) = git_source::split_token(config_json)
                .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
            let config = git_source::parse_config(&clean_config)
                .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
            stored_config = clean_config;
            if !token.is_empty() {
                require_project(
                    ctx,
                    org,
                    &project_id,
                    ProjectArea::Settings,
                    ProjectPermissionLevel::Write,
                )?;
                secret_enc = ctx
                    .state
                    .settings_cipher
                    .encrypt(&token)
                    .map_err(|e| db_error("source_secret_encrypt", e))?;
            }
            deny_private_repo_url(ctx, org, &project_id, &config.repo_url).await?;
            require_project(
                ctx,
                org,
                &project_id,
                ProjectArea::Repos,
                ProjectPermissionLevel::Write,
            )?;
            if !secret_enc.is_empty() {
                require_project(
                    ctx,
                    org,
                    &project_id,
                    ProjectArea::Settings,
                    ProjectPermissionLevel::Write,
                )?;
            }
            let project = project_id.clone();
            let source = source_id.clone();
            let dir = dir_path.clone();
            let token_for_msg = token.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ingest::CollectedFile>> {
                let checkout = git_source::clone(&project, &source, &config, &token)?;
                ingest::collect_tree_files(&checkout.tree_root, &dir)
            })
            .await
            .map_err(|_| ProtocolError::internal("git clone task panicked"))?
            .map_err(|e| {
                ProtocolError::bad_request(format!(
                    "git clone failed: {}",
                    git_source::scrub_token(&e.to_string(), &token_for_msg)
                ))
            })?
        }
        "zip" => {
            let sha = file_refs.first().ok_or_else(|| {
                ProtocolError::bad_request("zip source requires one uploaded file_ref")
            })?;
            ingest::finalized_meta(&org.org_id, &org.user_id, &project_id, &dir_path, sha)
                .ok_or_else(|| {
                    ProtocolError::bad_request(format!(
                        "unknown file_ref '{sha}' (upload expired?)"
                    ))
                })?;
            let archive = dir_path.join("files").join(sha);
            let project = project_id.clone();
            let source = source_id.clone();
            let dir = dir_path.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ingest::CollectedFile>> {
                let root = zip_source::extract(&project, &source, &archive)?;
                ingest::collect_tree_files(&root, &dir)
            })
            .await
            .map_err(|_| ProtocolError::internal("zip extract task panicked"))?
            .map_err(|e| ProtocolError::bad_request(format!("archive rejected: {e}")))?
        }
        "api_spec" => {
            let sha = file_refs.first().ok_or_else(|| {
                ProtocolError::bad_request(
                    "api_spec source requires one uploaded OpenAPI/Swagger file_ref",
                )
            })?;
            let meta =
                ingest::finalized_meta(&org.org_id, &org.user_id, &project_id, &dir_path, sha)
                    .ok_or_else(|| {
                        ProtocolError::bad_request(format!(
                            "unknown file_ref '{sha}' (upload expired?)"
                        ))
                    })?;
            let blob = dir_path.join("files").join(sha);
            let dir = dir_path.clone();
            let filename = meta.filename.clone();
            let sha_owned = sha.clone();
            let mime = meta.mime.clone();
            let size = meta.size_bytes;
            let (files, endpoints) = tokio::task::spawn_blocking(
                move || -> anyhow::Result<(Vec<ingest::CollectedFile>, String)> {
                    let bytes = std::fs::read(&blob)?;
                    let spec = api_spec::parse_spec(&bytes)?;
                    let endpoints = serde_json::to_string(&spec.endpoints)?;
                    let digest = ingest::store_generated_text(
                        &dir,
                        "endpoints.md",
                        &api_spec::endpoints_markdown(&spec),
                    )?;
                    Ok((
                        vec![
                            ingest::CollectedFile {
                                rel_path: filename,
                                sha256: sha_owned,
                                size_bytes: size,
                                mime,
                            },
                            digest,
                        ],
                        endpoints,
                    ))
                },
            )
            .await
            .map_err(|_| ProtocolError::internal("api spec parse task panicked"))?
            .map_err(|e| ProtocolError::bad_request(format!("spec rejected: {e}")))?;
            spec_endpoints = Some(endpoints);
            files
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown kind '{other}'"
            )))
        }
    };
    let authorized = require_project(
        ctx,
        org,
        &project_id,
        source_write_area(kind),
        ProjectPermissionLevel::Write,
    )
    .and_then(|project| {
        if !secret_enc.is_empty() {
            require_project(
                ctx,
                org,
                &project_id,
                ProjectArea::Settings,
                ProjectPermissionLevel::Write,
            )?;
        }
        Ok(project)
    });
    let (current_record, _) = match authorized {
        Ok(project) => project,
        Err(error) => {
            git_source::remove_source_dir(&project_id, &source_id);
            return Err(error);
        }
    };
    if prepared.is_empty() {
        git_source::remove_source_dir(&project_id, &source_id);
        return Err(ProtocolError::bad_request(
            "the source contains no indexable text or code files",
        ));
    }

    let pool = open_project_pool(&project_id)?;
    repository::create_source(&pool, &source_id, kind, name, &stored_config, &org.user_id)
        .map_err(|e| db_error("source_create", e))?;
    if let Some(endpoints) = spec_endpoints {
        repository::set_setting(
            &pool,
            &api_spec::endpoints_setting_key(&source_id),
            &endpoints,
        )
        .map_err(|e| db_error("source_endpoints", e))?;
    }
    if !secret_enc.is_empty() {
        repository::set_source_secret(&pool, &source_id, &secret_enc)
            .map_err(|e| db_error("source_secret_set", e))?;
    }
    let delta = ingest::TreeDelta {
        added: prepared.iter().map(|f| f.rel_path.clone()).collect(),
        ..Default::default()
    };
    let (file_ids, _removed) = repository::sync_tree_files(&pool, &source_id, &prepared, &delta)
        .map_err(|e| db_error("sync_tree_files", e))?;
    let job_id = spawn_tree_ingest_job(ctx, org, &current_record, &pool, &source_id, &file_ids)?;

    // A fresh code source gets a proposed build recipe so the unit-test flow
    // starts from a filled form instead of an empty one.
    if matches!(kind, "git" | "zip") {
        let paths: Vec<String> = prepared.iter().map(|f| f.rel_path.clone()).collect();
        if let Some(proposal) = build_profiles::detect_toolchain(&paths) {
            let _ = build_profiles::upsert(
                &pool,
                &source_id,
                proposal.toolchain,
                proposal.base_image,
                &proposal.install_cmd,
                proposal.test_cmd,
                &proposal.workdir,
                "detector",
            );
        }
    }

    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.created",
        "source",
        &source_id,
        &serde_json::json!({ "name": name, "kind": kind, "files": prepared.len() }).to_string(),
    );
    let _ = repository::touch_project(&project_id);
    Ok(ps(ProjectStudioPayload::SourceCreateResponse {
        source_id,
        job_id,
    }))
}

/// Git-only refresh: fetch + fast-forward, then re-embed ONLY the files whose
/// content hash changed (plus the new ones) and drop the vectors of the files
/// that disappeared.
async fn source_refresh_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;
    if source.kind != "git" {
        return Err(ProtocolError::bad_request(
            "only git sources can be refreshed",
        ));
    }
    let config = git_source::parse_config(&source.config_json)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let token = {
        let secret_enc = repository::get_source_secret_enc(&pool, source_id)
            .map_err(|e| db_error("source_secret", e))?;
        if secret_enc.is_empty() {
            String::new()
        } else {
            ctx.state
                .settings_cipher
                .decrypt(&secret_enc)
                .map_err(|e| db_error("source_secret_decrypt", e))?
        }
    };
    let stored = git_source::stored_file_hashes(&pool, source_id)
        .map_err(|e| db_error("stored_hashes", e))?;

    deny_private_repo_url(ctx, org, project_id, &config.repo_url).await?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let project = project_id.to_string();
    let source_owned = source_id.to_string();
    let dir = std::path::PathBuf::from(&record.dir_path);
    let token_for_msg = token.clone();
    let collected =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ingest::CollectedFile>> {
            let checkout = git_source::refresh(&project, &source_owned, &config, &token)?;
            ingest::collect_tree_files(&checkout.tree_root, &dir)
        })
        .await
        .map_err(|_| ProtocolError::internal("git refresh task panicked"))?
        .map_err(|e| {
            ProtocolError::bad_request(format!(
                "git refresh failed: {}",
                git_source::scrub_token(&e.to_string(), &token_for_msg)
            ))
        })?;

    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let delta = ingest::diff_tree(&stored, &collected);
    let (file_ids, removed_ids) = repository::sync_tree_files(&pool, source_id, &collected, &delta)
        .map_err(|e| db_error("sync_tree_files", e))?;
    if !removed_ids.is_empty() {
        let core_db = ctx.state.db.clone();
        let org_id = org.org_id.clone();
        let project_owned = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            for file_id in &removed_ids {
                ingest::delete_file_vectors(&core_db, &org_id, &project_owned, file_id)?;
                ingest::delete_file_graph(&core_db, &org_id, &project_owned, file_id)?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| ProtocolError::internal("knowledge cleanup task panicked"))?
        .map_err(|e| db_error("delete_knowledge", e))?;
    }
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    if file_ids.is_empty() {
        // Nothing changed: the source stays ready, no job is started. The
        // frontend treats an empty job_id as "up to date".
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "source.refreshed",
            "source",
            source_id,
            &serde_json::json!({ "changed": 0, "removed": delta.removed.len() }).to_string(),
        );
        return Ok(ps(ProjectStudioPayload::SourceRefreshResponse {
            job_id: String::new(),
        }));
    }
    let job_id = spawn_tree_ingest_job(ctx, org, &record, &pool, source_id, &file_ids)?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.refreshed",
        "source",
        source_id,
        &serde_json::json!({
            "added": delta.added.len(),
            "changed": delta.changed.len(),
            "removed": delta.removed.len(),
        })
        .to_string(),
    );
    Ok(ps(ProjectStudioPayload::SourceRefreshResponse { job_id }))
}

fn api_spec_endpoints_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    if source.kind != "api_spec" {
        return Err(ProtocolError::bad_request("source is not an API spec"));
    }
    let endpoints_json =
        repository::get_setting(&pool, &api_spec::endpoints_setting_key(source_id))
            .map_err(|e| db_error("api_spec_endpoints", e))?
            .unwrap_or_else(|| "[]".to_string());
    Ok(ps(ProjectStudioPayload::ApiSpecEndpointsResponse {
        endpoints_json,
    }))
}

fn source_secret_set_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
    token: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    require_project(
        ctx,
        org,
        project_id,
        source_write_area(&source.kind),
        ProjectPermissionLevel::Write,
    )?;
    if source.kind != "git" {
        return Err(ProtocolError::bad_request(
            "only git sources carry an access token",
        ));
    }
    let secret_enc = match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(plain) => {
            if plain.chars().count() > environments::MAX_SECRET_CHARS {
                return Err(ProtocolError::bad_request("token is too long"));
            }
            ctx.state
                .settings_cipher
                .encrypt(plain)
                .map_err(|e| db_error("source_secret_encrypt", e))?
        }
        None => String::new(),
    };
    let ok = repository::set_source_secret(&pool, source_id, &secret_enc)
        .map_err(|e| db_error("source_secret_set", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "source.secret_set",
        "source",
        source_id,
        &serde_json::json!({ "cleared": secret_enc.is_empty() }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::SourceSecretSetResult { ok }))
}

// =============================================================================
// F3: test environments (T12)
// =============================================================================

fn environment_to_wire(
    record: EnvironmentRecord,
    names: &HashMap<String, (String, String)>,
) -> EnvironmentInfo {
    EnvironmentInfo {
        requested_by_name: display_name(names, &record.requested_by),
        decided_by_name: display_name(names, &record.decided_by),
        has_secret: !record.secret_enc.is_empty(),
        host_allowlist: environments::host_allowlist_of(&record),
        environment_id: record.environment_id,
        name: record.name,
        env_type: record.env_type,
        base_url: record.base_url,
        auth_type: record.auth_type,
        extra_headers_json: record.extra_headers_json,
        approval_status: record.approval_status,
        approval_reason: record.approval_reason,
        is_private_address: record.is_private_address,
        requested_by: record.requested_by,
        decided_by: record.decided_by,
        created_at: record.created_at,
        updated_at: record.updated_at,
        decided_at: record.decided_at,
    }
}

fn environments_to_wire(records: Vec<EnvironmentRecord>) -> Vec<EnvironmentInfo> {
    let ids: Vec<String> = records
        .iter()
        .flat_map(|r| [r.requested_by.clone(), r.decided_by.clone()])
        .filter(|id| !id.is_empty())
        .collect();
    let names = repository::resolve_user_refs(&ids);
    records
        .into_iter()
        .map(|r| environment_to_wire(r, &names))
        .collect()
}

fn environments_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Environments,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let records = environments::list(&pool).map_err(|e| db_error("environments_list", e))?;
    Ok(ps(ProjectStudioPayload::EnvironmentsListResponse {
        environments: environments_to_wire(records),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn environment_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    environment_id: Option<&str>,
    name: &str,
    env_type: &str,
    base_url: &str,
    auth_type: &str,
    secret: Option<&str>,
    extra_headers_json: &str,
    host_allowlist: &[String],
    justification: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Environments,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(ProtocolError::bad_request("environment name is required"));
    }
    if !environments::ENV_TYPES.contains(&env_type) {
        return Err(ProtocolError::bad_request(format!(
            "unknown env_type '{env_type}'"
        )));
    }
    if !environments::AUTH_TYPES.contains(&auth_type) {
        return Err(ProtocolError::bad_request(format!(
            "unknown auth_type '{auth_type}'"
        )));
    }
    let extra_headers_json = environments::validate_extra_headers(extra_headers_json)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    // DNS resolution blocks — off the async worker. The allowlist is classified
    // together with the base url: the runner may egress to every host on it, so
    // one private entry makes the whole environment private (and pending).
    let raw_url = base_url.to_string();
    let extra_hosts = host_allowlist.to_vec();
    let target =
        tokio::task::spawn_blocking(move || environments::classify_target(&raw_url, &extra_hosts))
            .await
            .map_err(|_| ProtocolError::internal("address classification task panicked"))?
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let environments::EnvironmentTarget { address, hosts } = target;
    if address.is_private && justification.trim().is_empty() {
        return Err(ProtocolError::bad_request(
            "a private or LAN address needs a justification for the administrator",
        ));
    }

    let secret_enc = match secret {
        Some(plain) => Some(
            environments::encrypt_secret(&ctx.state.settings_cipher, plain)
                .map_err(|e| db_error("environment_secret", e))?,
        ),
        None => None,
    };
    let pool = open_project_pool(project_id)?;
    let input = environments::EnvironmentInput {
        name,
        env_type,
        auth_type,
        extra_headers_json: &extra_headers_json,
        justification: justification.trim(),
        address: &address,
        host_allowlist: &hosts,
        secret_enc: secret_enc.as_deref(),
    };
    let (environment_id, approval_status, created) = match environment_id {
        Some(existing_id) => {
            let existing = environments::get(&pool, existing_id)
                .map_err(|e| db_error("environment_get", e))?
                .ok_or_else(|| ProtocolError::not_found("environment not found"))?;
            let status = environments::update(&pool, &existing, &input)
                .map_err(|e| map_unique("environment_update", "environment name is taken", e))?;
            (existing_id.to_string(), status, false)
        }
        None => {
            let new_id = uuid::Uuid::new_v4().to_string();
            let status = environments::insert(&pool, &new_id, &input, &org.user_id)
                .map_err(|e| map_unique("environment_insert", "environment name is taken", e))?;
            (new_id, status, true)
        }
    };

    if approval_status == "pending" {
        notify_environment_admins(ctx, org, project_id, &record.name, &environment_id, name);
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        if created {
            "environment.created"
        } else {
            "environment.updated"
        },
        "environment",
        &environment_id,
        &serde_json::json!({
            "name": name,
            "approval_status": approval_status,
            "is_private_address": address.is_private,
        })
        .to_string(),
    );
    Ok(ps(ProjectStudioPayload::EnvironmentSaveResponse {
        environment_id,
        approval_status,
    }))
}

/// Notifies every `project_studio.admin` holder of the org that a private-address
/// environment is waiting. Best-effort, like every other notification path.
fn notify_environment_admins(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    project_name: &str,
    environment_id: &str,
    environment_name: &str,
) {
    let memberships =
        crate::services::org::repo::list_memberships_for_org(&ctx.state.db, &org.org_id)
            .unwrap_or_default();
    let link = serde_json::json!({ "environment_id": environment_id }).to_string();
    for (admin_id, role) in memberships {
        if admin_id == org.user_id || !role.permissions.iter().any(|p| p == PERM_ADMIN) {
            continue;
        }
        notifications::notify(
            &org.org_id,
            &admin_id,
            project_id,
            "environment_pending",
            "Środowisko czeka na zatwierdzenie",
            &format!("{project_name}: {environment_name}"),
            &link,
        );
    }
}

fn environment_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    environment_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Environments,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    environments::get(&pool, environment_id)
        .map_err(|e| db_error("environment_get", e))?
        .ok_or_else(|| ProtocolError::not_found("environment not found"))?;
    let refs = environments::run_reference_count(&pool, environment_id)
        .map_err(|e| db_error("environment_refs", e))?;
    if refs > 0 {
        return Err(ProtocolError::bad_request(format!(
            "the environment is used by {refs} run(s) and cannot be deleted"
        )));
    }
    let ok = environments::delete(&pool, environment_id)
        .map_err(|e| db_error("environment_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "environment.deleted",
            "environment",
            environment_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::EnvironmentDeleteResult { ok }))
}

/// Cross-project approval queue. Admin-only on purpose: an administrator
/// decides on LAN targets without opening every project.
fn env_approvals_list_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let projects =
        repository::list_projects(&org.org_id, false).map_err(|e| db_error("projects_list", e))?;
    let mut items = Vec::new();
    for project in projects {
        let Ok(pool) = project_db::open(&project.project_id) else {
            continue;
        };
        let Ok(pending) = environments::list_pending(&pool) else {
            continue;
        };
        for record in pending {
            let justification = record.justification.clone();
            let environment = environments_to_wire(vec![record])
                .into_iter()
                .next()
                .expect("one record in, one record out");
            items.push(EnvApprovalItem {
                project_id: project.project_id.clone(),
                project_name: project.name.clone(),
                environment,
                justification,
            });
        }
    }
    Ok(ps(ProjectStudioPayload::EnvApprovalsListResponse { items }))
}

fn env_approval_decide_v1(
    ctx: &HandlerContext,
    project_id: &str,
    environment_id: &str,
    approve: bool,
    reason: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    project_db::validate_project_id(project_id)
        .map_err(|_| ProtocolError::bad_request("invalid project_id"))?;
    let project = repository::get_project(&org.org_id, project_id)
        .map_err(|e| db_error("get_project", e))?
        .ok_or_else(not_found)?;
    let reason = reason.trim();
    if !approve && reason.is_empty() {
        return Err(ProtocolError::bad_request(
            "a rejection needs a reason for the requester",
        ));
    }
    let pool = open_project_pool(project_id)?;
    let environment = environments::get(&pool, environment_id)
        .map_err(|e| db_error("environment_get", e))?
        .ok_or_else(|| ProtocolError::not_found("environment not found"))?;
    let ok = environments::decide(&pool, environment_id, approve, reason, &org.user_id)
        .map_err(|e| db_error("environment_decide", e))?;
    if ok {
        let decision = if approve { "approved" } else { "rejected" };
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "environment.decided",
            "environment",
            environment_id,
            &serde_json::json!({ "decision": decision, "reason": reason }).to_string(),
        );
        // Granting a LAN target is a security decision — it belongs in the
        // hash-chained org audit log, not only the project feed.
        activity::record_org_security(
            &ctx.state.db,
            &ctx.state.local_node_id,
            &org.user_id,
            "project_studio.environment_decision",
            &format!("project:{project_id}/environment:{environment_id}"),
            &serde_json::json!({
                "decision": decision,
                "base_url": environment.base_url,
                "is_private_address": environment.is_private_address,
                "reason": reason,
            })
            .to_string(),
        );
        notifications::notify(
            &org.org_id,
            &environment.requested_by,
            project_id,
            "environment_decided",
            if approve {
                "Środowisko zatwierdzone"
            } else {
                "Środowisko odrzucone"
            },
            &format!("{}: {}", project.name, environment.name),
            &serde_json::json!({ "environment_id": environment_id }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::EnvApprovalDecideResult { ok }))
}

// =============================================================================
// F3: build profiles
// =============================================================================

fn build_profile_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let profile = build_profiles::get(&pool, source_id)
        .map_err(|e| db_error("build_profile_get", e))?
        .map(|p| BuildProfileWire {
            profile_id: p.profile_id,
            source_id: p.source_id,
            toolchain: p.toolchain,
            base_image: p.base_image,
            install_cmd: p.install_cmd,
            test_cmd: p.test_cmd,
            workdir: p.workdir,
            proposed_by: p.proposed_by,
        });
    Ok(ps(ProjectStudioPayload::BuildProfileGetResponse {
        profile,
    }))
}

#[allow(clippy::too_many_arguments)]
fn build_profile_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    source_id: &str,
    toolchain: &str,
    base_image: &str,
    install_cmd: &str,
    test_cmd: &str,
    workdir: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let source = repository::get_source(&pool, source_id)
        .map_err(|e| db_error("get_source", e))?
        .ok_or_else(|| ProtocolError::not_found("source not found"))?;
    if !matches!(source.kind.as_str(), "git" | "zip") {
        return Err(ProtocolError::bad_request(
            "build profiles apply to git and zip sources only",
        ));
    }
    build_profiles::validate(toolchain, install_cmd, test_cmd, workdir)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let profile_id = build_profiles::upsert(
        &pool,
        source_id,
        toolchain,
        base_image.trim(),
        install_cmd.trim(),
        test_cmd.trim(),
        workdir.trim(),
        "",
    )
    .map_err(|e| db_error("build_profile_save", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "build_profile.saved",
        "source",
        source_id,
        &serde_json::json!({ "toolchain": toolchain }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::BuildProfileSaveResponse {
        profile_id,
    }))
}

// =============================================================================
// F3: runner discovery + automated runs (T10, T11)
// =============================================================================

async fn runners_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let core_db = ctx.state.db.clone();
    let discovered = tokio::task::spawn_blocking(move || auto_runs::list_runners(&core_db))
        .await
        .map_err(|_| ProtocolError::internal("runner discovery task panicked"))?
        .map_err(|e| db_error("runners_list", e))?;
    let runners = discovered
        .into_iter()
        .map(|r| RunnerInfo {
            toolchains: r
                .health
                .as_ref()
                .map(|h| {
                    h.toolchains
                        .iter()
                        .map(|t| RunnerToolchain {
                            language: t.language.clone(),
                            frameworks: t.frameworks.clone(),
                            version: t.version.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // A service row that says running but does not answer /health is
            // reported as degraded — the UI must not offer it as a target.
            status: if r.health.is_some() {
                r.status
            } else {
                "degraded".to_string()
            },
            service_id: r.service_id,
            engine_id: r.engine_id,
            display_name: r.display_name,
            endpoint_url: r.endpoint_url,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::RunnersListResponse { runners }))
}

/// Loads an environment that may actually be targeted. The approval gate is the
/// reverse of the public-web SSRF guard: a private/LAN target only becomes
/// runnable after an administrator said so, and a rejected one never does.
fn require_approved_environment(
    pool: &crate::db::DbPool,
    environment_id: &str,
) -> Result<EnvironmentRecord, ProtocolError> {
    let environment = environments::get(pool, environment_id)
        .map_err(|e| db_error("environment_get", e))?
        .ok_or_else(|| ProtocolError::bad_request("unknown environment"))?;
    if environment.approval_status != "approved" {
        return Err(ProtocolError::bad_request(format!(
            "environment '{}' is not approved (status '{}')",
            environment.name, environment.approval_status
        )));
    }
    Ok(environment)
}

/// Re-resolves the environment address immediately before a run is submitted.
/// The approval decision was taken on a DNS answer that may have moved since
/// (rebinding): a host that was public when the admin approved it can point at
/// 127.0.0.1 or 169.254.169.254 by the time the runner connects. An environment
/// that is now private, and was NOT approved as private, is refused.
/// `settle_run` closes an already-created run row so the refusal does not leave
/// a run stuck in 'running'.
async fn recheck_environment_address(
    ctx: &HandlerContext,
    org: &OrgContext,
    project_id: &str,
    environment: &EnvironmentRecord,
    settle_run: Option<(&crate::db::DbPool, &str)>,
) -> Result<(), ProtocolError> {
    let record = environment.clone();
    let now_private = tokio::task::spawn_blocking(move || environments::recheck_private(&record))
        .await
        .map_err(|_| ProtocolError::internal("address classification task panicked"))?
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if !now_private || environment.is_private_address {
        return Ok(());
    }
    let reason = format!(
        "environment '{}' now resolves to a private address — it was approved as a public \
         target, so the run was refused",
        environment.name
    );
    if let Some((pool, run_id)) = settle_run {
        let _ = auto_runs::finish_run(pool, run_id, "error", &reason);
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.environment_address_rebinding_denied",
        &format!(
            "project:{project_id}/environment:{}",
            environment.environment_id
        ),
        &serde_json::json!({ "base_url": environment.base_url }).to_string(),
    );
    Err(ProtocolError::new(ProtocolErrorCode::PolicyDenied, reason))
}

/// Upper bound of cases in one automated run.
const MAX_AUTO_RUN_CASES: usize = 200;

#[allow(clippy::too_many_arguments)]
async fn run_start_auto_v1(
    ctx: &HandlerContext,
    project_id: &str,
    name: &str,
    suite_id: &str,
    case_ids: &[String],
    from_run_id: &str,
    environment_id: &str,
    runner_service_id: &str,
    perf_profile_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Environments,
        ProjectPermissionLevel::Read,
    )?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ProtocolError::bad_request("run name is required"));
    }
    let pool = open_project_pool(project_id)?;

    let environment = require_approved_environment(&pool, environment_id)?;

    let cases = auto_runs::resolve_cases(
        &pool,
        suite_id,
        case_ids,
        from_run_id,
        MAX_AUTO_RUN_CASES,
        true,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // Manual cases carry no script — they stay in the run as skipped items so
    // a mixed suite still produces one complete report.
    let runnable_language = cases
        .iter()
        .find(|c| generation::is_code_kind(&c.kind))
        .map(|c| c.language.clone())
        .ok_or_else(|| {
            ProtocolError::bad_request("the selection contains no automatable test cases")
        })?;

    let core_db = ctx.state.db.clone();
    let runners = tokio::task::spawn_blocking(move || auto_runs::list_runners(&core_db))
        .await
        .map_err(|_| ProtocolError::internal("runner discovery task panicked"))?
        .map_err(|e| db_error("runners_list", e))?;
    let runner = auto_runs::select_runner(runners, runner_service_id, &runnable_language)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if let Some(reason) = auto_runs::isolation_refusal(&ctx.state.db, &runner) {
        return Err(ProtocolError::new(ProtocolErrorCode::PolicyDenied, reason));
    }

    let run_type = if cases
        .iter()
        .filter(|c| generation::is_code_kind(&c.kind))
        .all(|c| c.kind == "perf")
    {
        "perf"
    } else {
        "auto"
    };
    let prepared = auto_runs::create_and_prepare_run(
        &pool,
        name,
        suite_id,
        run_type,
        environment_id,
        &cases,
        &runner,
        perf_profile_json,
        &org.user_id,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let auto_runs::PreparedRun {
        run_id,
        run_no,
        submit_items,
        skipped,
    } = prepared;
    if submit_items.is_empty() {
        let _ = auto_runs::finish_run(
            &pool,
            &run_id,
            "completed",
            "nothing to execute on this runner",
        );
        return Ok(ps(ProjectStudioPayload::RunStartAutoResponse {
            run_id,
            run_no,
        }));
    }

    recheck_environment_address(ctx, org, project_id, &environment, Some((&pool, &run_id))).await?;

    // Without an auth type there is nothing to authenticate with — a stale
    // stored secret must not reach the runner (and from there a test script).
    let secret = if environment.auth_type == "none" {
        String::new()
    } else {
        environments::decrypt_secret(&ctx.state.settings_cipher, &environment)
            .map_err(|e| db_error("environment_secret", e))?
    };
    let submit_env = auto_runs::SubmitEnvironment {
        base_url: environment.base_url.clone(),
        auth_type: environment.auth_type.clone(),
        secret,
        extra_headers: serde_json::from_str(&environment.extra_headers_json)
            .unwrap_or_else(|_| serde_json::json!({})),
        host_allowlist: environments::host_allowlist_of(&environment),
    };
    let meta = auto_runs::get_meta(&pool, &run_id)
        .map_err(|e| db_error("auto_run_meta", e))?
        .ok_or_else(|| ProtocolError::internal("auto run meta missing"))?;
    auto_runs::submit_and_watch(
        pool.clone(),
        run_id.clone(),
        std::path::PathBuf::from(&record.dir_path),
        runner.endpoint_url.clone(),
        submit_items,
        submit_env,
        meta.watchdog_deadline_ms,
        project_id.to_string(),
        org.org_id.clone(),
    )
    .await
    .map_err(|e| ProtocolError::bad_request(format!("the runner refused the run: {e}")))?;

    activity::record(
        &pool,
        &org.user_id,
        "user",
        "run.started_auto",
        "run",
        &run_id,
        &serde_json::json!({
            "run_no": run_no,
            "environment_id": environment_id,
            "runner_service_id": runner.service_id,
            "items": cases.len(),
            "skipped": skipped,
        })
        .to_string(),
    );
    let _ = repository::touch_project(project_id);
    Ok(ps(ProjectStudioPayload::RunStartAutoResponse {
        run_id,
        run_no,
    }))
}

fn artifact_to_wire(record: &crate::project_studio::models::RunArtifactRecord) -> ArtifactRef {
    ArtifactRef {
        artifact_id: record.artifact_id.clone(),
        name: record.name.clone(),
        kind: record.kind.clone(),
        size_bytes: record.size_bytes,
        mime: record.mime.clone(),
        download_ref: record.artifact_id.clone(),
    }
}

fn auto_run_response(
    pool: &crate::db::DbPool,
    run: TestRunInfo,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let items = auto_runs::list_auto_items(pool, run_id).map_err(|e| db_error("auto_items", e))?;
    let artifacts =
        auto_runs::list_artifacts(pool, run_id).map_err(|e| db_error("run_artifacts", e))?;
    let mut by_item: HashMap<String, Vec<ArtifactRef>> = HashMap::new();
    for artifact in &artifacts {
        by_item
            .entry(artifact.item_id.clone())
            .or_default()
            .push(artifact_to_wire(artifact));
    }
    let items_wire: Vec<TestRunItemAutoWire> = items
        .into_iter()
        .map(|item| TestRunItemAutoWire {
            artifact_refs: by_item.remove(&item.item_id).unwrap_or_default(),
            item_id: item.item_id,
            case_id: item.case_id,
            case_title: item.case_title,
            kind: item.kind,
            language: item.language,
            position: item.position,
            status: item.status,
            duration_ms: item.duration_ms,
            message: item.message,
            steps_total: item.steps_total,
            steps_done: item.steps_done,
        })
        .collect();
    let meta = auto_runs::get_meta(pool, run_id).map_err(|e| db_error("auto_run_meta", e))?;
    let (perf_stats, perf_timeline) = match meta {
        Some(meta) => (
            serde_json::from_str::<Vec<PerfStatsWire>>(&meta.perf_summary_json).unwrap_or_default(),
            serde_json::from_str::<Vec<PerfTimelinePoint>>(&meta.perf_timeline_json)
                .unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    Ok(ps(ProjectStudioPayload::RunAutoGetResponse {
        run,
        items: items_wire,
        perf_stats,
        perf_timeline,
    }))
}

fn run_auto_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    auto_runs::reconcile_running(&pool);
    let (record, counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    let run = load_run_wire(&pool, record, counts)?;
    auto_run_response(&pool, run, run_id)
}

fn run_auto_cancel_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (run, _counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    // Manager+ cancels any run; below that only the person who started it.
    let is_manager = access.allows(ProjectArea::Tests, ProjectPermissionLevel::Admin);
    if !is_manager && run.created_by != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only a test administrator or the run's creator can cancel it",
        ));
    }
    if run.status != "running" {
        return Err(ProtocolError::bad_request("the run is not running"));
    }
    // The watcher owns the terminal write: it cancels the runner job, then
    // settles the row. Without a live watcher (restart) the row is closed here.
    let ok = if auto_runs::signal_cancel(run_id) {
        true
    } else {
        auto_runs::finish_run(&pool, run_id, "cancelled", "cancelled by the user")
            .map_err(|e| db_error("finish_run", e))?
    };
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "run.cancelled_auto",
            "run",
            run_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::RunAutoCancelResult { ok }))
}

fn try_run_cancel_v1(
    ctx: &HandlerContext,
    project_id: &str,
    try_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Environments,
        ProjectPermissionLevel::Read,
    )?;
    Ok(ps(ProjectStudioPayload::TryRunCancelResult {
        ok: auto_runs::cancel_try_run(try_id, &org.user_id),
    }))
}

fn run_artifact_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    artifact_id: &str,
    max_bytes: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Tester tier, not viewer: an artifact is raw runner output (console log,
    // junit xml, HAR) and a traceback in it can echo the request the script
    // sent, including its Authorization header. The stored bytes are scrubbed
    // of the environment secret, but everything else the target answered with
    // is still in there, so this stays with the people who execute the tests.
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let artifact = auto_runs::get_artifact(&pool, artifact_id)
        .map_err(|e| db_error("artifact_get", e))?
        .ok_or_else(|| ProtocolError::not_found("artifact not found"))?;
    // 0 = "whatever the server allows", not "one byte".
    let clamp = if max_bytes == 0 {
        ARTIFACT_MAX_BYTES as usize
    } else {
        max_bytes.min(ARTIFACT_MAX_BYTES) as usize
    };
    let dir = std::path::Path::new(&record.dir_path);
    let path = auto_runs::run_artifact_dir(dir, &artifact.run_id).join(&artifact.rel_path);
    // The stored rel_path was validated at download time; re-verify containment
    // here so a tampered row can never read outside the run directory.
    let run_dir = auto_runs::run_artifact_dir(dir, &artifact.run_id);
    let (canonical_file, canonical_root) = match (path.canonicalize(), run_dir.canonicalize()) {
        (Ok(file), Ok(root)) => (file, root),
        _ => return Err(ProtocolError::not_found("artifact file is gone")),
    };
    if !canonical_file.starts_with(&canonical_root) {
        return Err(ProtocolError::not_found("artifact not found"));
    }
    // Read one byte past the cap instead of the whole file: a 64 MiB trace must
    // not be pulled into memory to answer a 1 MiB request.
    let mut file = std::fs::File::open(&canonical_file)
        .map_err(|_| ProtocolError::not_found("artifact file is gone"))?;
    let mut bytes = Vec::with_capacity(clamp.min(64 * 1024));
    let mut limited = std::io::Read::take(&mut file, clamp as u64 + 1);
    std::io::Read::read_to_end(&mut limited, &mut bytes)
        .map_err(|_| ProtocolError::not_found("artifact file is gone"))?;
    let truncated = bytes.len() > clamp;
    bytes.truncate(clamp);
    Ok(ps(ProjectStudioPayload::RunArtifactGetResponse {
        bytes,
        mime: artifact.mime,
        truncated,
    }))
}

/// Server clamp on an artifact download (32 MiB, per the protocol contract).
const ARTIFACT_MAX_BYTES: u32 = 32 * 1024 * 1024;

// =============================================================================
// F4: run schedules (T13)
// =============================================================================

/// Wire-shaped schedule definition, grouped so the save handler stays inside
/// the argument budget.
struct ScheduleWire<'a> {
    name: &'a str,
    run_type: &'a str,
    suite_id: &'a str,
    case_ids: &'a [String],
    environment_id: &'a str,
    runner_service_id: &'a str,
    perf_profile_json: &'a str,
    assignment_mode: &'a str,
    assignees: &'a [String],
    schedule_kind: &'a str,
    schedule_expr: &'a str,
    timezone: &'a str,
    enabled: bool,
}

fn schedule_to_wire(
    record: &crate::project_studio::models::ScheduleRecord,
    suite_names: &HashMap<String, String>,
    environments: &HashMap<String, (String, String)>,
    runners: &HashMap<String, String>,
    names: &HashMap<String, (String, String)>,
    now: chrono::DateTime<chrono::Utc>,
) -> ScheduleInfo {
    let case_ids: Vec<String> = serde_json::from_str(&record.case_ids_json).unwrap_or_default();
    let assignees: Vec<String> = serde_json::from_str(&record.assignees_json).unwrap_or_default();
    let (environment_name, environment_status) = environments
        .get(&record.environment_id)
        .cloned()
        .unwrap_or_default();
    ScheduleInfo {
        schedule_id: record.schedule_id.clone(),
        name: record.name.clone(),
        enabled: record.enabled,
        auto_disabled: record.auto_disabled,
        run_type: record.run_type.clone(),
        suite_name: suite_names
            .get(&record.suite_id)
            .cloned()
            .unwrap_or_default(),
        suite_id: record.suite_id.clone(),
        cases_count: case_ids.len() as u32,
        case_ids,
        environment_id: record.environment_id.clone(),
        environment_name,
        environment_status,
        runner_display_name: runners
            .get(&record.runner_service_id)
            .cloned()
            .unwrap_or_default(),
        runner_service_id: record.runner_service_id.clone(),
        perf_profile_json: record.perf_profile_json.clone(),
        assignment_mode: record.assignment_mode.clone(),
        assignees,
        schedule_kind: record.schedule_kind.clone(),
        schedule_expr: record.schedule_expr.clone(),
        timezone: record.timezone.clone(),
        next_run_at: record.next_run_at.clone().unwrap_or_default(),
        // Computed SERVER-side from the same arithmetic the loop uses: a
        // preview the UI derived itself would disagree around a DST switch.
        next_runs_preview: if record.enabled && !record.auto_disabled {
            schedules::next_runs_preview(
                &record.schedule_kind,
                &record.schedule_expr,
                &record.timezone,
                now,
                3,
            )
        } else {
            Vec::new()
        },
        last_trigger_at: record.last_trigger_at.clone(),
        last_run_id: record.last_run_id.clone(),
        last_run_no: 0,
        last_status: record.last_status.clone(),
        last_reason: record.last_reason.clone(),
        consecutive_failures: record.consecutive_failures,
        created_by_name: display_name(names, &record.created_by),
        created_by: record.created_by.clone(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
    }
}

/// Environment id → (name, approval status), for the list decoration and the
/// "blocked" highlight.
fn environment_index(pool: &crate::db::DbPool) -> HashMap<String, (String, String)> {
    environments::list(pool)
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.environment_id, (e.name, e.approval_status)))
        .collect()
}

fn schedules_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let records = schedules::list(&pool).map_err(|e| db_error("schedules_list", e))?;
    let suite_ids: Vec<String> = records.iter().map(|r| r.suite_id.clone()).collect();
    let suite_names =
        ps_tests::suite_names(&pool, &suite_ids).map_err(|e| db_error("suite_names", e))?;
    let environments = environment_index(&pool);
    let creator_ids: Vec<String> = records.iter().map(|r| r.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&creator_ids);
    let runners: HashMap<String, String> = auto_runs::list_runners(&ctx.state.db)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.service_id, r.display_name))
        .collect();
    let run_ids: Vec<String> = records.iter().map(|r| r.last_run_id.clone()).collect();
    let run_numbers = schedules::run_numbers(&pool, &run_ids).unwrap_or_default();
    let now = chrono::Utc::now();
    let schedules_wire = records
        .iter()
        .map(|record| {
            let mut wire =
                schedule_to_wire(record, &suite_names, &environments, &runners, &names, now);
            wire.last_run_no = run_numbers.get(&record.last_run_id).copied().unwrap_or(0);
            wire
        })
        .collect();
    Ok(ps(ProjectStudioPayload::SchedulesListResponse {
        schedules: schedules_wire,
        // The node's own zone: the UI renders "next run" next to it so a user in
        // another zone reads the same instant the loop will use.
        server_timezone: schedules::server_timezone(),
    }))
}

/// Validates a wire definition and returns the storable input. Every rule that
/// depends on CURRENT state (the environment approval, the case set) is checked
/// here, at save time — a schedule saved against an unapproved environment
/// would only fail hours later, in the middle of the night.
fn validate_schedule(
    pool: &crate::db::DbPool,
    wire: &ScheduleWire<'_>,
) -> Result<(String, String, String), ProtocolError> {
    let name = wire.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ProtocolError::bad_request("schedule name is required"));
    }
    if !schedules::RUN_TYPES.contains(&wire.run_type) {
        return Err(ProtocolError::bad_request(format!(
            "unknown run_type '{}'",
            wire.run_type
        )));
    }
    let selectors = [!wire.suite_id.is_empty(), !wire.case_ids.is_empty()];
    if selectors.iter().filter(|s| **s).count() != 1 {
        return Err(ProtocolError::bad_request(
            "provide exactly one of suite_id / case_ids",
        ));
    }
    if !wire.suite_id.is_empty()
        && ps_tests::get_suite(pool, wire.suite_id)
            .map_err(|e| db_error("suite_get", e))?
            .is_none()
    {
        return Err(ProtocolError::not_found("suite not found"));
    }
    if wire.case_ids.len() > 200 {
        return Err(ProtocolError::bad_request(
            "a schedule accepts at most 200 cases",
        ));
    }
    let automated = wire.run_type == "auto" || wire.run_type == "perf";
    if automated {
        if wire.environment_id.is_empty() {
            return Err(ProtocolError::bad_request(
                "an automated schedule requires an environment",
            ));
        }
        // The approval must hold AT SAVE TIME; the loop re-checks it before
        // every trigger and blocks the run if it was withdrawn since.
        require_approved_environment(pool, wire.environment_id)?;
    } else if !wire.environment_id.is_empty() {
        return Err(ProtocolError::bad_request(
            "a manual schedule must not bind an environment",
        ));
    }

    let assignment_mode = if automated {
        // Automated runs are pool runs — `create_auto_run` writes 'pool'.
        "pool".to_string()
    } else {
        let mode = if wire.assignment_mode.is_empty() {
            "pool"
        } else {
            wire.assignment_mode
        };
        if !matches!(mode, "single" | "per_case" | "pool") {
            return Err(ProtocolError::bad_request(format!(
                "unknown assignment_mode '{mode}'"
            )));
        }
        mode.to_string()
    };
    if !schedules::SCHEDULE_KINDS.contains(&wire.schedule_kind) {
        return Err(ProtocolError::bad_request(format!(
            "unknown schedule_kind '{}'",
            wire.schedule_kind
        )));
    }
    schedules::parse_timezone(wire.timezone)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let now = chrono::Utc::now();
    let next =
        schedules::compute_next_run(wire.schedule_kind, wire.schedule_expr, wire.timezone, now)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if wire.schedule_kind == "once" && next.is_none() {
        return Err(ProtocolError::bad_request(
            "the one-shot instant is already in the past",
        ));
    }
    let case_ids_json = serde_json::to_string(wire.case_ids).unwrap_or_else(|_| "[]".to_string());
    let assignees_json = serde_json::to_string(wire.assignees).unwrap_or_else(|_| "[]".to_string());
    Ok((assignment_mode, case_ids_json, assignees_json))
}

fn schedule_save_v1(
    ctx: &HandlerContext,
    project_id: &str,
    schedule_id: Option<&str>,
    wire: &ScheduleWire<'_>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    if matches!(wire.run_type, "auto" | "perf") {
        require_project(
            ctx,
            org,
            project_id,
            ProjectArea::Environments,
            ProjectPermissionLevel::Read,
        )?;
    }
    let pool = open_project_pool(project_id)?;
    let (assignment_mode, case_ids_json, assignees_json) = validate_schedule(&pool, wire)?;

    for user_id in wire.assignees {
        if !repository::project_access(&record, user_id, false)
            .map_err(|e| db_error("member_access", e))?
            .allows(ProjectArea::Tests, ProjectPermissionLevel::Write)
        {
            return Err(ProtocolError::bad_request(format!(
                "user '{user_id}' is not a project member"
            )));
        }
    }

    let now = chrono::Utc::now();
    let next = if wire.enabled {
        schedules::compute_next_run(wire.schedule_kind, wire.schedule_expr, wire.timezone, now)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?
            .map(|dt| dt.to_rfc3339())
    } else {
        None
    };
    let perf_profile_json = if wire.perf_profile_json.trim().is_empty() {
        "{}"
    } else {
        wire.perf_profile_json
    };
    let input = schedules::ScheduleInput {
        name: wire.name.trim(),
        run_type: wire.run_type,
        suite_id: wire.suite_id,
        case_ids_json: &case_ids_json,
        environment_id: wire.environment_id,
        runner_service_id: wire.runner_service_id,
        perf_profile_json,
        assignment_mode: &assignment_mode,
        assignees_json: &assignees_json,
        schedule_kind: wire.schedule_kind,
        schedule_expr: wire.schedule_expr.trim(),
        timezone: if wire.timezone.trim().is_empty() {
            "UTC"
        } else {
            wire.timezone.trim()
        },
        enabled: wire.enabled,
    };

    let schedule_id = match schedule_id {
        Some(id) => {
            if schedules::get(&pool, id)
                .map_err(|e| db_error("schedule_get", e))?
                .is_none()
            {
                return Err(ProtocolError::not_found("schedule not found"));
            }
            schedules::update(&pool, id, &input, next.as_deref())
                .map_err(|e| db_error("schedule_update", e))?;
            id.to_string()
        }
        None => {
            if schedules::count(&pool).map_err(|e| db_error("schedules_count", e))?
                >= schedules::MAX_SCHEDULES_PER_PROJECT
            {
                return Err(ProtocolError::bad_request(format!(
                    "a project holds at most {} schedules",
                    schedules::MAX_SCHEDULES_PER_PROJECT
                )));
            }
            let id = uuid::Uuid::new_v4().to_string();
            schedules::insert(&pool, &id, &input, next.as_deref(), &org.user_id)
                .map_err(|e| db_error("schedule_insert", e))?;
            id
        }
    };
    // The registry hint is what the loop reads; without this refresh the new
    // schedule would not be selected until something else touched the project.
    let _ = schedules::refresh_hint(project_id, &org.org_id);
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "schedule.saved",
        "schedule",
        &schedule_id,
        &serde_json::json!({
            "name": wire.name.trim(),
            "kind": wire.schedule_kind,
            "enabled": wire.enabled,
        })
        .to_string(),
    );
    Ok(ps(ProjectStudioPayload::ScheduleSaveResponse {
        schedule_id,
        next_run_at: next.clone().unwrap_or_default(),
        next_runs_preview: if wire.enabled {
            schedules::next_runs_preview(
                wire.schedule_kind,
                wire.schedule_expr,
                wire.timezone,
                now,
                3,
            )
        } else {
            Vec::new()
        },
    }))
}

fn schedule_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    schedule_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let ok = schedules::delete(&pool, schedule_id).map_err(|e| db_error("schedule_delete", e))?;
    if ok {
        let _ = schedules::refresh_hint(project_id, &org.org_id);
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "schedule.deleted",
            "schedule",
            schedule_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::ScheduleDeleteResult { ok }))
}

fn schedule_set_enabled_v1(
    ctx: &HandlerContext,
    project_id: &str,
    schedule_id: &str,
    enabled: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Admin,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let schedule = schedules::get(&pool, schedule_id)
        .map_err(|e| db_error("schedule_get", e))?
        .ok_or_else(|| ProtocolError::not_found("schedule not found"))?;
    if enabled && matches!(schedule.run_type.as_str(), "auto" | "perf") {
        require_project(
            ctx,
            org,
            project_id,
            ProjectArea::Environments,
            ProjectPermissionLevel::Read,
        )?;
    }
    let next = if enabled {
        schedules::compute_next_run(
            &schedule.schedule_kind,
            &schedule.schedule_expr,
            &schedule.timezone,
            chrono::Utc::now(),
        )
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?
        .map(|dt| dt.to_rfc3339())
    } else {
        None
    };
    if enabled && next.is_none() {
        return Err(ProtocolError::bad_request(
            "this schedule has no future fire instant — edit it first",
        ));
    }
    let ok = schedules::set_enabled(&pool, schedule_id, enabled, next.as_deref())
        .map_err(|e| db_error("schedule_set_enabled", e))?;
    if ok {
        let _ = schedules::refresh_hint(project_id, &org.org_id);
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "schedule.toggled",
            "schedule",
            schedule_id,
            &serde_json::json!({ "enabled": enabled }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::ScheduleSetEnabledResult {
        ok,
        next_run_at: next.unwrap_or_default(),
        auto_disabled: false,
    }))
}

async fn schedule_run_now_v1(
    ctx: &HandlerContext,
    project_id: &str,
    schedule_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let schedule = schedules::get(&pool, schedule_id)
        .map_err(|e| db_error("schedule_get", e))?
        .ok_or_else(|| ProtocolError::not_found("schedule not found"))?;
    let trigger_ctx = schedules::TriggerCtx {
        core_db: ctx.state.db.clone(),
        settings_cipher: ctx.state.settings_cipher.clone(),
        node_id: ctx.state.local_node_id.clone(),
    };
    // Same gate chain as the loop; `next_run_at` is deliberately NOT advanced —
    // a manual trigger is extra, not a replacement for the scheduled one.
    let outcome = schedules::trigger_once(
        &trigger_ctx,
        &record,
        &pool,
        &schedule,
        &chrono::Utc::now().to_rfc3339(),
        &org.user_id,
    )
    .await;
    Ok(ps(ProjectStudioPayload::ScheduleRunNowResponse {
        outcome: outcome.outcome,
        reason: outcome.reason,
        run_id: outcome.run_id,
        run_no: outcome.run_no,
    }))
}

fn schedule_runs_list_v1(
    ctx: &HandlerContext,
    project_id: &str,
    schedule_id: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Tests,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let limit = if limit == 0 { 50 } else { limit.clamp(1, 200) };
    let records = schedules::list_triggers(&pool, schedule_id, limit)
        .map_err(|e| db_error("schedule_runs", e))?;
    let run_ids: Vec<String> = records.iter().map(|r| r.run_id.clone()).collect();
    let run_numbers = schedules::run_numbers(&pool, &run_ids).unwrap_or_default();
    let actor_ids: Vec<String> = records.iter().map(|r| r.actor.clone()).collect();
    let names = repository::resolve_user_refs(&actor_ids);
    let runs = records
        .into_iter()
        .map(|r| ScheduleRunWire {
            trigger_id: r.trigger_id,
            scheduled_for: r.scheduled_for,
            fired_at: r.fired_at,
            outcome: r.outcome,
            reason: r.reason,
            run_no: run_numbers.get(&r.run_id).copied().unwrap_or(0),
            run_id: r.run_id,
            run_status: r.run_status,
            actor_name: if r.actor.is_empty() {
                String::new()
            } else {
                display_name(&names, &r.actor)
            },
            actor: r.actor,
        })
        .collect();
    Ok(ps(ProjectStudioPayload::ScheduleRunsListResponse { runs }))
}

// =============================================================================
// F4: ML Studio links (X02)
// =============================================================================

fn ml_summary_to_wire(
    summary: crate::project_studio::ml_link::MlProjectSummary,
) -> MlProjectSummaryWire {
    MlProjectSummaryWire {
        deep_link: format!("/ml-studio/projects/{}", summary.ml_project_id),
        ml_project_id: summary.ml_project_id,
        name: summary.name,
        project_type: summary.project_type,
        project_type_label: summary.project_type_label,
        status: summary.status,
        dataset_count: summary.dataset_count,
        model_count: summary.model_count,
        models: summary.models,
        last_training_run_id: summary.last_training_run_id,
        last_training_status: summary.last_training_status,
        last_training_started_at: summary.last_training_started_at,
        last_training_finished_at: summary.last_training_finished_at,
        last_training_metrics_json: summary.last_training_metrics_json,
        training_in_progress: summary.training_in_progress,
    }
}

fn role_map_to_wire(map: &[(String, String)]) -> Vec<MlRoleMapEntry> {
    map.iter()
        .map(|(project_role, ml_role)| MlRoleMapEntry {
            project_role: project_role.clone(),
            ml_role: ml_role.clone(),
        })
        .collect()
}

fn role_map_from_wire(entries: &[MlRoleMapEntry]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|e| (e.project_role.clone(), e.ml_role.clone()))
        .collect()
}

fn ml_links_list_v1(ctx: &HandlerContext, project_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Knowledge,
        ProjectPermissionLevel::Read,
    )?;
    let pool = open_project_pool(project_id)?;
    let records = ml_link::list(&pool).map_err(|e| db_error("ml_links_list", e))?;
    let creator_ids: Vec<String> = records.iter().map(|r| r.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&creator_ids);
    let links = records
        .into_iter()
        .map(|record| {
            let summary = ml_link::summary(&record.ml_project_id)
                .ok()
                .flatten()
                .map(ml_summary_to_wire);
            MlLinkInfo {
                link_id: record.link_id,
                // The deep link is only useful to someone ML Studio will let in.
                can_open: ml_link::ml_member_role(&record.ml_project_id, &org.user_id).is_some(),
                ml_project_id: record.ml_project_id,
                label: record.label,
                origin: record.origin,
                sync_permissions: record.sync_permissions,
                role_map: role_map_to_wire(&ml_link::role_map_from_json(&record.role_map_json)),
                last_sync_at: record.last_sync_at,
                last_sync_result: record.last_sync_result,
                created_by_name: display_name(&names, &record.created_by),
                created_by: record.created_by,
                created_at: record.created_at,
                summary,
            }
        })
        .collect();
    Ok(ps(ProjectStudioPayload::MlLinksListResponse {
        links,
        can_manage: access.allows(ProjectArea::Settings, ProjectPermissionLevel::Write)
            && access.allows(ProjectArea::Repos, ProjectPermissionLevel::Write),
    }))
}

fn ml_project_create_from_project_v1(
    ctx: &HandlerContext,
    project_id: &str,
    ml_name: &str,
    project_type: &str,
    role_map: &[MlRoleMapEntry],
    sync_permissions: bool,
    label: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let map = role_map_from_wire(role_map);
    let (link_id, ml_project_id, members_mapped, members_skipped) = ml_link::create_from_project(
        &pool,
        project_id,
        &org.org_id,
        &org.user_id,
        ml_name.trim(),
        project_type,
        label.trim(),
        sync_permissions,
        &map,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    activity::record(
        &pool,
        &org.user_id,
        "user",
        "ml_link.created",
        "ml_link",
        &link_id,
        &serde_json::json!({
            "ml_project_id": ml_project_id,
            "members_mapped": members_mapped,
        })
        .to_string(),
    );
    Ok(ps(
        ProjectStudioPayload::MlProjectCreateFromProjectResponse {
            link_id,
            ml_project_id,
            members_mapped,
            members_skipped,
        },
    ))
}

fn ml_project_candidates_v1(
    ctx: &HandlerContext,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let candidates = ml_link::owned_candidates(&pool, &org.user_id)
        .map_err(|e| db_error("ml_candidates", e))?
        .into_iter()
        .map(ml_summary_to_wire)
        .collect();
    Ok(ps(ProjectStudioPayload::MlProjectCandidatesResponse {
        candidates,
    }))
}

fn ml_link_attach_v1(
    ctx: &HandlerContext,
    project_id: &str,
    ml_project_id: &str,
    label: &str,
    sync_permissions: bool,
    role_map: &[MlRoleMapEntry],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    // Attaching requires OWNERSHIP of the ML project: every membership write the
    // sync performs goes through owner-only repository calls, so a link created
    // by a non-owner could never apply anything.
    let ml_project = crate::ml_studio::repository::get_project(&org.user_id, ml_project_id)
        .map_err(|e| db_error("ml_project", e))?
        .filter(|summary| summary.project.org_id == org.org_id)
        .ok_or_else(|| ProtocolError::not_found("ML project not found"))?;
    if ml_project.role != "owner" {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the owner of the ML project may link it",
        ));
    }
    let map = role_map_from_wire(role_map);
    ml_link::validate_role_map(&map).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let pool = open_project_pool(project_id)?;
    if ml_link::count(&pool).map_err(|e| db_error("ml_links_count", e))?
        >= ml_link::MAX_LINKS_PER_PROJECT
    {
        return Err(ProtocolError::bad_request(format!(
            "a project holds at most {} ML Studio links",
            ml_link::MAX_LINKS_PER_PROJECT
        )));
    }
    let link_id = uuid::Uuid::new_v4().to_string();
    ml_link::insert(
        &pool,
        &link_id,
        ml_project_id,
        label.trim(),
        "linked_existing",
        sync_permissions,
        &ml_link::role_map_to_json(&map),
        &org.user_id,
    )
    .map_err(|e| map_unique("ml_link_attach", "this ML project is already linked", e))?;

    if sync_permissions {
        let project = project_id.to_string();
        tokio::spawn(async move {
            tokio::task::spawn_blocking(move || ml_link::sync_project_memberships(project)).await
        });
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "ml_link.attached",
        "ml_link",
        &link_id,
        &serde_json::json!({ "ml_project_id": ml_project_id }).to_string(),
    );
    Ok(ps(ProjectStudioPayload::MlLinkAttachResponse { link_id }))
}

fn ml_link_update_v1(
    ctx: &HandlerContext,
    project_id: &str,
    link_id: &str,
    label: &str,
    sync_permissions: bool,
    role_map: &[MlRoleMapEntry],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    if ml_link::get(&pool, link_id)
        .map_err(|e| db_error("ml_link_get", e))?
        .is_none()
    {
        return Err(ProtocolError::not_found("link not found"));
    }
    let map = role_map_from_wire(role_map);
    ml_link::validate_role_map(&map).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let ok = ml_link::update(
        &pool,
        link_id,
        label.trim(),
        sync_permissions,
        &ml_link::role_map_to_json(&map),
    )
    .map_err(|e| db_error("ml_link_update", e))?;
    if ok && sync_permissions {
        let project = project_id.to_string();
        tokio::spawn(async move {
            tokio::task::spawn_blocking(move || ml_link::sync_project_memberships(project)).await
        });
    }
    Ok(ps(ProjectStudioPayload::MlLinkUpdateResult { ok }))
}

fn ml_link_detach_v1(
    ctx: &HandlerContext,
    project_id: &str,
    link_id: &str,
    revoke_members: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let link = ml_link::get(&pool, link_id)
        .map_err(|e| db_error("ml_link_get", e))?
        .ok_or_else(|| ProtocolError::not_found("link not found"))?;
    let members_removed = ml_link::detach(&pool, project_id, &link, revoke_members)
        .map_err(|e| db_error("ml_link_detach", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "ml_link.detached",
        "ml_link",
        link_id,
        &serde_json::json!({
            "ml_project_id": link.ml_project_id,
            "members_removed": members_removed,
        })
        .to_string(),
    );
    Ok(ps(ProjectStudioPayload::MlLinkDetachResult {
        ok: true,
        members_removed,
    }))
}

fn ml_link_sync_now_v1(
    ctx: &HandlerContext,
    project_id: &str,
    link_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) = require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Settings,
        ProjectPermissionLevel::Write,
    )?;
    require_active(&record)?;
    require_project(
        ctx,
        org,
        project_id,
        ProjectArea::Repos,
        ProjectPermissionLevel::Write,
    )?;
    let pool = open_project_pool(project_id)?;
    let link = ml_link::get(&pool, link_id)
        .map_err(|e| db_error("ml_link_get", e))?
        .ok_or_else(|| ProtocolError::not_found("link not found"))?;
    let outcome = ml_link::sync_link(project_id, &pool, &link);
    let refreshed = ml_link::get(&pool, link_id)
        .map_err(|e| db_error("ml_link_get", e))?
        .unwrap_or(link);
    Ok(ps(ProjectStudioPayload::MlLinkSyncNowResponse {
        outcome: MlSyncOutcomeWire {
            applied_add: outcome.applied_add,
            applied_update: outcome.applied_update,
            applied_remove: outcome.applied_remove,
            skipped: outcome.skipped,
            errors: outcome.errors,
        },
        last_sync_at: refreshed.last_sync_at,
        last_sync_result: refreshed.last_sync_result,
    }))
}

/// Fires the one-way project → ML Studio permission sync after a membership
/// mutation. Spawned and never awaited: an ML Studio failure must not roll back
/// (or even delay) the membership change that already succeeded.
fn spawn_ml_permission_sync(project_id: &str) {
    let project = project_id.to_string();
    tokio::spawn(async move {
        let _ =
            tokio::task::spawn_blocking(move || ml_link::sync_project_memberships(project)).await;
    });
}

// =============================================================================
// F4: kanban board (Z01)
// =============================================================================

fn task_status_set_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    status: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _access) =
        require_task_access(ctx, org, project_id, ProjectPermissionLevel::Write)?;
    require_active(&record)?;
    if !tasks::TASK_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    let pool = open_project_pool(project_id)?;
    tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    // Status-only write: TaskInfo (what the board renders) carries neither
    // `description_md` nor `attachments`, so routing a card move through
    // TaskSave would write both back empty.
    let mutation = tasks::set_task_status(&pool, task_id, status, &org.user_id)
        .map_err(|e| task_mutation_error("task_status_set", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let task = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    if mutation.changed {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task.status_changed",
            "task",
            task_id,
            &serde_json::json!({"status":status,"event_id":mutation.status_event_id}).to_string(),
        );
        notifications::notify_task_changes(ctx, project_id, &mutation);
    }
    Ok(ps(ProjectStudioPayload::TaskStatusSetResult {
        ok: true,
        updated_at: task.updated_at,
        event_id: mutation.status_event_id,
        previous_status: mutation.previous_status,
    }))
}

// =============================================================================
// F4: project export / import
// =============================================================================

/// Signed-URL lifetime for a finished archive. Matches the retention window so
/// a link never outlives the file it points at.
const PS_EXPORT_URL_TTL_SECS: u64 = 7 * 24 * 3600;

// Upload staging caps for a streamed-to-disk import archive. No chunk bytes are
// ever held in RAM — each one is appended straight to the staging file.
const MAX_PS_UPLOAD_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_PS_UPLOAD_CHUNKS: u32 = 200_000;
const PS_UPLOAD_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);
const PS_CONSUMED_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const MAX_PS_UPLOADS_PER_USER: usize = 2;

/// One in-flight (or recently consumed) import upload. Holds NO payload bytes.
struct PsArchiveUpload {
    owner_user_id: String,
    path: std::path::PathBuf,
    total_chunks: u32,
    next_seq: u32,
    received_bytes: u64,
    last_chunk_len: usize,
    consumed: bool,
    last_touch: std::time::Instant,
}

fn ps_uploads() -> &'static std::sync::Mutex<HashMap<String, PsArchiveUpload>> {
    static UPLOADS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PsArchiveUpload>>> =
        std::sync::OnceLock::new();
    UPLOADS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

// job_id → export_ref, so the status handler can mint the signed URL for a
// finished export (the job record carries progress, not the archive path).
fn ps_export_refs() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static REFS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    REFS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Drops refs whose archive is gone — reaped by retention, deleted by hand, or
/// never produced because the export failed. The map is in-memory and lives as
/// long as the process, so without this it only ever grows.
fn prune_export_refs() {
    if let Ok(mut refs) = ps_export_refs().lock() {
        refs.retain(|_, export_ref| {
            crate::api::project_studio_export::export_archive_path(export_ref).is_file()
        });
    }
}

fn append_upload_chunk(path: &std::path::Path, bytes: &[u8]) -> Result<(), ProtocolError> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| ProtocolError::internal(format!("open staging file: {e}")))?;
    f.write_all(bytes)
        .map_err(|e| ProtocolError::internal(format!("append chunk: {e}")))?;
    Ok(())
}

/// Evicts stale uploads (deleting their temp files) and sweeps the staging dir
/// for files orphaned by a restart — the map is in-memory only.
fn reap_ps_uploads(map: &mut HashMap<String, PsArchiveUpload>) {
    let now = std::time::Instant::now();
    let stale: Vec<String> = map
        .iter()
        .filter(|(_, e)| {
            let ttl = if e.consumed {
                PS_CONSUMED_TTL
            } else {
                PS_UPLOAD_TTL
            };
            now.duration_since(e.last_touch) >= ttl
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(entry) = map.remove(&id) {
            let _ = std::fs::remove_file(&entry.path);
        }
    }
    let referenced: HashSet<std::path::PathBuf> = map.values().map(|e| e.path.clone()).collect();
    if let Ok(entries) = std::fs::read_dir(crate::paths::project_studio_import_staging_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ours = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("psimp_"))
                .unwrap_or(false);
            if ours && !referenced.contains(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn inventory_to_wire(inventory: &archive::Inventory) -> ArchiveInventoryWire {
    ArchiveInventoryWire {
        cases: inventory.cases,
        suites: inventory.suites,
        runs: inventory.runs,
        tasks: inventory.tasks,
        documents: inventory.documents,
        sources: inventory.sources,
        files: inventory.files,
        bytes_files: inventory.bytes_files,
        bytes_runs: inventory.bytes_runs,
        vectors: inventory.vectors,
        vector_dim: inventory.vector_dim,
        embedding_alias: inventory.embedding_alias.clone(),
        embedding_model: inventory.embedding_model.clone(),
    }
}

fn project_export_start_v1(
    ctx: &HandlerContext,
    project_id: &str,
    include_runs: bool,
    include_vectors: bool,
    include_user_names: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // An ARCHIVED project may still be exported — that is often exactly why it
    // was archived — so `require_active` deliberately does not apply here.
    let (record, _access) = require_project_access(ctx, org, project_id)?;
    if !_access.project_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project export access denied",
        ));
    }
    archive::reap_export_archives();
    prune_export_refs();

    let export_ref = format!("psexp_{}", uuid::Uuid::new_v4());
    let dest = crate::api::project_studio_export::export_archive_path(&export_ref);
    let job_id = archive::spawn_export(archive::ExportTask {
        core_db: ctx.state.db.clone(),
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        node_id: ctx.state.local_node_id.to_string(),
        project_id: project_id.to_string(),
        dir_path: std::path::PathBuf::from(&record.dir_path),
        project: archive::ProjectMeta {
            project_id: project_id.to_string(),
            key_prefix: record.key_prefix.clone(),
            name: record.name.clone(),
            description: record.description.clone(),
            template: record.template.clone(),
            modules: parse_modules_json(&record.modules_json),
            owner_user_id: record.owner_user_id.clone(),
            org_id: record.org_id.clone(),
        },
        options: archive::ExportOptions {
            include_runs,
            include_vectors,
            include_user_names,
        },
        export_ref: export_ref.clone(),
        dest_zip: dest,
    })
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    ps_export_refs()
        .lock()
        .map_err(|_| ProtocolError::internal("export ref registry poisoned"))?
        .insert(job_id.clone(), export_ref);

    if include_user_names {
        // Display names are personal data; who asked for them and when is worth
        // an audit row of its own.
        activity::record_org_security(
            &ctx.state.db,
            &ctx.state.local_node_id,
            &org.user_id,
            "project_studio.project.exported_with_user_names",
            project_id,
            &record.name,
        );
    }
    if let Ok(pool) = project_db::open(project_id) {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "project.export_started",
            "project",
            project_id,
            &serde_json::json!({
                "include_runs": include_runs,
                "include_vectors": include_vectors,
                "include_user_names": include_user_names,
            })
            .to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::ProjectExportStartResponse {
        job_id,
    }))
}

fn project_export_status_v1(
    ctx: &HandlerContext,
    project_id: &str,
    job_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _access) = require_project_access(ctx, org, project_id)?;
    if !_access.project_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "project export access denied",
        ));
    }
    // Authorize on the job's stored owner — never on the bare job id.
    let job = archive::job(job_id)
        .filter(|j| j.owner_user_id == org.user_id && j.project_id == project_id)
        .ok_or_else(|| ProtocolError::not_found("export job not found"))?;

    let mut signed_url = String::new();
    let mut export_ref = String::new();
    if job.status == "success" {
        if let Some(ref_id) = ps_export_refs()
            .lock()
            .ok()
            .and_then(|map| map.get(job_id).cloned())
        {
            if crate::api::project_studio_export::export_archive_path(&ref_id).is_file() {
                let issuer = crate::services::project_studio_export_url_issuer();
                match issuer.issue(ref_id.clone(), PS_EXPORT_URL_TTL_SECS) {
                    Ok(signed) => {
                        signed_url = format!(
                            "/project-studio/exports/{}?{}",
                            ref_id,
                            signed.query_string()
                        );
                    }
                    Err(e) => tracing::warn!("project studio export signed url failed: {e}"),
                }
                export_ref = ref_id;
            }
        }
    } else if job.status == "failed" {
        // No archive will ever exist for this job.
        if let Ok(mut refs) = ps_export_refs().lock() {
            refs.remove(job_id);
        }
    }
    Ok(ps(ProjectStudioPayload::ProjectExportStatusResponse {
        job_id: job_id.to_string(),
        status: job.status,
        progress_pct: job.progress_pct,
        phase: job.phase,
        error: job.error,
        export_ref,
        signed_url,
        archive_bytes: job.archive_bytes,
        inventory: job.inventory.as_ref().map(inventory_to_wire),
    }))
}

/// Importing creates a NEW project, so it needs the creator grant — exactly like
/// the create wizard. Membership in the archived project means nothing here: it
/// came from another node.
fn require_import_grant(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_read(ctx)?;
    let can_create = is_admin(ctx)
        || repository::has_creator_grant(&org.user_id, &org.org_id)
            .map_err(|e| db_error("creator_grant", e))?;
    if !can_create {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "importing a project requires a creator grant",
        ));
    }
    Ok(org)
}

fn project_import_upload_chunk_v1(
    ctx: &HandlerContext,
    upload_id: &str,
    _filename: &str,
    seq: u32,
    total_chunks: u32,
    bytes: &[u8],
) -> Result<MessageBody, ProtocolError> {
    let org = require_import_grant(ctx)?;
    if upload_id.trim().is_empty() || upload_id.len() > 128 {
        return Err(ProtocolError::bad_request("invalid upload_id"));
    }
    if total_chunks == 0 || total_chunks > MAX_PS_UPLOAD_CHUNKS {
        return Err(ProtocolError::bad_request("invalid total_chunks"));
    }
    if seq >= total_chunks {
        return Err(ProtocolError::bad_request("chunk seq out of range"));
    }

    let mut map = ps_uploads()
        .lock()
        .map_err(|_| ProtocolError::internal("upload registry poisoned"))?;
    reap_ps_uploads(&mut map);
    let chunk_len = bytes.len();
    let complete = match map.get_mut(upload_id) {
        Some(entry) => {
            // A foreign-owned id must look exactly like an unknown one.
            if entry.owner_user_id != org.user_id {
                return Err(ProtocolError::not_found("upload not found"));
            }
            if entry.consumed {
                return Err(ProtocolError::bad_request("upload already consumed"));
            }
            if entry.total_chunks != total_chunks {
                return Err(ProtocolError::bad_request("total_chunks mismatch"));
            }
            if seq == entry.next_seq {
                if entry.received_bytes + chunk_len as u64 > MAX_PS_UPLOAD_BYTES {
                    return Err(ProtocolError::bad_request("upload exceeds size limit"));
                }
                append_upload_chunk(&entry.path, bytes)?;
                entry.next_seq += 1;
                entry.received_bytes += chunk_len as u64;
                entry.last_chunk_len = chunk_len;
                entry.last_touch = std::time::Instant::now();
            } else if entry.next_seq > 0 && seq == entry.next_seq - 1 {
                // Idempotent re-send of the previous chunk: length-checked,
                // never appended a second time.
                if chunk_len != entry.last_chunk_len {
                    return Err(ProtocolError::bad_request("resend length mismatch"));
                }
                entry.last_touch = std::time::Instant::now();
            } else {
                return Err(ProtocolError::bad_request("out-of-order chunk (hole)"));
            }
            entry.next_seq == entry.total_chunks
        }
        None => {
            if seq != 0 {
                return Err(ProtocolError::not_found("upload not found"));
            }
            let live = map
                .values()
                .filter(|e| e.owner_user_id == org.user_id && !e.consumed)
                .count();
            if live >= MAX_PS_UPLOADS_PER_USER {
                return Err(ProtocolError::bad_request(
                    "too many concurrent import uploads — finish or cancel one first",
                ));
            }
            if chunk_len as u64 > MAX_PS_UPLOAD_BYTES {
                return Err(ProtocolError::bad_request("upload exceeds size limit"));
            }
            let staging = crate::paths::project_studio_import_staging_dir();
            std::fs::create_dir_all(&staging)
                .map_err(|e| ProtocolError::internal(format!("staging dir: {e}")))?;
            // The on-disk name is always server-generated: a client filename
            // never reaches the filesystem.
            let path = staging.join(format!("psimp_{}", uuid::Uuid::new_v4()));
            std::fs::File::create(&path)
                .map_err(|e| ProtocolError::internal(format!("create staging file: {e}")))?;
            append_upload_chunk(&path, bytes)?;
            let entry = PsArchiveUpload {
                owner_user_id: org.user_id.clone(),
                path,
                total_chunks,
                next_seq: 1,
                received_bytes: chunk_len as u64,
                last_chunk_len: chunk_len,
                consumed: false,
                last_touch: std::time::Instant::now(),
            };
            let complete = entry.next_seq == entry.total_chunks;
            map.insert(upload_id.to_string(), entry);
            complete
        }
    };
    Ok(ps(ProjectStudioPayload::ProjectImportUploadChunkResponse {
        complete,
    }))
}

/// Path of an uploaded archive owned by the caller.
fn staged_archive_path(
    org: &OrgContext,
    upload_id: &str,
    require_unconsumed: bool,
) -> Result<std::path::PathBuf, ProtocolError> {
    let map = ps_uploads()
        .lock()
        .map_err(|_| ProtocolError::internal("upload registry poisoned"))?;
    match map.get(upload_id) {
        Some(entry) if entry.owner_user_id == org.user_id => {
            if require_unconsumed && entry.consumed {
                return Err(ProtocolError::bad_request("upload already consumed"));
            }
            Ok(entry.path.clone())
        }
        _ => Err(ProtocolError::not_found("upload not found")),
    }
}

fn project_import_preview_v1(
    ctx: &HandlerContext,
    upload_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_import_grant(ctx)?;
    let path = staged_archive_path(org, upload_id, false)?;
    // Reads the manifest ONLY: nothing is unpacked before the user confirms,
    // and an archive from a newer schema is refused right here.
    let manifest = archive::read_manifest(&path)
        .map_err(|e| ProtocolError::bad_request(format!("nie mozna odczytac archiwum: {e:#}")))?;
    let (vectors_reusable, vectors_reason) =
        archive::vectors_reusable(&ctx.state.db, &org.org_id, &manifest);
    Ok(ps(ProjectStudioPayload::ProjectImportPreviewResponse {
        archive_version: manifest.version,
        exported_at: manifest.exported_at,
        source_node_id: manifest.source_node_id,
        project_name: manifest.project.name,
        template: manifest.project.template,
        modules: manifest.project.modules,
        total_uncompressed_bytes: manifest.files.iter().map(|f| f.size).sum(),
        has_runs: manifest.options.include_runs && manifest.inventory.runs > 0,
        inventory: inventory_to_wire(&manifest.inventory),
        vectors_reusable,
        vectors_reason,
    }))
}

fn project_import_apply_v1(
    ctx: &HandlerContext,
    upload_id: &str,
    name_override: &str,
    import_vectors: bool,
    import_runs: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_import_grant(ctx)?;
    let path = staged_archive_path(org, upload_id, true)?;
    let manifest = archive::read_manifest(&path)
        .map_err(|e| ProtocolError::bad_request(format!("nie mozna odczytac archiwum: {e:#}")))?;
    let job_id = archive::spawn_import(archive::ImportTask {
        core_db: ctx.state.db.clone(),
        router: ctx.state.router.clone(),
        // The org is the caller's session org — NEVER the one in the archive.
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        archive_path: path,
        manifest,
        name_override: name_override.trim().to_string(),
        import_vectors,
        import_runs,
    })
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    // Consumed: the file survives for the running import, can no longer be
    // reused, and is reaped after the consumed TTL.
    if let Ok(mut map) = ps_uploads().lock() {
        if let Some(entry) = map.get_mut(upload_id) {
            entry.consumed = true;
            entry.last_touch = std::time::Instant::now();
        }
    }
    activity::record_org_security(
        &ctx.state.db,
        &ctx.state.local_node_id,
        &org.user_id,
        "project_studio.project.import_started",
        &job_id,
        upload_id,
    );
    Ok(ps(ProjectStudioPayload::ProjectImportApplyResponse {
        job_id,
    }))
}

fn project_import_status_v1(
    ctx: &HandlerContext,
    job_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let job = archive::job(job_id)
        .filter(|j| j.owner_user_id == org.user_id)
        .ok_or_else(|| ProtocolError::not_found("import job not found"))?;
    Ok(ps(ProjectStudioPayload::ProjectImportStatusResponse {
        job_id: job_id.to_string(),
        status: job.status,
        progress_pct: job.progress_pct,
        phase: job.phase,
        error: job.error,
        project_id: job.project_id,
        reindex_job_ids: job.reindex_job_ids,
        vectors_imported: job.vectors_imported,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn project_area_dispatch_enforces_functions_expiry_modules_and_task_creation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));
        let project_id = format!("gate-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join(&project_id);
        std::fs::create_dir_all(dir.join("files")).expect("project dir");
        let member = |user: &str, functions: &[&str]| MemberInput {
            user_id: user.to_string(),
            functions: functions.iter().map(|id| id.to_string()).collect(),
            project_admin: false,
            expires_at: None,
        };
        repository::create_project(
            &project_id,
            "org-gate",
            &format!("Access {project_id}"),
            "",
            "custom",
            "[\"knowledge\",\"tests\",\"tasks\",\"chat\"]",
            "owner-gate",
            &dir.to_string_lossy(),
            "",
            &[
                member("developer-gate", &["developer"]),
                member("tester-gate", &["tester"]),
                member("multi-gate", &["developer", "tester"]),
                member("empty-gate", &[]),
            ],
        )
        .expect("create project");
        std::mem::forget(tmp);
        let state = super::super::state::AppState::for_test();
        let instance =
            super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        super::super::app_gate::test_support::grant(
            &state,
            &instance,
            "app-admin-gate",
            PERM_ADMIN,
        );
        let ctx = |user: &str| HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0x21; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: user.to_string(),
                org_id: "org-gate".to_string(),
                role_id: "role-x".to_string(),
                permissions: Default::default(),
            }),
        };
        let get = || {
            ps(ProjectStudioPayload::ProjectGetRequest {
                project_id: project_id.clone(),
            })
        };
        let case = || {
            ps(ProjectStudioPayload::CaseSaveRequest {
                project_id: project_id.clone(),
                case_id: None,
                kind: "manual".into(),
                title: "Access case".into(),
                priority: "medium".into(),
                content_json: r#"{"steps":[{"action":"Open","expected":"Visible"}]}"#.into(),
                tag_ids: vec![],
                linked_source_ids: vec![],
                attachments_json: "[]".into(),
                expected_version: None,
                change_note: String::new(),
            })
        };
        let task = || {
            ps(ProjectStudioPayload::TaskSaveRequest {
                project_id: project_id.clone(),
                task_id: None,
                task_type: "technical".into(),
                title: "Access task".into(),
                description_md: String::new(),
                severity: String::new(),
                priority: "medium".into(),
                status: "todo".into(),
                assigned_to: String::new(),
                due_date: String::new(),
                links_json: "[]".into(),
                attachments_json: "[]".into(),
                parent_task_id: None,
            })
        };
        let denied = project_studio_dispatch(&case(), &ctx("developer-gate"))
            .await
            .expect_err("developer tests are read only");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        assert!(project_studio_dispatch(&case(), &ctx("tester-gate"))
            .await
            .is_ok());
        assert!(project_studio_dispatch(&case(), &ctx("multi-gate"))
            .await
            .is_ok());
        let catalogue_request = ps(ProjectStudioPayload::Access(
            ProjectAccessPayload::CatalogueGetRequest {
                project_id: project_id.clone(),
            },
        ));
        let owner_ctx = ctx("owner-gate");
        super::super::seed_session_account(&owner_ctx);
        let (catalogue_response, error) =
            super::super::dispatch(&catalogue_request, &owner_ctx).await;
        assert!(
            !error,
            "registered catalogue request failed: {catalogue_response:?}"
        );
        assert!(matches!(
            catalogue_response,
            MessageBody::ProjectStudioBody(ProjectStudioPayload::Access(
                ProjectAccessPayload::CatalogueGetResponse { .. }
            ))
        ));
        let response = project_studio_dispatch(&get(), &ctx("multi-gate"))
            .await
            .expect("get union");
        let MessageBody::ProjectStudioBody(ProjectStudioPayload::ProjectGetResponse { project }) =
            response
        else {
            panic!("project response")
        };
        assert!(project
            .access
            .allows(ProjectArea::Tests, ProjectPermissionLevel::Write));
        assert!(project
            .access
            .allows(ProjectArea::Repos, ProjectPermissionLevel::Write));
        assert!(project.my_role.is_none());
        assert!(
            project_studio_dispatch(&task(), &ctx("empty-gate"))
                .await
                .is_ok(),
            "any accessible member creates a task"
        );
        assert!(
            project_studio_dispatch(&task(), &ctx("app-admin-gate"))
                .await
                .is_ok(),
            "app inspection access also creates tasks"
        );
        let denied = project_studio_dispatch(&case(), &ctx("app-admin-gate"))
            .await
            .expect_err("app admin has no content write");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        let outsider = project_studio_dispatch(&get(), &ctx("outsider-gate"))
            .await
            .expect_err("outsider hidden");
        assert_eq!(outsider.code, ProtocolErrorCode::NotFound);
        let mut foreign = ctx("owner-gate");
        foreign.org_context.as_mut().unwrap().org_id = "foreign-gate".into();
        assert_eq!(
            project_studio_dispatch(&get(), &foreign)
                .await
                .expect_err("foreign hidden")
                .code,
            ProtocolErrorCode::NotFound
        );

        repository::set_member_access(&project_id, "owner-gate", &[], true, None)
            .expect("project admin without confidential functions");
        let function = ProjectFunctionWire {
            function_id: "custom_security".into(),
            name: "Explicit security".into(),
            description: String::new(),
            builtin: false,
            grants: ProjectArea::ALL
                .iter()
                .map(
                    |&area| tentaflow_protocol::project_studio::access::ProjectAreaGrantWire {
                        area,
                        level: if area == ProjectArea::SecurityConfidential {
                            ProjectPermissionLevel::Read
                        } else {
                            ProjectPermissionLevel::None
                        },
                    },
                )
                .collect(),
        };
        let save = || {
            ps(ProjectStudioPayload::Access(
                ProjectAccessPayload::FunctionSaveRequest {
                    project_id: project_id.clone(),
                    function: function.clone(),
                },
            ))
        };
        assert_eq!(
            project_studio_dispatch(&save(), &ctx("app-admin-gate"))
                .await
                .expect_err("real project admin required")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        assert!(
            project_studio_dispatch(&save(), &ctx("owner-gate"))
                .await
                .is_ok(),
            "project admin can explicitly grant confidentiality without a privilege ceiling"
        );
        let assign = ps(ProjectStudioPayload::Access(
            ProjectAccessPayload::MemberAccessSetRequest {
                project_id: project_id.clone(),
                user_id: "empty-gate".into(),
                functions: vec!["custom_security".into()],
                project_admin: false,
                expires_at: None,
            },
        ));
        assert!(project_studio_dispatch(&assign, &ctx("owner-gate"))
            .await
            .is_ok());
        let assigned = repository::member_access(&project_id, "empty-gate")
            .expect("member")
            .expect("assigned");
        let catalogue = repository::list_functions(&project_id).expect("catalogue");
        assert_eq!(
            crate::project_studio::models::function_level(
                &assigned.functions,
                &catalogue,
                ProjectArea::SecurityConfidential
            ),
            ProjectPermissionLevel::Read
        );
        let owner = repository::get_project("org-gate", &project_id)
            .expect("project")
            .expect("owner");
        let owner_access =
            repository::project_access(&owner, "owner-gate", false).expect("owner access");
        assert_eq!(
            owner_access.level(ProjectArea::SecurityConfidential),
            ProjectPermissionLevel::None,
            "future security module is disabled"
        );

        let legacy = ps(ProjectStudioPayload::MemberRoleSetRequest {
            project_id: project_id.clone(),
            user_id: "tester-gate".into(),
            role: "manager".into(),
        });
        assert_eq!(
            project_studio_dispatch(&legacy, &ctx("owner-gate"))
                .await
                .expect_err("legacy setter rejected")
                .code,
            ProtocolErrorCode::BadRequest
        );
        crate::project_studio::db::pool().expect("registry").write().expect("write")
            .execute("UPDATE project_members SET expires_at = '2000-01-01T00:00:00Z' WHERE project_id = ?1 AND user_id = 'tester-gate'", [&project_id]).expect("expire");
        assert_eq!(
            project_studio_dispatch(&get(), &ctx("tester-gate"))
                .await
                .expect_err("expired hidden")
                .code,
            ProtocolErrorCode::NotFound
        );
        assert_eq!(
            project_studio_dispatch(&case(), &ctx("tester-gate"))
                .await
                .expect_err("expired write hidden")
                .code,
            ProtocolErrorCode::NotFound
        );
        assert!(
            repository::member_access(&project_id, "tester-gate")
                .expect("expired record")
                .is_some(),
            "administration retains expired member"
        );
        for (user, function, grants) in [
            (
                "board-gate",
                "custom_board",
                vec![
                    (ProjectArea::Tasks, ProjectPermissionLevel::Read),
                    (ProjectArea::Board, ProjectPermissionLevel::Write),
                ],
            ),
            (
                "tasks-gate",
                "custom_tasks",
                vec![(ProjectArea::Tasks, ProjectPermissionLevel::Write)],
            ),
            (
                "board-read-gate",
                "custom_board_read",
                vec![(ProjectArea::Board, ProjectPermissionLevel::Read)],
            ),
        ] {
            repository::save_function(
                &project_id,
                &ProjectFunctionWire {
                    function_id: function.into(),
                    name: function.into(),
                    description: String::new(),
                    builtin: false,
                    grants: ProjectArea::ALL
                        .iter()
                        .map(|&area| {
                            tentaflow_protocol::project_studio::access::ProjectAreaGrantWire {
                                area,
                                level: grants
                                    .iter()
                                    .find(|(cell, _)| *cell == area)
                                    .map(|(_, level)| *level)
                                    .unwrap_or(ProjectPermissionLevel::None),
                            }
                        })
                        .collect(),
                },
            )
            .expect("custom task/board matrix");
            repository::add_members(
                &project_id,
                &[MemberInput {
                    user_id: user.into(),
                    functions: vec![function.into()],
                    project_admin: false,
                    expires_at: None,
                }],
                "owner-gate",
            )
            .expect("task/board member");
        }
        let created = project_studio_dispatch(&task(), &ctx("board-gate"))
            .await
            .expect("board user creates task");
        let MessageBody::ProjectStudioBody(ProjectStudioPayload::TaskSaveResponse {
            task_id, ..
        }) = created
        else {
            panic!("task response");
        };
        let status_request = ps(ProjectStudioPayload::TaskStatusSetRequest {
            project_id: project_id.clone(),
            task_id: task_id.clone(),
            status: "in_progress".into(),
        });
        assert!(project_studio_dispatch(&status_request, &ctx("board-gate"))
            .await
            .is_ok());
        assert!(project_studio_dispatch(&status_request, &ctx("tasks-gate"))
            .await
            .is_ok());
        let mut edit_request = task();
        if let MessageBody::ProjectStudioBody(ProjectStudioPayload::TaskSaveRequest {
            task_id: id,
            ..
        }) = &mut edit_request
        {
            *id = Some(task_id.clone());
        }
        assert_eq!(
            project_studio_dispatch(&edit_request, &ctx("board-gate"))
                .await
                .expect_err("board write cannot edit task fields")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        assert!(project_studio_dispatch(&edit_request, &ctx("tasks-gate"))
            .await
            .is_ok());
        assert!(project_studio_dispatch(
            &ps(ProjectStudioPayload::TaskGetRequest {
                project_id: project_id.clone(),
                task_id
            }),
            &ctx("board-read-gate")
        )
        .await
        .is_ok());
        let overview = project_studio_dispatch(
            &ps(ProjectStudioPayload::OverviewRequest {
                project_id: project_id.clone(),
            }),
            &ctx("board-read-gate"),
        )
        .await
        .expect("board overview");
        let MessageBody::ProjectStudioBody(ProjectStudioPayload::OverviewResponse {
            kpis,
            activity,
            ..
        }) = overview
        else {
            panic!("overview");
        };
        assert!(kpis.tasks_open > 0);
        assert!(activity.iter().any(|entry| entry.object_type == "task"));
        repository::update_project_modules(
            "org-gate",
            &project_id,
            "[\"tasks\",\"knowledge\",\"chat\"]",
        )
        .expect("disable tests");
        assert_eq!(
            project_studio_dispatch(&case(), &ctx("owner-gate"))
                .await
                .expect_err("admin cannot bypass disabled tests")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        repository::set_project_archived("org-gate", &project_id, true).expect("archive");
        assert!(project_studio_dispatch(&get(), &ctx("multi-gate"))
            .await
            .is_ok());
        assert_eq!(
            project_studio_dispatch(&task(), &ctx("owner-gate"))
                .await
                .expect_err("archive is read only")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
    }

    /// Archived projects are read-only: `require_active` (called by every
    /// mutation handler after the role gate) rejects with BadRequest and
    /// unarchiving restores mutability.
    #[test]
    fn archived_project_rejects_mutations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));

        let project_id = format!("arch-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join(&project_id);
        std::fs::create_dir_all(&dir).expect("project directory");
        project_db::open_pool_at(&dir).expect("real project storage");
        repository::create_project(
            &project_id,
            "org-a",
            &format!("Projekt {project_id}"),
            "",
            "custom",
            "[\"knowledge\"]",
            "owner-a",
            &dir.to_string_lossy(),
            "",
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
        std::mem::forget(tmp);
    }

    fn f3_pool() -> crate::db::DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn seed_environment(
        pool: &crate::db::DbPool,
        id: &str,
        base_url: &str,
        private: bool,
        secret_enc: &str,
    ) -> String {
        let address = environments::AddressClass {
            base_url: base_url.to_string(),
            host: url::Url::parse(base_url)
                .expect("url")
                .host_str()
                .expect("host")
                .to_string(),
            is_private: private,
        };
        environments::insert(
            pool,
            id,
            &environments::EnvironmentInput {
                name: id,
                env_type: "api",
                auth_type: "bearer",
                extra_headers_json: "{}",
                justification: "test",
                address: &address,
                host_allowlist: std::slice::from_ref(&address.host),
                secret_enc: Some(secret_enc),
            },
            "requester",
        )
        .expect("insert environment")
    }

    /// (a) The wire mapping never carries the environment secret: only the
    /// `has_secret` flag, and the encoded frame contains no ciphertext either.
    #[test]
    fn environment_wire_exposes_only_has_secret() {
        let pool = f3_pool();
        seed_environment(
            &pool,
            "env-secret",
            "https://example.com/",
            false,
            "enc:CIPHERTEXT",
        );
        seed_environment(&pool, "env-plain", "https://plain.example.com/", false, "");

        let wire = environments_to_wire(environments::list(&pool).expect("list"));
        let with_secret = wire
            .iter()
            .find(|e| e.environment_id == "env-secret")
            .expect("row");
        assert!(with_secret.has_secret);
        assert_eq!(with_secret.approval_status, "approved");
        let without = wire
            .iter()
            .find(|e| e.environment_id == "env-plain")
            .expect("row");
        assert!(!without.has_secret);

        let body = ps(ProjectStudioPayload::EnvironmentsListResponse { environments: wire });
        let bytes = tentaflow_protocol::cbor::encode(&body).expect("encode");
        let encoded = String::from_utf8_lossy(&bytes);
        assert!(
            !encoded.contains("CIPHERTEXT"),
            "the stored secret must never reach the wire"
        );
    }

    /// (e) An automated run refuses a pending or rejected environment; only an
    /// approved one passes.
    #[test]
    fn run_start_auto_requires_an_approved_environment() {
        let pool = f3_pool();
        seed_environment(&pool, "env-lan", "http://192.168.5.5:8080/", true, "");
        let err = require_approved_environment(&pool, "env-lan").expect_err("pending refused");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("not approved"), "{}", err.message);

        assert!(
            environments::decide(&pool, "env-lan", false, "brak zgody", "admin").expect("reject")
        );
        let err = require_approved_environment(&pool, "env-lan").expect_err("rejected refused");
        assert!(err.message.contains("rejected"), "{}", err.message);

        seed_environment(&pool, "env-pub", "https://example.com/", false, "");
        let ok = require_approved_environment(&pool, "env-pub").expect("approved passes");
        assert_eq!(ok.approval_status, "approved");

        let err = require_approved_environment(&pool, "env-missing").expect_err("unknown");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    /// (b) Case saves are validated against the contract of their kind: a code
    /// case without a script, an out-of-range perf profile and an unknown kind
    /// are all rejected; a well-formed case passes.
    #[test]
    fn case_save_validates_content_per_kind() {
        assert!(validate_case_fields("manual", "pl", "Tytul", "high", r#"{"steps":[]}"#).is_ok());
        assert!(validate_case_fields(
            "ui",
            "python",
            "Logowanie",
            "high",
            r#"{"script":"def test_x(page): pass"}"#
        )
        .is_ok());

        // Code kind without a script.
        let err = validate_case_fields("api", "python", "T", "high", "{}").expect_err("no script");
        assert!(err.message.contains("script"), "{}", err.message);
        // Perf profile outside the runner's accepted range.
        let err = validate_case_fields(
            "perf",
            "python",
            "T",
            "high",
            r#"{"script":"x","profile":{"users":999999}}"#,
        )
        .expect_err("users out of range");
        assert!(err.message.contains("profile.users"), "{}", err.message);
        // A language the runner cannot execute.
        let err = validate_case_fields("ui", "kotlin", "T", "high", r#"{"script":"x"}"#)
            .expect_err("unsupported language");
        assert!(err.message.contains("kotlin"), "{}", err.message);
        // Junk kind and junk content.
        assert!(validate_case_fields("wat", "pl", "T", "high", "{}").is_err());
        assert!(validate_case_fields("manual", "pl", "T", "high", "not json").is_err());
        assert!(validate_case_fields("manual", "pl", "T", "high", "[1,2]").is_err());
        assert!(validate_case_fields("manual", "pl", "", "high", "{}").is_err());
        assert!(validate_case_fields("manual", "pl", "T", "urgent", "{}").is_err());

        // The language is derived from the content for code kinds only.
        assert_eq!(case_language("manual", "{}"), "pl");
        assert_eq!(case_language("api", "{}"), "python");
        assert_eq!(case_language("api", r#"{"language":"Python"}"#), "python");
    }

    /// CR-005: `SourceUpdate` used to persist `config_json` verbatim, so an
    /// access token pasted into the edit form ended up in `sources.config_json`
    /// — a column every project viewer reads back. The token must move into the
    /// encrypted column, and a non-git source must not accept one at all.
    #[test]
    fn source_update_never_persists_a_token_in_the_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("project dir");

        let project_id = format!("src-{}", uuid::Uuid::new_v4());
        repository::create_project(
            &project_id,
            "org-s",
            &format!("Projekt {project_id}"),
            "",
            "custom",
            "[\"knowledge\"]",
            "owner-s",
            &dir.to_string_lossy(),
            "",
            &[MemberInput {
                user_id: "editor-s".to_string(),
                functions: vec!["developer".to_string(), "devops".to_string()],
                project_admin: false,
                expires_at: None,
            }],
        )
        .expect("create project");
        std::mem::forget(tmp);

        let pool = open_project_pool(&project_id).expect("project pool");
        repository::create_source(
            &pool,
            "src-git",
            "git",
            "Repo",
            r#"{"repo_url":"https://example.com/org/repo.git"}"#,
            "owner-s",
        )
        .expect("create git source");
        repository::create_source(
            &pool,
            "src-url",
            "url",
            "Strona",
            r#"{"url":"https://example.com/docs"}"#,
            "owner-s",
        )
        .expect("create url source");

        let state = crate::dispatch::state::AppState::for_test();
        super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let ctx = HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [7u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: "editor-s".to_string(),
                org_id: "org-s".to_string(),
                role_id: "role-x".to_string(),
                permissions: Default::default(),
            }),
        };

        source_update_v1(
            &ctx,
            &project_id,
            "src-git",
            "Repo",
            r#"{"repo_url":"https://example.com/org/repo.git","branch":"main","token":"ghp_topsecret"}"#,
        )
        .expect("git update");

        let stored = repository::get_source(&pool, "src-git")
            .expect("get")
            .expect("row");
        assert!(
            !stored.config_json.contains("ghp_topsecret"),
            "the token must not stay in config_json: {}",
            stored.config_json
        );
        assert!(!stored.config_json.contains("token"));
        assert!(stored.config_json.contains("repo_url"));
        let secret_enc = repository::get_source_secret_enc(&pool, "src-git").expect("secret");
        assert!(secret_enc.starts_with("enc:"), "token stored encrypted");
        assert_eq!(
            ctx.state
                .settings_cipher
                .decrypt(&secret_enc)
                .expect("decrypt"),
            "ghp_topsecret"
        );

        // A source kind that has no credential contract refuses the key
        // outright instead of storing it in the clear.
        let err = source_update_v1(
            &ctx,
            &project_id,
            "src-url",
            "Strona",
            r#"{"url":"https://example.com/docs","token":"ghp_topsecret"}"#,
        )
        .expect_err("url source refuses a token");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        let stored = repository::get_source(&pool, "src-url")
            .expect("get")
            .expect("row");
        assert!(!stored.config_json.contains("ghp_topsecret"));
    }

    /// The schedule loop reads `project_schedule_hints`, NOT the per-project
    /// databases: with a 16-pool cache, a tick that opened every project would
    /// thrash the LRU. So every save / toggle / delete has to leave the hint in
    /// sync, otherwise a schedule either never fires or keeps the project on the
    /// due list forever.
    #[test]
    fn schedule_writes_keep_the_registry_hint_in_sync() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));

        let project_id = format!("hint-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join(&project_id);
        std::fs::create_dir_all(dir.join("files")).expect("dir");
        project_db::open_pool_at(&dir).expect("project db");
        repository::create_project(
            &project_id,
            "org-h",
            &format!("Projekt {project_id}"),
            "",
            "tests",
            "[\"tests\"]",
            "owner-h",
            &dir.to_string_lossy(),
            "",
            &[],
        )
        .expect("create project");

        let state = crate::dispatch::state::AppState::for_test();
        super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let ctx = HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [9u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: "owner-h".to_string(),
                org_id: "org-h".to_string(),
                role_id: "role-x".to_string(),
                permissions: Default::default(),
            }),
        };

        let hint = || -> Option<(Option<String>, i64)> {
            let pool = crate::project_studio::db::pool().expect("registry");
            let conn = pool.read().expect("read");
            conn.query_row(
                "SELECT next_run_at, enabled_count FROM project_schedule_hints \
                 WHERE project_id = ?1",
                rusqlite::params![project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()
        };
        assert!(hint().is_none(), "no schedules, no hint yet");

        let wire = ScheduleWire {
            name: "Nocny smoke",
            run_type: "manual",
            suite_id: "",
            case_ids: &["c-nieistotne".to_string()],
            environment_id: "",
            runner_service_id: "",
            perf_profile_json: "",
            assignment_mode: "pool",
            assignees: &[],
            schedule_kind: "interval",
            schedule_expr: "1h",
            timezone: "Europe/Warsaw",
            enabled: true,
        };
        let saved = schedule_save_v1(&ctx, &project_id, None, &wire).expect("save");
        let MessageBody::ProjectStudioBody(ProjectStudioPayload::ScheduleSaveResponse {
            schedule_id,
            next_run_at,
            next_runs_preview,
        }) = saved
        else {
            panic!("unexpected save response");
        };
        assert!(!next_run_at.is_empty());
        assert_eq!(next_runs_preview.len(), 3, "the server renders the preview");
        let (hint_next, enabled_count) = hint().expect("hint written on save");
        assert_eq!(enabled_count, 1);
        assert_eq!(
            hint_next.as_deref(),
            Some(next_run_at.as_str()),
            "the hint carries the same instant the loop will compare against"
        );

        // Toggling off must clear the due instant, or the loop keeps opening the
        // project for a schedule that can no longer fire.
        schedule_set_enabled_v1(&ctx, &project_id, &schedule_id, false).expect("disable");
        let (hint_next, enabled_count) = hint().expect("hint after disable");
        assert!(hint_next.is_none());
        assert_eq!(enabled_count, 0);

        // Re-enabling recomputes it.
        schedule_set_enabled_v1(&ctx, &project_id, &schedule_id, true).expect("enable");
        let (hint_next, enabled_count) = hint().expect("hint after enable");
        assert!(hint_next.is_some());
        assert_eq!(enabled_count, 1);

        schedule_delete_v1(&ctx, &project_id, &schedule_id).expect("delete");
        let (hint_next, enabled_count) = hint().expect("hint after delete");
        assert!(
            hint_next.is_none(),
            "a deleted schedule leaves no due instant"
        );
        assert_eq!(enabled_count, 0);

        // A manual schedule must not bind an environment, and an automated one
        // requires an APPROVED environment at save time.
        let bad = ScheduleWire {
            environment_id: "e-nieznane",
            ..wire
        };
        assert_eq!(
            schedule_save_v1(&ctx, &project_id, None, &bad)
                .expect_err("manual schedule with an environment")
                .code,
            ProtocolErrorCode::BadRequest
        );
        let auto = ScheduleWire {
            run_type: "auto",
            environment_id: "e-nieznane",
            ..wire
        };
        assert_eq!(
            schedule_save_v1(&ctx, &project_id, None, &auto)
                .expect_err("unknown environment")
                .code,
            ProtocolErrorCode::BadRequest
        );
        // Firing-rule bounds are refused before anything is written.
        for (kind, expr) in [
            ("interval", "1m"),
            ("interval", "400d"),
            ("cron", "* * * * *"),
        ] {
            let invalid = ScheduleWire {
                schedule_kind: kind,
                schedule_expr: expr,
                ..wire
            };
            assert!(
                schedule_save_v1(&ctx, &project_id, None, &invalid).is_err(),
                "{kind} '{expr}' must be refused"
            );
        }
        // An unknown timezone would make the cron drift silently.
        let bad_zone = ScheduleWire {
            schedule_kind: "cron",
            schedule_expr: "30 2 * * *",
            timezone: "Mars/Olympus",
            ..wire
        };
        assert!(schedule_save_v1(&ctx, &project_id, None, &bad_zone).is_err());
        std::mem::forget(tmp);
    }
    #[tokio::test]
    async fn archived_project_export_start_and_status_produce_a_real_archive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));
        let id = format!("export-archived-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join(&id);
        std::fs::create_dir_all(dir.join("files")).expect("project dir");
        project_db::open_pool_at(&dir).expect("content db");
        repository::create_project(
            &id,
            "org-export",
            &id,
            "",
            "custom",
            "[\"knowledge\"]",
            "export-owner",
            &dir.to_string_lossy(),
            "",
            &[],
        )
        .expect("project");
        repository::set_project_archived("org-export", &id, true).expect("archive");
        let state = super::super::state::AppState::for_test();
        let instance =
            super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let ctx = HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0x45; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: "export-owner".into(),
                org_id: "org-export".into(),
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
        };
        super::super::app_gate::test_support::grant(
            &ctx.state,
            &instance,
            "export-app-admin",
            PERM_ADMIN,
        );
        let admin_ctx = HandlerContext {
            session: ctx.session.clone(),
            correlation_id: 2,
            connection_id: 0,
            resume_secret: None,
            state: ctx.state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: "export-app-admin".into(),
                org_id: "org-export".into(),
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
        };
        assert_eq!(
            project_export_start_v1(&admin_ctx, &id, false, false, false)
                .expect_err("app admin has no export entitlement")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        let response = project_studio_dispatch(
            &ps(ProjectStudioPayload::ProjectExportStartRequest {
                project_id: id.clone(),
                include_runs: false,
                include_vectors: false,
                include_user_names: false,
            }),
            &ctx,
        )
        .await
        .expect("archived export starts");
        let MessageBody::ProjectStudioBody(ProjectStudioPayload::ProjectExportStartResponse {
            job_id,
        }) = response
        else {
            panic!("export response")
        };
        assert_eq!(
            project_export_status_v1(&admin_ctx, &id, &job_id)
                .expect_err("app admin cannot download archived export")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let export_ref = loop {
            let response = project_studio_dispatch(
                &ps(ProjectStudioPayload::ProjectExportStatusRequest {
                    project_id: id.clone(),
                    job_id: job_id.clone(),
                }),
                &ctx,
            )
            .await
            .expect("archived export status");
            let MessageBody::ProjectStudioBody(ProjectStudioPayload::ProjectExportStatusResponse {
                status,
                error,
                export_ref,
                archive_bytes,
                ..
            }) = response
            else {
                panic!("status response")
            };
            if status == "success" {
                assert!(archive_bytes > 0);
                break export_ref;
            }
            assert_ne!(status, "failed", "export failed: {error}");
            assert!(tokio::time::Instant::now() < deadline, "export timed out");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let path = crate::api::project_studio_export::export_archive_path(&export_ref);
        let manifest = archive::read_manifest(&path).expect("real exported manifest");
        assert_eq!(manifest.project.project_id, id);
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "db/project.db"));
        std::fs::remove_file(path).expect("remove exported archive");
        std::mem::forget(tmp);
    }
    #[tokio::test]
    async fn ml_link_attach_enforces_the_existing_ml_project_organization() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&tmp.path().join("projects.db"));
        let _ = crate::ml_studio::db::init(&tmp.path().join("ml.db"));
        let id = format!("attach-org-{}", uuid::Uuid::new_v4());
        let org_id = format!("org-{id}");
        let owner = format!("owner-{id}");
        let dir = tmp.path().join(&id);
        std::fs::create_dir_all(dir.join("files")).expect("project dir");
        project_db::open_pool_at(&dir).expect("content db");
        repository::create_project(
            &id,
            &org_id,
            &id,
            "",
            "custom",
            "[\"knowledge\"]",
            &owner,
            &dir.to_string_lossy(),
            "",
            &[],
        )
        .expect("project");
        let foreign = crate::ml_studio::repository::create_project(
            &owner,
            "foreign-attach-org",
            &format!("Foreign {id}"),
            "",
            "recognition",
        )
        .expect("foreign ML project");
        let same_org = crate::ml_studio::repository::create_project(
            &owner,
            &org_id,
            &format!("Local {id}"),
            "",
            "recognition",
        )
        .expect("same organization ML project");
        let state = super::super::state::AppState::for_test();
        super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let ctx = HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0x64; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: owner,
                org_id,
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
        };
        let request = |ml_project_id: &str| {
            ps(ProjectStudioPayload::MlLinkAttachRequest {
                project_id: id.clone(),
                ml_project_id: ml_project_id.into(),
                label: "ML integration".into(),
                sync_permissions: false,
                role_map: vec![],
            })
        };
        assert_eq!(
            project_studio_dispatch(&request(&foreign.project.project_id), &ctx)
                .await
                .expect_err("foreign organization must remain hidden")
                .code,
            ProtocolErrorCode::NotFound,
        );
        let pool = open_project_pool(&id).expect("content");
        assert_eq!(ml_link::count(&pool).expect("links"), 0);
        assert!(matches!(
            project_studio_dispatch(&request(&same_org.project.project_id), &ctx)
                .await
                .expect("same organization owner attaches"),
            MessageBody::ProjectStudioBody(ProjectStudioPayload::MlLinkAttachResponse { .. }),
        ));
        assert_eq!(ml_link::count(&pool).expect("links"), 1);
        std::mem::forget(tmp);
    }
    fn access_fixture(
        modules: &str,
        functions: &[&str],
    ) -> (
        HandlerContext,
        crate::project_studio::models::ProjectRecord,
        crate::db::DbPool,
    ) {
        use crate::project_studio::models::MemberInput;
        use crate::services::rbac::middleware::OrgContext;
        let state = crate::dispatch::state::AppState::for_test();
        super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let root = tempfile::tempdir().expect("tempdir");
        let _ = crate::project_studio::db::init(&root.path().join("projects.db"));
        let project_id = format!("async-access-{}", uuid::Uuid::new_v4());
        let actor = format!("actor-{project_id}");
        let owner = format!("owner-{project_id}");
        let dir = root.path().join(&project_id);
        std::fs::create_dir_all(dir.join("files")).expect("project directory");
        repository::create_project(
            &project_id,
            "org-async-access",
            &project_id,
            "",
            "custom",
            modules,
            &owner,
            &dir.to_string_lossy(),
            "",
            &[MemberInput {
                user_id: actor.clone(),
                functions: functions
                    .iter()
                    .map(|function| function.to_string())
                    .collect(),
                project_admin: false,
                expires_at: None,
            }],
        )
        .expect("project");
        let pool = project_db::open(&project_id).expect("content");
        let record = repository::get_project("org-async-access", &project_id)
            .expect("project lookup")
            .expect("row");
        let ctx = HandlerContext {
            session: tentaflow_protocol::SessionAuth::UserSession {
                user_id: [0x7c; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            origin: crate::dispatch::RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: actor,
                org_id: "org-async-access".into(),
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
        };
        std::mem::forget(root);
        (ctx, record, pool)
    }

    #[tokio::test]
    async fn knowledge_dispatch_rechecks_access_after_a_real_delayed_embedding() {
        use crate::project_studio::ingest::access_test_support::{
            advertise_model, seed_passage, DelayedModel, ModelReply,
        };
        for change in ["expiry", "module"] {
            let (ctx, project, pool) = access_fixture("[\"knowledge\"]", &["developer"]);
            repository::create_source(
                &pool,
                "access-source",
                "document",
                "Private specification",
                "{}",
                &project.owner_user_id,
            )
            .expect("source");
            let vectors = crate::services::vector_namespace_manager(&ctx.state.db);
            seed_passage(
                &vectors,
                &project.org_id,
                &project.project_id,
                "PRIVATE-DISPATCH-PASSAGE",
            );
            assert_eq!(
                crate::project_studio::knowledge::search(
                    &vectors,
                    &project.org_id,
                    &project.project_id,
                    &[0.1, 0.2, 0.3],
                    &[],
                    10
                )
                .expect("positive retrieval")
                .len(),
                1
            );
            let mut http = DelayedModel::new(ingest::EMBEDDINGS_ALIAS, ModelReply::Embedding);
            advertise_model(
                &ctx.state,
                "embeddings",
                ingest::EMBEDDINGS_ALIAS,
                &http.endpoint,
            );
            let request = ps(ProjectStudioPayload::KbSearchRequest {
                project_id: project.project_id.clone(),
                query: "private specification".into(),
                source_ids: vec![],
                limit: 10,
            });
            let actor = ctx.org_context.as_ref().expect("org").user_id.clone();
            let search = tokio::spawn(async move { project_studio_dispatch(&request, &ctx).await });
            http.wait_request().await;
            if change == "expiry" {
                crate::project_studio::db::pool().expect("registry").write().expect("write").execute(
                    "UPDATE project_members SET expires_at='2000-01-01T00:00:00Z' WHERE project_id=?1 AND user_id=?2", rusqlite::params![project.project_id, actor],
                ).expect("expire during embedding");
            } else {
                repository::update_project_modules(&project.org_id, &project.project_id, "[]")
                    .expect("disable knowledge during embedding");
            }
            http.release();
            let error = search
                .await
                .expect("dispatch")
                .expect_err("current permission must prevent retrieval");
            assert_eq!(
                error.code,
                if change == "expiry" {
                    ProtocolErrorCode::NotFound
                } else {
                    ProtocolErrorCode::PolicyDenied
                }
            );
            assert!(!error.message.contains("PRIVATE-DISPATCH-PASSAGE"));
            http.finish();
        }
    }

    #[tokio::test]
    async fn automated_schedule_save_and_enable_require_current_environment_read() {
        use tentaflow_protocol::project_studio::access::{
            ProjectAreaGrantWire, ProjectFunctionWire,
        };
        let (ctx, project, pool) = access_fixture("[\"tests\"]", &["tester"]);
        let actor = &ctx.org_context.as_ref().expect("org").user_id;
        let mut function = ProjectFunctionWire {
            function_id: "test-scheduler".into(),
            name: "Test scheduler".into(),
            description: String::new(),
            builtin: false,
            grants: ProjectArea::ALL
                .iter()
                .map(|&area| ProjectAreaGrantWire {
                    area,
                    level: if area == ProjectArea::Tests {
                        ProjectPermissionLevel::Admin
                    } else {
                        ProjectPermissionLevel::None
                    },
                })
                .collect(),
        };
        repository::save_function(&project.project_id, &function).expect("custom matrix");
        repository::set_member_access(
            &project.project_id,
            actor,
            &[function.function_id.clone()],
            false,
            None,
        )
        .expect("scheduler");
        pool.write().expect("write").execute("INSERT INTO environments(environment_id,name,env_type,base_url,auth_type,approval_status,is_private_address,requested_by) \
            VALUES('schedule-env','Schedule environment','api','http://127.0.0.1:8090','none','approved',1,?1)", [&project.owner_user_id]).expect("approved environment");
        let request = |run_type: &str| {
            ps(ProjectStudioPayload::ScheduleSaveRequest {
                project_id: project.project_id.clone(),
                schedule_id: None,
                name: format!("{run_type} schedule"),
                run_type: run_type.into(),
                suite_id: String::new(),
                case_ids: vec!["schedule-case".into()],
                environment_id: if run_type == "manual" {
                    String::new()
                } else {
                    "schedule-env".into()
                },
                runner_service_id: String::new(),
                perf_profile_json: "{}".into(),
                assignment_mode: "pool".into(),
                assignees: vec![],
                schedule_kind: "interval".into(),
                schedule_expr: "1h".into(),
                timezone: "UTC".into(),
                enabled: false,
            })
        };
        assert!(
            project_studio_dispatch(&request("manual"), &ctx)
                .await
                .is_ok(),
            "manual schedule does not read an environment"
        );
        for run_type in ["auto", "perf"] {
            assert_eq!(
                project_studio_dispatch(&request(run_type), &ctx)
                    .await
                    .expect_err("save reads environment even when disabled")
                    .code,
                ProtocolErrorCode::PolicyDenied
            );
            function
                .grants
                .iter_mut()
                .find(|grant| grant.area == ProjectArea::Environments)
                .expect("environment cell")
                .level = ProjectPermissionLevel::Read;
            repository::save_function(&project.project_id, &function)
                .expect("environment read grant");
            let response = project_studio_dispatch(&request(run_type), &ctx)
                .await
                .expect("explicit environment read permits save");
            let MessageBody::ProjectStudioBody(ProjectStudioPayload::ScheduleSaveResponse {
                schedule_id,
                ..
            }) = response
            else {
                panic!("schedule response")
            };
            function
                .grants
                .iter_mut()
                .find(|grant| grant.area == ProjectArea::Environments)
                .expect("environment cell")
                .level = ProjectPermissionLevel::None;
            repository::save_function(&project.project_id, &function)
                .expect("revoke environment read");
            let toggle = |enabled| {
                ps(ProjectStudioPayload::ScheduleSetEnabledRequest {
                    project_id: project.project_id.clone(),
                    schedule_id: schedule_id.clone(),
                    enabled,
                })
            };
            assert_eq!(
                project_studio_dispatch(&toggle(true), &ctx)
                    .await
                    .expect_err("enable reads environment")
                    .code,
                ProtocolErrorCode::PolicyDenied
            );
            assert!(
                project_studio_dispatch(&toggle(false), &ctx).await.is_ok(),
                "disabling remains available to test administrators"
            );
            function
                .grants
                .iter_mut()
                .find(|grant| grant.area == ProjectArea::Environments)
                .expect("environment cell")
                .level = ProjectPermissionLevel::Read;
            repository::save_function(&project.project_id, &function)
                .expect("restore environment read");
            assert!(project_studio_dispatch(&toggle(true), &ctx).await.is_ok());
            function
                .grants
                .iter_mut()
                .find(|grant| grant.area == ProjectArea::Environments)
                .expect("environment cell")
                .level = ProjectPermissionLevel::None;
            repository::save_function(&project.project_id, &function)
                .expect("next scenario without environment grant");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_creation_rechecks_after_delayed_spec_preparation_before_any_rows() {
        use std::io::Write;
        for change in ["expiry", "archive"] {
            let (ctx, project, pool) = access_fixture("[\"knowledge\"]", &["developer"]);
            let actor = ctx.org_context.as_ref().expect("org").user_id.clone();
            let bytes = br#"{"openapi":"3.0.3","info":{"title":"Delayed API","version":"1.0"},"paths":{"/ping":{"get":{"responses":{"200":{"description":"OK"}}}}}}"#;
            let upload_id = uuid::Uuid::new_v4().to_string();
            let uploaded = ingest::accept_upload_chunk(&ingest::UploadChunk {
                org_id: &project.org_id,
                user_id: &actor,
                project_id: &project.project_id,
                dir_path: std::path::Path::new(&project.dir_path),
                upload_id: &upload_id,
                filename: "api.json",
                mime: "application/json",
                position: ingest::UploadPosition::Sequence {
                    seq: 0,
                    total_chunks: 1,
                },
                bytes,
                allowed: &|| Ok(()),
            })
            .expect("actual upload");
            assert!(uploaded.complete);
            let sha256 = uploaded.sha256;
            let blob = std::path::Path::new(&project.dir_path)
                .join("files")
                .join(&sha256);
            std::fs::remove_file(&blob).expect("prepare delayed blob read");
            assert!(std::process::Command::new("mkfifo")
                .arg(&blob)
                .status()
                .expect("fifo")
                .success());
            let (reading_tx, reading_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let writer_blob = blob.clone();
            let writer = std::thread::spawn(move || {
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(writer_blob)
                    .expect("wait for actual spec reader");
                reading_tx.send(()).expect("reader started");
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("release spec bytes");
                writer.write_all(bytes).expect("valid OpenAPI bytes");
            });
            let request = ps(ProjectStudioPayload::SourceCreateRequest {
                project_id: project.project_id.clone(),
                kind: "api_spec".into(),
                name: "Delayed API".into(),
                config_json: "{}".into(),
                file_refs: vec![sha256],
            });
            let creation =
                tokio::spawn(async move { project_studio_dispatch(&request, &ctx).await });
            tokio::task::spawn_blocking(move || {
                reading_rx.recv_timeout(std::time::Duration::from_secs(10))
            })
            .await
            .expect("reader signal")
            .expect("handler is preparing the real spec");
            if change == "expiry" {
                crate::project_studio::db::pool().expect("registry").write().expect("write").execute("UPDATE project_members SET expires_at='2000-01-01T00:00:00Z' WHERE project_id=?1 AND user_id=?2", rusqlite::params![project.project_id, actor]).expect("expire during preparation");
            } else {
                repository::set_project_archived(&project.org_id, &project.project_id, true)
                    .expect("archive during preparation");
            }
            release_tx.send(()).expect("complete real preparation");
            let error = creation
                .await
                .expect("source dispatch")
                .expect_err("revoked creation must refuse persistence");
            assert_eq!(
                error.code,
                if change == "expiry" {
                    ProtocolErrorCode::NotFound
                } else {
                    ProtocolErrorCode::PolicyDenied
                }
            );
            writer.join().expect("blob writer");
            std::fs::remove_file(blob).expect("remove FIFO");
            let conn = pool.read().expect("read");
            for table in ["sources", "source_files", "build_profiles", "ingest_jobs"] {
                let rows: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("no unauthorized rows");
                assert_eq!(rows, 0, "{change} must prevent {table} persistence");
            }
            let endpoints: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key LIKE 'api_spec_endpoints:%'",
                    [],
                    |row| row.get(0),
                )
                .expect("endpoint metadata");
            assert_eq!(
                endpoints, 0,
                "endpoint metadata is persisted only after current access"
            );
        }
    }

    #[tokio::test]
    async fn source_delete_rechecks_after_waiting_for_ingest_to_finish() {
        for change in ["expiry", "archive"] {
            let (ctx, project, pool) = access_fixture("[\"knowledge\"]", &["developer"]);
            let actor = ctx.org_context.as_ref().expect("org").user_id.clone();
            repository::create_source(
                &pool,
                "access-source",
                "document",
                "Private specification",
                "{}",
                &actor,
            )
            .expect("source");
            let job_id = format!("delete-wait-{}", uuid::Uuid::new_v4());
            repository::create_ingest_job(&pool, &job_id, "access-source", 1, &actor)
                .expect("running ingest row");
            let request = ps(ProjectStudioPayload::SourceDeleteRequest {
                project_id: project.project_id.clone(),
                source_id: "access-source".into(),
            });
            let mut deletion =
                tokio::spawn(async move { project_studio_dispatch(&request, &ctx).await });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut deletion)
                    .await
                    .is_err(),
                "running ingest holds the real deletion before its async boundary"
            );
            if change == "expiry" {
                crate::project_studio::db::pool().expect("registry").write().expect("write").execute("UPDATE project_members SET expires_at='2000-01-01T00:00:00Z' WHERE project_id=?1 AND user_id=?2", rusqlite::params![project.project_id, actor]).expect("expire while deletion waits");
            } else {
                repository::set_project_archived(&project.org_id, &project.project_id, true)
                    .expect("archive while deletion waits");
            }
            repository::finish_ingest_job(&pool, &job_id, "cancelled", "processing stopped")
                .expect("finish actual job row");
            let error = deletion
                .await
                .expect("delete dispatch")
                .expect_err("current grant must prevent deletion");
            assert_eq!(
                error.code,
                if change == "expiry" {
                    ProtocolErrorCode::NotFound
                } else {
                    ProtocolErrorCode::PolicyDenied
                }
            );
            assert!(repository::get_source(&pool, "access-source")
                .expect("source")
                .is_some());
        }
    }

    #[tokio::test]
    async fn project_delete_revokes_only_owned_ml_grants_and_preserves_provenance_on_failure() {
        use tentaflow_protocol::{MlStudioPayload, MlStudioProjectDetailRequest};
        let (member_ctx, project, pool) = access_fixture("[\"knowledge\"]", &["developer"]);
        let actor = member_ctx
            .org_context
            .as_ref()
            .expect("org")
            .user_id
            .clone();
        let owner = project.owner_user_id.clone();
        let core = crate::db::global_pool().expect("core directory");
        core.write().expect("write").execute("INSERT OR IGNORE INTO user_accounts(id,username,password_hash,display_name,role,is_active) VALUES(?1,?1,'','ML owner','user',1)", [&owner]).expect("real owner identity");
        let ml_path = std::path::Path::new(&project.dir_path)
            .parent()
            .expect("root")
            .join("ml.db");
        let _ = crate::ml_studio::db::init(&ml_path).expect("ML database");
        let (link_id, ml_id, _, _) = ml_link::create_from_project(
            &pool,
            &project.project_id,
            &project.org_id,
            &owner,
            &project.name,
            "recognition",
            "Owned ML integration",
            true,
            &ml_link::default_role_map(),
        )
        .expect("real mirrored ML project");
        let manual = format!("manual-{}", uuid::Uuid::new_v4());
        crate::ml_studio::repository::invite_member(&ml_id, &owner, &manual, "editor")
            .expect("independent manual ML grant");
        super::super::app_gate::test_support::install_app(
            &member_ctx.state,
            "ml-studio",
            &["mlstudio.read"],
        );
        let request = MessageBody::MlStudioBody(MlStudioPayload::ProjectDetailRequest(
            MlStudioProjectDetailRequest {
                project_id: ml_id.clone(),
            },
        ));
        assert!(
            super::super::ml_studio::ml_studio_project_detail(&request, &member_ctx).is_ok(),
            "mirrored member can read before project deletion"
        );
        let owner_ctx = HandlerContext {
            org_context: Some(OrgContext {
                user_id: owner.clone(),
                org_id: project.org_id.clone(),
                role_id: "role-test".into(),
                permissions: Default::default(),
            }),
            session: member_ctx.session.clone(),
            correlation_id: 2,
            connection_id: 0,
            resume_secret: None,
            state: member_ctx.state.clone(),
            origin: crate::dispatch::RequestOrigin::Local,
        };
        let deletion = ps(ProjectStudioPayload::ProjectDeleteRequest {
            project_id: project.project_id.clone(),
        });
        let ml_db = crate::ml_studio::db::pool().expect("ML pool");
        ml_db.write().expect("write").execute_batch(&format!("CREATE TRIGGER reject_owned_revoke BEFORE DELETE ON project_members WHEN OLD.project_id='{ml_id}' AND OLD.user_id='{actor}' BEGIN SELECT RAISE(ABORT,'owned revoke unavailable'); END;")).expect("inject actual external revoke failure");
        let failure = project_studio_dispatch(&deletion, &owner_ctx)
            .await
            .expect_err("failed revoke must stop destructive project deletion");
        assert_eq!(failure.code, ProtocolErrorCode::Internal);
        assert_eq!(failure.message, "project studio database error");
        assert!(std::path::Path::new(&project.dir_path)
            .join("project.db")
            .is_file());
        assert!(
            repository::get_project(&project.org_id, &project.project_id)
                .expect("project")
                .is_some()
        );
        assert!(ml_link::get(&pool, &link_id).expect("link").is_some());
        assert_eq!(
            repository::ml_grant_origins(&ml_id, &actor).expect("retained provenance"),
            vec![(project.project_id.clone(), link_id)]
        );
        ml_db
            .write()
            .expect("write")
            .execute_batch("DROP TRIGGER reject_owned_revoke;")
            .expect("restore external revoke availability");
        assert!(matches!(
            project_studio_dispatch(&deletion, &owner_ctx)
                .await
                .expect("delete after actual revocation"),
            MessageBody::ProjectStudioBody(ProjectStudioPayload::ProjectDeleteResult { ok: true })
        ));
        assert!(!std::path::Path::new(&project.dir_path).exists());
        assert!(
            repository::get_project(&project.org_id, &project.project_id)
                .expect("deleted project")
                .is_none()
        );
        assert!(repository::ml_grant_origins(&ml_id, &actor)
            .expect("revoked provenance")
            .is_empty());
        assert_eq!(
            super::super::ml_studio::ml_studio_project_detail(&request, &member_ctx)
                .expect_err("former mirror cannot read directly after deletion")
                .code,
            ProtocolErrorCode::NotFound
        );
        ml_link::restore_grant_index().expect("real startup provenance restore after deletion");
        assert_eq!(
            super::super::ml_studio::ml_studio_project_detail(&request, &member_ctx)
                .expect_err("restart cannot restore deleted project's access")
                .code,
            ProtocolErrorCode::NotFound
        );
        assert_eq!(
            crate::ml_studio::repository::member_role(&ml_id, &manual)
                .expect("manual membership")
                .as_deref(),
            Some("editor")
        );
        assert_eq!(
            crate::ml_studio::repository::member_role(&ml_id, &owner)
                .expect("ML owner")
                .as_deref(),
            Some("owner")
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn source_refresh_rechecks_after_a_real_delayed_git_fetch_before_sync() {
        use std::os::unix::fs::PermissionsExt;
        let git = |directory: &std::path::Path, arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(arguments)
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        for change in ["expiry", "archive"] {
            let (ctx, project, pool) = access_fixture("[\"knowledge\"]", &["developer", "devops"]);
            let actor = ctx.org_context.as_ref().expect("org").user_id.clone();
            let upstream = std::path::Path::new(&project.dir_path).join("upstream");
            std::fs::create_dir_all(&upstream).expect("upstream");
            git(&upstream, &["init", "--initial-branch=main"]);
            git(&upstream, &["config", "user.name", "Project fixture"]);
            git(
                &upstream,
                &["config", "user.email", "project-fixture@example.invalid"],
            );
            std::fs::write(
                upstream.join("specification.py"),
                "# Initial indexed specification
",
            )
            .expect("initial file");
            git(&upstream, &["add", "specification.py"]);
            git(&upstream, &["commit", "-m", "Create initial specification"]);
            let source_id = format!("refresh-{}", uuid::Uuid::new_v4());
            let checkout =
                git_source::source_dir(&project.project_id, &source_id).expect("checkout path");
            std::fs::create_dir_all(checkout.parent().expect("sources root"))
                .expect("sources root");
            let cloned = std::process::Command::new("git")
                .args(["clone", "--branch", "main"])
                .arg(&upstream)
                .arg(&checkout)
                .output()
                .expect("local clone");
            assert!(
                cloned.status.success(),
                "{}",
                String::from_utf8_lossy(&cloned.stderr)
            );
            let public_url = "https://1.1.1.1/project-access-fixture.git";
            git(&checkout, &["remote", "set-url", "origin", public_url]);
            let rewritten = format!("url.file://{}.insteadOf", upstream.to_string_lossy());
            git(&checkout, &["config", &rewritten, public_url]);
            repository::create_source(
                &pool,
                &source_id,
                "git",
                "Delayed repository",
                &serde_json::json!({"repo_url":public_url,"branch":"main"}).to_string(),
                &actor,
            )
            .expect("source");
            let collected =
                ingest::collect_tree_files(&checkout, std::path::Path::new(&project.dir_path))
                    .expect("actual initial files");
            let delta = ingest::TreeDelta {
                added: collected.iter().map(|file| file.rel_path.clone()).collect(),
                ..Default::default()
            };
            repository::sync_tree_files(&pool, &source_id, &collected, &delta)
                .expect("initial stored files");
            let initial =
                repository::files_for_ingest(&pool, &source_id, None).expect("initial rows");
            assert_eq!(initial.len(), 1);
            let initial_sha = initial[0].sha256.clone();
            std::fs::write(
                upstream.join("specification.py"),
                "# Changed repository content must not reach the revoked knowledge index
",
            )
            .expect("updated file");
            git(&upstream, &["add", "specification.py"]);
            git(&upstream, &["commit", "-m", "Update specification"]);
            // A fetched checkout can contain a newer revision than the ingested index.
            git(
                &checkout,
                &[
                    "pull",
                    "--ff-only",
                    upstream.to_str().expect("upstream path"),
                    "main",
                ],
            );
            let entered = std::path::Path::new(&project.dir_path).join("git-fetch-entered");
            let release = std::path::Path::new(&project.dir_path).join("git-fetch-release");
            let upload_pack =
                std::path::Path::new(&project.dir_path).join("delayed-upload-pack.sh");
            std::fs::write(&upload_pack, format!("#!/bin/sh\n: > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.02; done\nexec git upload-pack \"$@\"\n", entered.display(), release.display())).expect("delayed real upload pack");
            std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o700))
                .expect("upload pack permissions");
            git(
                &checkout,
                &["config", &format!("remote.{public_url}.url"), public_url],
            );
            git(
                &checkout,
                &[
                    "config",
                    &format!("remote.{public_url}.uploadpack"),
                    upload_pack.to_str().expect("script path"),
                ],
            );
            let request = ps(ProjectStudioPayload::SourceRefreshRequest {
                project_id: project.project_id.clone(),
                source_id: source_id.clone(),
            });
            let refresh =
                tokio::spawn(async move { project_studio_dispatch(&request, &ctx).await });
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            while !entered.is_file() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "real Git fetch did not enter the controlled upload-pack boundary"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            if change == "expiry" {
                crate::project_studio::db::pool().expect("registry").write().expect("write").execute("UPDATE project_members SET expires_at='2000-01-01T00:00:00Z' WHERE project_id=?1 AND user_id=?2", rusqlite::params![project.project_id, actor]).expect("expire during real fetch");
            } else {
                repository::set_project_archived(&project.org_id, &project.project_id, true)
                    .expect("archive during real fetch");
            }
            std::fs::write(&release, "complete actual upload-pack").expect("release actual fetch");
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), refresh)
                .await
                .expect("actual fetch completion")
                .expect("refresh dispatch")
                .expect_err("post-fetch grant must prevent persistent synchronization");
            assert_eq!(
                error.code,
                if change == "expiry" {
                    ProtocolErrorCode::NotFound
                } else {
                    ProtocolErrorCode::PolicyDenied
                }
            );
            let after =
                repository::files_for_ingest(&pool, &source_id, None).expect("stored files");
            assert_eq!(after.len(), 1);
            assert_eq!(
                after[0].sha256, initial_sha,
                "revoked fetch may not replace the indexed file version"
            );
            let jobs: i64 = pool
                .read()
                .expect("read")
                .query_row("SELECT COUNT(*) FROM ingest_jobs", [], |row| row.get(0))
                .expect("jobs");
            assert_eq!(jobs, 0);
            assert!(
                std::fs::read_to_string(checkout.join("specification.py"))
                    .expect("actual updated checkout")
                    .starts_with("# Changed repository"),
                "the fetched checkout differs from the index before the authorization refusal"
            );
            git_source::remove_source_dir(&project.project_id, &source_id);
        }
    }
}

#[cfg(test)]
mod p1_tests {
    use super::*;
    use sha2::Digest;

    struct Fixture {
        project: ProjectRecord,
        pool: crate::db::DbPool,
        contexts: Vec<HandlerContext>,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("tempdir");
        crate::project_studio::db::init(&root.path().join("projects.db")).expect("registry");
        let state = crate::dispatch::AppState::for_test();
        super::super::app_gate::test_support::install_app(&state, PACKAGE_ID, &[PERM_READ]);
        let org_id = crate::services::org::DEFAULT_ORG_ID;
        let mut users = Vec::new();
        for name in ["owner", "developer", "observer", "empty"] {
            let user = crate::db::repository::create_user_account(
                &state.db,
                name,
                "hash",
                name,
                &format!("{name}@example.test"),
            )
            .expect("actual account");
            crate::services::org::add_membership(
                &state.db,
                org_id,
                &user,
                "role-org-viewer",
                "test",
            )
            .expect("actual organization membership");
            state
                .db
                .write()
                .expect("writer")
                .execute(
                    "UPDATE user_accounts SET must_change_password = 0 WHERE id = ?1",
                    [&user],
                )
                .expect("active session");
            users.push(user);
        }
        let project_id = format!("p1-{}", uuid::Uuid::new_v4());
        let dir = root.path().join(&project_id);
        std::fs::create_dir_all(dir.join("files")).expect("directory");
        repository::create_project(
            &project_id,
            org_id,
            &project_id,
            "",
            "custom",
            "[\"tasks\",\"tests\",\"knowledge\"]",
            &users[0],
            &dir.to_string_lossy(),
            "",
            &[
                MemberInput {
                    user_id: users[1].clone(),
                    functions: vec!["developer".into()],
                    project_admin: false,
                    expires_at: None,
                },
                MemberInput {
                    user_id: users[2].clone(),
                    functions: vec!["observer".into()],
                    project_admin: false,
                    expires_at: None,
                },
                MemberInput {
                    user_id: users[3].clone(),
                    functions: vec![],
                    project_admin: false,
                    expires_at: None,
                },
            ],
        )
        .expect("project");
        let contexts = users
            .iter()
            .map(|user| HandlerContext {
                session: tentaflow_protocol::SessionAuth::UserSession {
                    user_id: *uuid::Uuid::parse_str(user).expect("UUID").as_bytes(),
                    role: None,
                },
                correlation_id: 1,
                connection_id: 0,
                resume_secret: None,
                state: state.clone(),
                origin: crate::dispatch::RequestOrigin::Local,
                org_context: Some(
                    crate::services::rbac::resolve_org_context(&state.db, user, Some(org_id))
                        .expect("actual org context"),
                ),
            })
            .collect();
        let project = repository::get_project(org_id, &project_id)
            .expect("lookup")
            .expect("record");
        let pool = project_db::open(&project_id).expect("project pool");
        std::mem::forget(root);
        Fixture {
            project,
            pool,
            contexts,
        }
    }

    async fn call(
        ctx: &HandlerContext,
        request: ProjectStudioPayload,
    ) -> Result<ProjectStudioPayload, ProtocolError> {
        let (response, error) = super::super::dispatch(&ps(request), ctx).await;
        match response {
            MessageBody::ProjectStudioBody(payload) if !error => Ok(payload),
            MessageBody::Error(error) => Err(error),
            other => panic!("unexpected real dispatch response {other:?}"),
        }
    }

    fn save(
        f: &Fixture,
        task_id: Option<String>,
        assigned_to: &str,
        status: &str,
        attachments: &str,
    ) -> ProjectStudioPayload {
        ProjectStudioPayload::TaskSaveRequest {
            project_id: f.project.project_id.clone(),
            task_id,
            task_type: "technical".into(),
            title: "Actual P1 task".into(),
            description_md: "Real task history".into(),
            severity: String::new(),
            priority: "medium".into(),
            status: status.into(),
            assigned_to: assigned_to.into(),
            due_date: String::new(),
            parent_task_id: None,
            links_json: "[]".into(),
            attachments_json: attachments.into(),
        }
    }

    fn user(ctx: &HandlerContext) -> &str {
        &ctx.org_context.as_ref().expect("org").user_id
    }

    async fn create(f: &Fixture, assigned: &str, attachments: &str) -> String {
        let result = call(&f.contexts[0], save(f, None, assigned, "todo", attachments))
            .await
            .expect("real task create");
        let ProjectStudioPayload::TaskSaveResponse {
            task_id,
            task_key,
            event_ids,
            ..
        } = result
        else {
            panic!("task")
        };
        assert!(task_key.starts_with(&format!("{}-", f.project.key_prefix)));
        assert!(!event_ids.is_empty());
        task_id
    }

    async fn bell(ctx: &HandlerContext) -> Vec<NotificationWire> {
        let result = call(
            ctx,
            ProjectStudioPayload::NotificationsListRequest {
                only_unread: false,
                before_id: None,
                limit: 100,
            },
        )
        .await
        .expect("actual private notification list");
        let ProjectStudioPayload::NotificationsListResponse { notifications, .. } = result else {
            panic!("bell")
        };
        notifications
    }

    #[tokio::test]
    async fn real_dispatch_task_events_mentions_noops_and_private_bell_follow_current_access() {
        let f = fixture();
        let actor = &f.contexts[0];
        let assigned = user(&f.contexts[1]);
        let observer = user(&f.contexts[2]);
        let task_id = create(&f, assigned, "[]").await;
        assert_eq!(
            bell(&f.contexts[1])
                .await
                .iter()
                .filter(|n| n.kind == "task_assigned")
                .count(),
            1
        );
        let status = || ProjectStudioPayload::TaskStatusSetRequest {
            project_id: f.project.project_id.clone(),
            task_id: task_id.clone(),
            status: "in_progress".into(),
        };
        let first = call(actor, status()).await.expect("transition");
        let ProjectStudioPayload::TaskStatusSetResult {
            event_id: Some(event_id),
            previous_status,
            ..
        } = first
        else {
            panic!("real transition ID")
        };
        assert_eq!(previous_status.as_deref(), Some("todo"));
        let before = tasks::list_task_events(&f.pool, &task_id, None, 100)
            .expect("events")
            .0;
        assert!(before.iter().any(|e| e.event_id == event_id
            && e.actor_id == user(actor)
            && e.kind == "status_changed"));
        assert!(matches!(
            call(actor, status()).await.expect("status no-op"),
            ProjectStudioPayload::TaskStatusSetResult { event_id: None, .. }
        ));
        assert_eq!(
            tasks::list_task_events(&f.pool, &task_id, None, 100)
                .expect("events")
                .0
                .len(),
            before.len()
        );
        assert_eq!(
            bell(&f.contexts[1])
                .await
                .iter()
                .filter(|n| n.kind == "task_status_changed")
                .count(),
            1
        );
        assert!(
            bell(actor).await.is_empty(),
            "actor receives no own notifications"
        );
        let comment = call(
            actor,
            ProjectStudioPayload::TaskCommentAddRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                body_md: "Please inspect this transition".into(),
                mention_user_ids: vec![observer.into(), observer.into(), user(actor).into()],
            },
        )
        .await
        .expect("structured mentions");
        let ProjectStudioPayload::TaskCommentAddResponse { comment } = comment else {
            panic!("comment")
        };
        assert_eq!(comment.mention_user_ids, vec![observer]);
        assert_eq!(
            bell(&f.contexts[2])
                .await
                .iter()
                .filter(|n| n.kind == "task_mentioned")
                .count(),
            1
        );
        call(
            actor,
            ProjectStudioPayload::TaskCommentEditRequest {
                project_id: f.project.project_id.clone(),
                comment_id: comment.comment_id.clone(),
                body_md: comment.body_md.clone(),
                mention_user_ids: comment.mention_user_ids.clone(),
            },
        )
        .await
        .expect("comment no-op");
        assert_eq!(bell(&f.contexts[2]).await.len(), 1);
        let denied = call(
            actor,
            ProjectStudioPayload::TaskCommentAddRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                body_md: "Invalid mention".into(),
                mention_user_ids: vec![user(&f.contexts[3]).into()],
            },
        )
        .await
        .expect_err("no task read is not an eligible recipient");
        assert_eq!(denied.code, ProtocolErrorCode::BadRequest);
        let stored_notifications = notifications::list(observer, false, None, 100)
            .expect("stored notification history")
            .0
            .len();
        assert_eq!(stored_notifications, 1);
        repository::set_member_access(
            &f.project.project_id,
            observer,
            &["developer".into()],
            false,
            None,
        )
        .expect("next assignee has current task write");
        let reassigned = call(
            actor,
            save(&f, Some(task_id.clone()), observer, "in_progress", "[]"),
        )
        .await
        .expect("ordinary full-save reassignment");
        let ProjectStudioPayload::TaskSaveResponse { event_ids, .. } = reassigned else {
            panic!("save result");
        };
        let assignment_event = tasks::list_task_events(&f.pool, &task_id, None, 100)
            .expect("actual events")
            .0
            .into_iter()
            .find(|event| event_ids.contains(&event.event_id) && event.kind == "reassigned")
            .expect("committed reassignment event");
        let old_bell = bell(&f.contexts[1]).await;
        let new_bell = bell(&f.contexts[2]).await;
        for (rows, kind) in [(&old_bell, "task_reassigned"), (&new_bell, "task_assigned")] {
            let notices: Vec<_> = rows
                .iter()
                .filter(|notification| notification.kind == kind)
                .collect();
            assert_eq!(
                notices.len(),
                1,
                "each side receives its own assignment notification once"
            );
            let link: serde_json::Value =
                serde_json::from_str(&notices[0].link_json).expect("link facts");
            assert_eq!(link["event_id"], assignment_event.event_id);
            assert_eq!(link["from_user_id"], assigned);
            assert_eq!(link["to_user_id"], observer);
            assert_eq!(link["task_title"], "Actual P1 task");
        }
        assert!(
            bell(actor).await.is_empty(),
            "reassigning actor remains excluded"
        );
        let unchanged = call(
            actor,
            save(&f, Some(task_id.clone()), observer, "in_progress", "[]"),
        )
        .await
        .expect("ordinary reassignment no-op");
        assert!(
            matches!(unchanged, ProjectStudioPayload::TaskSaveResponse { event_ids, .. } if event_ids.is_empty())
        );
        assert_eq!(bell(&f.contexts[1]).await.len(), old_bell.len());
        assert_eq!(bell(&f.contexts[2]).await.len(), new_bell.len());
        let unassigned = call(
            actor,
            save(&f, Some(task_id.clone()), "", "in_progress", "[]"),
        )
        .await
        .expect("actual unassignment");
        let ProjectStudioPayload::TaskSaveResponse { event_ids, .. } = unassigned else {
            panic!("save result");
        };
        let unassignment_event = tasks::list_task_events(&f.pool, &task_id, None, 100)
            .expect("actual events")
            .0
            .into_iter()
            .find(|event| event_ids.contains(&event.event_id) && event.kind == "unassigned")
            .expect("committed unassignment event");
        let after_unassignment = bell(&f.contexts[2]).await;
        let notice = after_unassignment
            .iter()
            .find(|notification| notification.kind == "task_unassigned")
            .expect("previous assignee is notified");
        let link: serde_json::Value = serde_json::from_str(&notice.link_json).expect("link facts");
        assert_eq!(link["event_id"], unassignment_event.event_id);
        assert_eq!(link["from_user_id"], observer);
        assert_eq!(link["to_user_id"], "");
        assert_eq!(
            bell(&f.contexts[1]).await.len(),
            old_bell.len(),
            "an unrelated previous assignee receives no unassignment notification"
        );
        call(
            actor,
            save(&f, Some(task_id.clone()), "", "in_progress", "[]"),
        )
        .await
        .expect("unassignment no-op");
        assert_eq!(bell(&f.contexts[2]).await.len(), after_unassignment.len());
        repository::set_member_access(&f.project.project_id, observer, &[], false, None)
            .expect("revoke read");
        assert!(
            bell(&f.contexts[2]).await.is_empty(),
            "stored title and body are hidden after grant revocation"
        );
        crate::project_studio::db::pool().expect("registry").write().expect("writer").execute("UPDATE project_members SET expires_at = '2000-01-01T00:00:00Z' WHERE project_id = ?1 AND user_id = ?2", rusqlite::params![f.project.project_id, assigned]).expect("expire recipient");
        assert!(
            bell(&f.contexts[1]).await.is_empty(),
            "expired membership hides old assignment and transition details"
        );
        assert!(
            notifications::list(assigned, false, None, 100)
                .expect("stored history")
                .0
                .len()
                >= 2
        );
    }

    #[tokio::test]
    async fn real_binary_notification_push_rechecks_queued_task_visibility_after_revoke() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let f = fixture();
        let task_id = create(&f, "", "[]").await;
        let target = user(&f.contexts[2]).to_string();
        let (server_stream, client_stream) = tokio::io::duplex(1024 * 1024);
        let state = f.contexts[2].state.clone();
        let target_for_socket = target.clone();
        let server = tokio::spawn(crate::api::dashboard::ws_binary::handle_ws_connection(
            server_stream,
            Some(target_for_socket),
            Some("user".into()),
            std::sync::Arc::new(vec![1; 32]),
            state,
            "127.0.0.1".into(),
        ));
        let mut client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client_stream,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let body = tentaflow_protocol::cbor::encode(&MessageBody::MetaSchemaVersionCheck {
            client_version: tentaflow_protocol::envelope::SCHEMA_VERSION,
        })
        .expect("body");
        let envelope = tentaflow_protocol::Envelope::new_direct(
            1,
            1,
            tentaflow_protocol::envelope::message_kind::META_HEARTBEAT,
            body,
        );
        client
            .send(Message::Binary(
                tentaflow_protocol::cbor::encode(&envelope)
                    .expect("binary handshake")
                    .into(),
            ))
            .await
            .expect("send");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let message = client.next().await.expect("socket").expect("frame");
                if let Message::Binary(bytes) = message {
                    let envelope: tentaflow_protocol::Envelope =
                        tentaflow_protocol::cbor::decode(&bytes).expect("envelope");
                    let body: MessageBody =
                        tentaflow_protocol::cbor::decode(&envelope.body).expect("body");
                    if matches!(
                        body,
                        MessageBody::MetaSchemaVersionAck { accepted: true, .. }
                    ) {
                        break;
                    }
                }
            }
        })
        .await
        .expect("handshake confirms live pump");
        task_comment_add_v1(
            &f.contexts[0],
            &f.project.project_id,
            &task_id,
            "First actual queued mention",
            &[target.clone()],
        )
        .expect("actual committed notification");
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let message = client.next().await.expect("socket").expect("frame");
                if let Message::Binary(bytes) = message {
                    let envelope: tentaflow_protocol::Envelope =
                        tentaflow_protocol::cbor::decode(&bytes).expect("envelope");
                    let body: MessageBody =
                        tentaflow_protocol::cbor::decode(&envelope.body).expect("body");
                    if let MessageBody::SystemEventBody(
                        tentaflow_protocol::SystemEventPayload::UserNotification {
                            user_id,
                            kind,
                            link_json,
                            ..
                        },
                    ) = body
                    {
                        break (user_id, kind, link_json);
                    }
                }
            }
        })
        .await
        .expect("authorized actual push");
        assert_eq!(first.0, target);
        assert_eq!(first.1, "task_mentioned");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first.2).expect("actual metadata")
                ["task_title"],
            "Actual P1 task"
        );
        task_comment_add_v1(
            &f.contexts[0],
            &f.project.project_id,
            &task_id,
            "Queued before access revocation",
            &[target.clone()],
        )
        .expect("second committed notification");
        repository::set_member_access(&f.project.project_id, &target, &[], false, None)
            .expect("revoke before pump resumes on this current-thread runtime");
        crate::dispatch::system_event_broadcast::publish_service_status(
            "p1-sentinel",
            "test",
            "running",
            "after queued private notification",
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let message = client.next().await.expect("socket").expect("frame");
                if let Message::Binary(bytes) = message {
                    let envelope: tentaflow_protocol::Envelope =
                        tentaflow_protocol::cbor::decode(&bytes).expect("envelope");
                    let body: MessageBody =
                        tentaflow_protocol::cbor::decode(&envelope.body).expect("body");
                    match body {
                        MessageBody::SystemEventBody(
                            tentaflow_protocol::SystemEventPayload::UserNotification { .. },
                        ) => panic!("queued task details leaked after revocation"),
                        MessageBody::SystemEventBody(
                            tentaflow_protocol::SystemEventPayload::ServiceStatusChanged {
                                service_name,
                                ..
                            },
                        ) if service_name == "p1-sentinel" => break,
                        _ => {}
                    }
                }
            }
        })
        .await
        .expect("generic event remains visible after private event filtering");
        client.close(None).await.expect("close");
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server closes")
            .expect("server task");
    }

    #[tokio::test]
    async fn real_dispatch_catalogue_links_archive_and_self_handover_have_atomic_history() {
        let f = fixture();
        let owner = &f.contexts[0];
        let reader = &f.contexts[1];
        let observer = user(&f.contexts[2]);
        let task_id = create(&f, user(reader), "[]").await;
        let target_id = create(&f, "", "[]").await;
        let custom = || ProjectStudioPayload::TaskTypeSaveRequest {
            project_id: f.project.project_id.clone(),
            type_id: "customer_note".into(),
            name: "Customer note".into(),
            description: "Real custom type".into(),
            sort_order: 10,
            active: true,
        };
        assert_eq!(
            call(reader, custom())
                .await
                .expect_err("catalogue admin")
                .code,
            ProtocolErrorCode::PolicyDenied
        );
        call(owner, custom())
            .await
            .expect("custom catalogue persists");
        let types = call(
            &f.contexts[3],
            ProjectStudioPayload::TaskTypesListRequest {
                project_id: f.project.project_id.clone(),
            },
        )
        .await
        .expect("no-function creator can pick types");
        let ProjectStudioPayload::TaskTypesListResponse { types } = types else {
            panic!("types")
        };
        assert_eq!(types.iter().filter(|t| t.built_in).count(), 6);
        assert!(types
            .iter()
            .any(|t| t.type_id == "customer_note" && t.active));
        let linked = call(
            owner,
            ProjectStudioPayload::TaskLinkSaveRequest {
                project_id: f.project.project_id.clone(),
                source_task_id: task_id.clone(),
                target_task_id: target_id.clone(),
                kind: "fs".into(),
                lag_days: 2,
            },
        )
        .await
        .expect("relation");
        let ProjectStudioPayload::TaskLinkSaveResult {
            link,
            source_event_id,
            target_event_id,
            ..
        } = linked
        else {
            panic!("relation")
        };
        assert_ne!(source_event_id, target_event_id);
        repository::set_member_access(
            &f.project.project_id,
            user(reader),
            &["observer".into()],
            false,
            None,
        )
        .expect("current assignee becomes reader");
        let handed = call(
            reader,
            ProjectStudioPayload::TaskHandoverRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                assigned_to: user(owner).into(),
                note_md: "Finish the pending review".into(),
                mention_user_ids: vec![observer.into()],
            },
        )
        .await
        .expect("self handover exception");
        let ProjectStudioPayload::TaskHandoverResult {
            event_ids,
            comment_id: Some(comment_id),
            ..
        } = handed
        else {
            panic!("atomic handover")
        };
        assert_eq!(event_ids.len(), 2);
        let events = tasks::list_task_events(&f.pool, &task_id, None, 100)
            .expect("history")
            .0;
        assert!(events.iter().any(|e| e.kind == "handed_over"
            && e.actor_id == user(reader)
            && e.after_json.contains(&comment_id)));
        assert_eq!(
            tasks::latest_handover_comment_id(&f.pool, &task_id).expect("pin"),
            Some(comment_id.clone())
        );
        assert_eq!(
            bell(&f.contexts[2])
                .await
                .iter()
                .filter(|n| n.kind == "task_mentioned")
                .count(),
            1,
            "committed handover mentions are sent"
        );
        assert_eq!(
            call(
                reader,
                save(&f, Some(task_id.clone()), user(owner), "todo", "[]")
            )
            .await
            .expect_err("reader cannot edit ordinary fields")
            .code,
            ProtocolErrorCode::PolicyDenied
        );
        repository::set_member_access(
            &f.project.project_id,
            user(reader),
            &["developer".into()],
            false,
            None,
        )
        .expect("comment author regains task write before archive tests");
        call(
            owner,
            ProjectStudioPayload::TaskArchiveRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                archived: true,
            },
        )
        .await
        .expect("archive");
        let snapshot_events = |id: &str| {
            tasks::list_task_events(&f.pool, id, None, 100)
                .expect("event snapshot")
                .0
                .into_iter()
                .map(|event| {
                    (
                        event.event_id,
                        event.kind,
                        event.actor_id,
                        event.before_json,
                        event.after_json,
                    )
                })
                .collect::<Vec<_>>()
        };
        let archived_history = snapshot_events(&task_id);
        let counterpart_history = snapshot_events(&target_id);
        let before_comment = tasks::get_comment(&f.pool, &comment_id)
            .expect("comment")
            .expect("row");
        let edit_comment = || ProjectStudioPayload::TaskCommentEditRequest {
            project_id: f.project.project_id.clone(),
            comment_id: comment_id.clone(),
            body_md: "Edited after actual restore".into(),
            mention_user_ids: vec![],
        };
        let delete_comment = || ProjectStudioPayload::TaskCommentDeleteRequest {
            project_id: f.project.project_id.clone(),
            comment_id: comment_id.clone(),
        };
        let delete_link = || ProjectStudioPayload::TaskLinkDeleteRequest {
            project_id: f.project.project_id.clone(),
            link_id: link.link_id,
        };
        for request in [edit_comment(), delete_comment()] {
            assert_eq!(
                call(reader, request)
                    .await
                    .expect_err("archived task comment is read-only")
                    .code,
                ProtocolErrorCode::BadRequest
            );
        }
        assert_eq!(
            call(owner, delete_link())
                .await
                .expect_err("link touching an archived task is read-only")
                .code,
            ProtocolErrorCode::BadRequest
        );
        let after_comment = tasks::get_comment(&f.pool, &comment_id)
            .expect("comment")
            .expect("row preserved");
        assert_eq!(
            (
                after_comment.body_md,
                after_comment.mention_user_ids_json,
                after_comment.edited_at
            ),
            (
                before_comment.body_md,
                before_comment.mention_user_ids_json,
                before_comment.edited_at
            )
        );
        assert!(tasks::list_task_links(&f.pool, &task_id)
            .expect("relation preserved")
            .iter()
            .any(|row| row.link_id == link.link_id));
        assert_eq!(snapshot_events(&task_id), archived_history);
        assert_eq!(snapshot_events(&target_id), counterpart_history);
        let list = |include_archived| ProjectStudioPayload::TasksListRequest {
            project_id: f.project.project_id.clone(),
            task_type: String::new(),
            status: String::new(),
            assigned_to: String::new(),
            search: String::new(),
            offset: 0,
            limit: 100,
            severity: String::new(),
            include_archived,
        };
        let ProjectStudioPayload::TasksListResponse { tasks: active, .. } =
            call(owner, list(false)).await.expect("active list")
        else {
            panic!("list")
        };
        assert!(!active.iter().any(|t| t.task_id == task_id));
        let ProjectStudioPayload::TasksListResponse { tasks: history, .. } =
            call(owner, list(true)).await.expect("archived list")
        else {
            panic!("list")
        };
        assert!(history
            .iter()
            .any(|t| t.task_id == task_id && t.archived_at.is_some()));
        call(
            owner,
            ProjectStudioPayload::TaskArchiveRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                archived: false,
            },
        )
        .await
        .expect("undo archive");
        assert!(tasks::get_task(&f.pool, &task_id)
            .expect("task")
            .expect("row")
            .archived_at
            .is_none());
        call(reader, edit_comment())
            .await
            .expect("restored task permits author comment edit");
        call(reader, delete_comment())
            .await
            .expect("restored task permits author comment delete");
        call(owner, delete_link())
            .await
            .expect("restored endpoints permit relation delete");
        assert!(tasks::get_comment(&f.pool, &comment_id)
            .expect("comment")
            .is_none());
        assert!(tasks::list_task_links(&f.pool, &task_id)
            .expect("relations")
            .is_empty());
    }

    #[tokio::test]
    async fn real_dispatch_case_item_step_attachment_scopes_and_module_revocation() {
        let f = fixture();
        let bytes = b"Exact execution attachment";
        let sha = hex::encode(sha2::Sha256::digest(bytes));
        let upload_id = uuid::Uuid::new_v4().to_string();
        call(
            &f.contexts[0],
            ProjectStudioPayload::AttachmentUploadChunkRequest {
                project_id: f.project.project_id.clone(),
                upload_id,
                filename: "execution.bin".into(),
                mime: "application/octet-stream".into(),
                sha256: sha.clone(),
                total_size: bytes.len() as u64,
                offset: 0,
                bytes: bytes.to_vec(),
            },
        )
        .await
        .expect("actual durable upload");
        let raw = serde_json::to_string(&vec![AttachmentWire {
            sha256: sha.clone(),
            name: "execution.bin".into(),
            mime: "application/octet-stream".into(),
            size_bytes: bytes.len() as u64,
        }])
        .expect("refs");
        let result = call(
            &f.contexts[0],
            ProjectStudioPayload::CaseSaveRequest {
                project_id: f.project.project_id.clone(),
                case_id: None,
                kind: "manual".into(),
                title: "Actual execution case".into(),
                priority: "medium".into(),
                content_json: r#"{"steps":[{"action":"Open","expected":"Visible"}]}"#.into(),
                tag_ids: vec![],
                linked_source_ids: vec![],
                attachments_json: raw.clone(),
                expected_version: None,
                change_note: String::new(),
            },
        )
        .await
        .expect("case");
        let ProjectStudioPayload::CaseSaveResponse { case_id, .. } = result else {
            panic!("case")
        };
        {
            let conn = f.pool.write().expect("writer");
            conn.execute("INSERT INTO test_runs (run_id,run_no,name,assignment_mode,status,created_by) VALUES ('execution-run',1,'Manual execution','single','running',?1)", [user(&f.contexts[0])]).expect("real run");
            conn.execute("INSERT INTO test_run_items (item_id,run_id,case_id,case_title,case_version,position,attachments_json) VALUES ('execution-item','execution-run',?1,'Actual execution case',1,0,?2)", rusqlite::params![case_id,raw]).expect("item");
            conn.execute("INSERT INTO test_run_steps (item_id,step_index,action,attachments_json) VALUES ('execution-item',0,'Open',?1)", [&raw]).expect("step");
            conn.execute("INSERT INTO test_run_steps (item_id,step_index,action) VALUES ('execution-item',1,'Close')", []).expect("other step");
        }
        let read = |kind, owner_id: &str, step_index| ProjectStudioPayload::AttachmentGetRequest {
            project_id: f.project.project_id.clone(),
            owner_kind: kind,
            owner_id: owner_id.into(),
            step_index,
            sha256: sha.clone(),
            offset: 6,
            max_bytes: 4,
            preview: false,
        };
        for (kind, id, index) in [
            (AttachmentOwnerKind::Case, case_id.as_str(), None),
            (AttachmentOwnerKind::RunItem, "execution-item", None),
            (AttachmentOwnerKind::RunStep, "execution-item", Some(0)),
        ] {
            let result = call(&f.contexts[1], read(kind, id, index))
                .await
                .expect("actual Tests Read scope");
            let ProjectStudioPayload::AttachmentGetResponse {
                bytes: result,
                total_size,
                ..
            } = result
            else {
                panic!("read")
            };
            assert_eq!(result, &bytes[6..10]);
            assert_eq!(total_size, bytes.len() as u64);
            assert_eq!(
                call(&f.contexts[3], read(kind, id, index))
                    .await
                    .expect_err("no-function cannot read tests")
                    .code,
                ProtocolErrorCode::PolicyDenied
            );
        }
        assert_eq!(
            call(
                &f.contexts[1],
                read(AttachmentOwnerKind::RunStep, "execution-item", Some(1))
            )
            .await
            .expect_err("wrong step cannot reuse sibling SHA")
            .code,
            ProtocolErrorCode::NotFound
        );
        assert_eq!(
            call(
                &f.contexts[1],
                read(AttachmentOwnerKind::RunStep, "execution-item", None)
            )
            .await
            .expect_err("step index is required")
            .code,
            ProtocolErrorCode::BadRequest
        );
        assert_eq!(
            call(
                &f.contexts[1],
                read(AttachmentOwnerKind::Case, &case_id, Some(0))
            )
            .await
            .expect_err("step index cannot scope a case")
            .code,
            ProtocolErrorCode::BadRequest
        );
        repository::update_project_modules(&f.project.org_id, &f.project.project_id, "[\"tasks\"]")
            .expect("disable tests");
        assert_eq!(
            call(
                &f.contexts[1],
                read(AttachmentOwnerKind::Case, &case_id, None)
            )
            .await
            .expect_err("disabled module revokes media")
            .code,
            ProtocolErrorCode::PolicyDenied
        );
    }

    #[tokio::test]
    async fn real_dispatch_attachment_save_requires_upload_or_exact_readable_owner_reference() {
        let f = fixture();
        let bytes = b"Original owned recording";
        let sha = hex::encode(sha2::Sha256::digest(bytes));
        let upload = |upload_id: &str| ProjectStudioPayload::AttachmentUploadChunkRequest {
            project_id: f.project.project_id.clone(),
            upload_id: upload_id.into(),
            filename: "recording.bin".into(),
            mime: "application/octet-stream".into(),
            sha256: sha.clone(),
            total_size: bytes.len() as u64,
            offset: 0,
            bytes: bytes.to_vec(),
        };
        call(&f.contexts[0], upload("original-owner"))
            .await
            .expect("owner upload");
        let raw = serde_json::to_string(&vec![AttachmentWire {
            sha256: sha.clone(),
            name: "recording.bin".into(),
            mime: "application/octet-stream".into(),
            size_bytes: bytes.len() as u64,
        }])
        .expect("reference");
        let task_id = create(&f, "", &raw).await;
        let denied = call(&f.contexts[1], save(&f, None, "", "todo", &raw))
            .await
            .expect_err("a known SHA from another record is not a new authorized upload");
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        call(&f.contexts[1], save(&f, Some(task_id), "", "todo", &raw))
            .await
            .expect("retaining exact readable owner reference");
        call(&f.contexts[3], upload("no-function-owner"))
            .await
            .expect("no-function creator uploads own bytes");
        call(&f.contexts[3], save(&f, None, "", "todo", &raw))
            .await
            .expect("no-function creator may attach their verified upload");
        let mut wrong_size: Vec<AttachmentWire> = serde_json::from_str(&raw).expect("reference");
        wrong_size[0].size_bytes += 1;
        assert_eq!(
            call(
                &f.contexts[0],
                save(
                    &f,
                    None,
                    "",
                    "todo",
                    &serde_json::to_string(&wrong_size).expect("JSON")
                )
            )
            .await
            .expect_err("untrusted size metadata")
            .code,
            ProtocolErrorCode::PolicyDenied
        );
        #[cfg(unix)]
        {
            let original = std::path::Path::new(&f.project.dir_path)
                .join("files")
                .join(&sha);
            let other = std::path::Path::new(&f.project.dir_path).join("outside");
            std::fs::rename(&original, &other).expect("move original");
            std::os::unix::fs::symlink(&other, &original).expect("symlink");
            assert_eq!(
                call(&f.contexts[0], save(&f, None, "", "todo", &raw))
                    .await
                    .expect_err("symlink cannot be persisted as original")
                    .code,
                ProtocolErrorCode::BadRequest
            );
        }
    }

    #[tokio::test]
    async fn real_dispatch_more_than_20_attachments_retains_original_history_and_bounds_reads() {
        let f = fixture();
        let mut attachments = Vec::new();
        for index in 0..21u8 {
            let bytes = vec![index; 1024];
            let sha = hex::encode(sha2::Sha256::digest(&bytes));
            call(
                &f.contexts[0],
                ProjectStudioPayload::AttachmentUploadChunkRequest {
                    project_id: f.project.project_id.clone(),
                    upload_id: format!("asset-{index}"),
                    filename: format!("recording-{index}.bin"),
                    mime: "application/octet-stream".into(),
                    sha256: sha.clone(),
                    total_size: bytes.len() as u64,
                    offset: 0,
                    bytes,
                },
            )
            .await
            .expect("actual original upload");
            attachments.push(AttachmentWire {
                sha256: sha,
                name: format!("recording-{index}.bin"),
                mime: "application/octet-stream".into(),
                size_bytes: 1024,
            });
        }
        let raw = serde_json::to_string(&attachments).expect("refs");
        let task_id = create(&f, "", &raw).await;
        let sha = &attachments[20].sha256;
        let read = |sha: &str, owner_id: &str, offset, max_bytes| {
            ProjectStudioPayload::AttachmentGetRequest {
                project_id: f.project.project_id.clone(),
                owner_kind: AttachmentOwnerKind::Task,
                owner_id: owner_id.into(),
                step_index: None,
                sha256: sha.into(),
                offset,
                max_bytes,
                preview: false,
            }
        };
        let ProjectStudioPayload::AttachmentGetResponse {
            bytes,
            total_size,
            eof,
            ..
        } = call(&f.contexts[2], read(sha, &task_id, 1000, 1024))
            .await
            .expect("bounded exact task ref")
        else {
            panic!("bytes")
        };
        assert_eq!(total_size, 1024);
        assert_eq!(bytes, vec![20u8; 24]);
        assert!(eof);
        assert_eq!(
            call(&f.contexts[2], read(sha, "another-task", 0, 1024))
                .await
                .expect_err("unlinked blob hidden")
                .code,
            ProtocolErrorCode::NotFound
        );
        assert_eq!(
            call(&f.contexts[2], read(sha, &task_id, 0, 4 * 1024 * 1024 + 1))
                .await
                .expect_err("transport bound")
                .code,
            ProtocolErrorCode::BadRequest
        );
        call(
            &f.contexts[0],
            save(&f, Some(task_id.clone()), "", "todo", "[]"),
        )
        .await
        .expect("remove current refs");
        call(&f.contexts[2], read(sha, &task_id, 0, 1024))
            .await
            .expect("historical original remains authorized");
        call(
            &f.contexts[0],
            ProjectStudioPayload::TaskArchiveRequest {
                project_id: f.project.project_id.clone(),
                task_id: task_id.clone(),
                archived: true,
            },
        )
        .await
        .expect("archive task");
        let ProjectStudioPayload::AttachmentUsageResponse {
            file_count,
            total_bytes,
            per_task,
            ..
        } = call(
            &f.contexts[2],
            ProjectStudioPayload::AttachmentUsageRequest {
                project_id: f.project.project_id.clone(),
                offset: 0,
                limit: 100,
            },
        )
        .await
        .expect("actual historical usage")
        else {
            panic!("usage")
        };
        assert_eq!(file_count, 21);
        assert_eq!(total_bytes, 21 * 1024);
        assert_eq!(per_task.len(), 1);
        assert_eq!(per_task[0].file_count, 21);
        repository::set_project_archived(&f.project.org_id, &f.project.project_id, true)
            .expect("archive project");
        call(&f.contexts[2], read(sha, &task_id, 0, 1024))
            .await
            .expect("archived read remains valid");
        crate::project_studio::db::pool().expect("registry").write().expect("writer").execute("UPDATE project_members SET expires_at = '2000-01-01T00:00:00Z' WHERE project_id = ?1 AND user_id = ?2", rusqlite::params![f.project.project_id,user(&f.contexts[2])]).expect("expire reader");
        assert_eq!(
            call(&f.contexts[2], read(sha, &task_id, 0, 1024))
                .await
                .expect_err("expired media access")
                .code,
            ProtocolErrorCode::NotFound
        );
    }
}
