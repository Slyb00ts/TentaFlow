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
    ActivityEntry, AttachmentWire, CaseVersionInfo, ChatInfo, ChatMessageWire, CreatorGrantInfo,
    CsvImportError, GenerationRunInfo, IngestJobWire, KbHit, MemberInfo, MemberInputWire,
    MyWorkEntry, NotificationWire, OverviewKpis, ProjectAgentBinding, ProjectInfo,
    ProjectSettings, ProjectStudioPayload, RunAssignmentWire, RunItemWire, RunStepWire,
    SourceFileInfo, SourceInfo, SuiteCaseRef, SuiteInfo, TagInfo, TaskCommentWire, TaskDetail,
    TaskInfo, TestCaseDetail, TestCaseInfo, TestRunInfo, UserRefWire,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use crate::project_studio::models::{
    ActivityRecord, CaseListItem, GenerationRunRecord, IngestJobRecord, ProjectRecord,
    ProjectRole, RunCounts, RunItemRecord, RunRecord, RunStepRecord, SourceListItem,
    TaskCommentRecord, TaskRecord,
};
use crate::project_studio::{
    activity, generation, ingest, notifications, project_db, reports, repository, runs, tasks,
    tests as ps_tests,
};
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
            sha256,
            max_bytes,
        } => attachment_get_v1(ctx, project_id, sha256, *max_bytes),
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
        } => suite_save_v1(ctx, project_id, suite_id.as_deref(), name, description, case_ids),
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
        } => run_step_set_v1(ctx, project_id, item_id, *step_index, status, note, attachments_json),
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
        } => tasks_list_v1(
            ctx, project_id, task_type, status, assigned_to, search, *offset, *limit,
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
            links_json,
            attachments_json,
        ),
        P::TaskDeleteRequest {
            project_id,
            task_id,
        } => task_delete_v1(ctx, project_id, task_id),
        P::TaskCommentAddRequest {
            project_id,
            task_id,
            body_md,
        } => task_comment_add_v1(ctx, project_id, task_id, body_md),
        P::TaskCommentEditRequest {
            project_id,
            comment_id,
            body_md,
        } => task_comment_edit_v1(ctx, project_id, comment_id, body_md),
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
        P::GenerationGetRequest { project_id, gen_id } => generation_get_v1(ctx, project_id, gen_id),
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
        } => report_query_v1(ctx, project_id, report, from_date, to_date, suite_id),
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
        | P::ReportQueryResponse { .. } => Err(ProtocolError::bad_request(
            "variant is not a valid project studio request",
        )),
        // Warianty F3 (środowiska, auto-runy, try-run, code assist) są już na
        // wire, ale ich handlery przychodzą razem z backendem F3 — to
        // tymczasowe ramię utrzymuje kompilację exhaustive matcha do tego
        // czasu. Backend F3 usuwa je i wpina realny routing.
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
    // Tester tier (not editor): testers upload screenshots as run/step/task
    // attachments through this same chunked endpoint (section C).
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
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
        let refs = repository::blob_ref_count(&pool, &f.sha256)
            .map_err(|e| db_error("blob_ref_count", e))?;
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
    let f2 = repository::project_f2_kpis(&pool, &org.user_id).map_err(|e| db_error("kpis_f2", e))?;
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
            cases_total: f2.cases_total,
            cases_approved: f2.cases_approved,
            suites_total: f2.suites_total,
            runs_open: f2.runs_open,
            my_run_items_pending: f2.my_run_items_pending,
            tasks_open: f2.tasks_open,
            defects_open: f2.defects_open,
            generations_running: f2.generations_running,
            // Liczniki F3 zaczną być zliczane razem z backendem F3
            // (environments/auto_run_meta jeszcze nie mają repozytorium);
            // 0 = brak danych, zgodne z serde(default) na wire.
            environments_approved: 0,
            environments_pending: 0,
            auto_runs_open: 0,
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
fn normalize_attachments(raw: &str) -> Result<String, ProtocolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok("[]".to_string());
    }
    let list: Vec<AttachmentWire> = serde_json::from_str(raw)
        .map_err(|e| ProtocolError::bad_request(format!("invalid attachments_json: {e}")))?;
    if list.len() > 20 {
        return Err(ProtocolError::bad_request("too many attachments (max 20)"));
    }
    for entry in &list {
        if !is_sha256_hex(&entry.sha256) {
            return Err(ProtocolError::bad_request(
                "attachment sha256 must be 64 lowercase hex characters",
            ));
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
        if entry.get("id").and_then(|i| i.as_str()).unwrap_or("").is_empty() {
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

fn run_to_wire(
    record: RunRecord,
    counts: RunCounts,
    suite_name: String,
    names: &HashMap<String, (String, String)>,
) -> TestRunInfo {
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
        // Pola F3 wypełni backend F3 (auto_run_meta + resolve nazwy
        // środowiska); do tego czasu wartości puste = run manualny.
        environment_name: String::new(),
        runner_service_id: String::new(),
        errored: 0,
        perf_summary_json: None,
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
    let ids: Vec<String> = items
        .iter()
        .map(|i| i.record.created_by.clone())
        .collect();
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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

/// Shared field validation of a case save (create + edit).
fn validate_case_fields(
    kind: &str,
    title: &str,
    priority: &str,
    content_json: &str,
) -> Result<(), ProtocolError> {
    if kind != "manual" {
        return Err(ProtocolError::bad_request(
            "only kind 'manual' is available in this phase",
        ));
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ProtocolError::bad_request("title must be 1..200 characters"));
    }
    if !ps_tests::CASE_PRIORITIES.contains(&priority) {
        return Err(ProtocolError::bad_request(format!(
            "unknown priority '{priority}'"
        )));
    }
    let parsed: serde_json::Value = serde_json::from_str(content_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid content_json: {e}")))?;
    if !parsed.is_object() {
        return Err(ProtocolError::bad_request("content_json must be an object"));
    }
    if content_json.len() > 256 * 1024 {
        return Err(ProtocolError::bad_request("content_json exceeds 256 KiB"));
    }
    Ok(())
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    validate_case_fields(kind, title, priority, content_json)?;
    let attachments_json = normalize_attachments(attachments_json)?;
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
    role: ProjectRole,
    case_id: &str,
    target: &str,
    reason: &str,
) -> Result<Result<bool, &'static str>, ProtocolError> {
    let Some(item) = ps_tests::get_case(pool, case_id).map_err(|e| db_error("case_get", e))? else {
        return Ok(Err("case not found"));
    };
    let from = item.record.status.as_str();
    let Some((min_role, needs_reason)) = ps_tests::transition_requirement(from, target) else {
        return Ok(Err("transition not allowed"));
    };
    if role < min_role {
        return Ok(Err("requires a higher project role"));
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
    let (record, role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let role = role.expect("editor gate never yields admin override");
    if !ps_tests::CASE_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    let pool = open_project_pool(project_id)?;
    let ok = match apply_status_transition(&pool, role, case_id, status, reason)? {
        Ok(ok) => ok,
        Err("case not found") => return Err(ProtocolError::not_found("case not found")),
        Err("requires a higher project role") => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "this transition requires a higher project role",
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
    let (record, role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let role = role.expect("editor gate never yields admin override");
    if !ps_tests::CASE_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    if case_ids.is_empty() || case_ids.len() > 200 {
        return Err(ProtocolError::bad_request("case_ids must contain 1..200 ids"));
    }
    let pool = open_project_pool(project_id)?;
    let mut updated = 0u32;
    for case_id in case_ids {
        // Bulk semantics: cases the caller may not (or must not) transition
        // are skipped, the rest proceed — `updated` reports the real count.
        if let Ok(true) = apply_status_transition(&pool, role, case_id, status, reason)? {
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
    Ok(ps(ProjectStudioPayload::CasesBulkStatusResponse { updated }))
}

fn case_duplicate_v1(
    ctx: &HandlerContext,
    project_id: &str,
    case_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
    let (record, role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let role = role.expect("editor gate never yields admin override");
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
    let refs = ps_tests::case_run_item_refs(&pool, case_id)
        .map_err(|e| db_error("case_refs", e))?;
    if refs > 0 {
        return Err(ProtocolError::bad_request(
            "case is referenced by test runs — deprecate instead",
        ));
    }
    // Editor: own drafts only. Manager+: draft + review.
    let allowed = if role >= ProjectRole::Manager {
        matches!(item.record.status.as_str(), "draft" | "review")
    } else {
        item.record.status == "draft" && item.record.created_by == org.user_id
    };
    if !allowed {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "editors may delete only their own drafts",
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
                &serde_json::json!({ "from_version": version, "version": new_version })
                    .to_string(),
            );
            Ok(ps(ProjectStudioPayload::CaseRestoreVersionResponse {
                case_id: case_id.to_string(),
                version: new_version,
            }))
        }
        ps_tests::CaseUpdateOutcome::Conflict => Err(conflict()),
        ps_tests::CaseUpdateOutcome::NotFound => Err(ProtocolError::not_found(
            "case or version not found",
        )),
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
            created: if errors.is_empty() { rows.len() as u32 } else { 0 },
            errors,
        }));
    }
    let pool = open_project_pool(project_id)?;
    let created =
        ps_tests::import_cases(&pool, &rows, &org.user_id).map_err(|e| db_error("csv_import", e))?;
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

const ATTACHMENT_MAX_BYTES: u32 = 8 * 1024 * 1024;

fn attachment_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    sha256: &str,
    max_bytes: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    if !is_sha256_hex(sha256) {
        return Err(ProtocolError::bad_request(
            "sha256 must be 64 lowercase hex characters",
        ));
    }
    let cap = max_bytes.clamp(1, ATTACHMENT_MAX_BYTES) as usize;
    let blob = std::path::Path::new(&record.dir_path)
        .join("files")
        .join(sha256);
    let bytes = std::fs::read(&blob)
        .map_err(|_| ProtocolError::not_found("attachment not found"))?;
    let truncated = bytes.len() > cap;
    let mut bytes = bytes;
    bytes.truncate(cap);
    let pool = open_project_pool(project_id)?;
    let mime = repository::attachment_mime(&pool, sha256)
        .map_err(|e| db_error("attachment_mime", e))?
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok(ps(ProjectStudioPayload::AttachmentGetResponse {
        bytes,
        mime,
        truncated,
    }))
}

// =============================================================================
// F2: test suites (T04)
// =============================================================================

fn suite_item_to_wire(
    item: crate::project_studio::tests::SuiteListItem,
) -> Result<SuiteInfo, ProtocolError> {
    let last_run = match item.last_run {
        Some((record, counts)) => {
            let suite_name = item.record.name.clone();
            let names =
                repository::resolve_user_refs(std::slice::from_ref(&record.created_by));
            Some(run_to_wire(record, counts, suite_name, &names))
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let items = ps_tests::list_suites(&pool).map_err(|e| db_error("suites_list", e))?;
    let _ = &pool;
    let mut suites = Vec::with_capacity(items.len());
    for item in items {
        suites.push(suite_item_to_wire(item)?);
    }
    Ok(ps(ProjectStudioPayload::SuitesListResponse { suites }))
}

fn suite_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    suite_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    Ok(ps(ProjectStudioPayload::SuiteGetResponse {
        suite: suite_item_to_wire(item)?,
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ProtocolError::bad_request("suite name is required"));
    }
    if case_ids.len() > 500 {
        return Err(ProtocolError::bad_request("a suite holds at most 500 cases"));
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let limit = limit.clamp(1, 200);
    let (rows, total) = runs::list_runs(&pool, status, run_type, offset, limit)
        .map_err(|e| db_error("runs_list", e))?;
    let suite_ids: Vec<String> = rows.iter().map(|(r, _)| r.suite_id.clone()).collect();
    let suite_names =
        ps_tests::suite_names(&pool, &suite_ids).map_err(|e| db_error("suite_names", e))?;
    let ids: Vec<String> = rows.iter().map(|(r, _)| r.created_by.clone()).collect();
    let names = repository::resolve_user_refs(&ids);
    let runs_wire = rows
        .into_iter()
        .map(|(record, counts)| {
            let suite_name = suite_names.get(&record.suite_id).cloned().unwrap_or_default();
            run_to_wire(record, counts, suite_name, &names)
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
        let target = repository::effective_role(project_id, user_id)
            .map_err(|e| db_error("member_role", e))?;
        if !matches!(target, Some(r) if r >= ProjectRole::Tester) {
            return Err(ProtocolError::bad_request(format!(
                "user '{user_id}' is not a tester (or higher) of this project"
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
    Ok(ps(ProjectStudioPayload::RunCreateResponse { run_id, run_no }))
}

fn load_run_wire(
    pool: &crate::db::DbPool,
    record: RunRecord,
    counts: RunCounts,
) -> Result<TestRunInfo, ProtocolError> {
    let suite_names = ps_tests::suite_names(pool, std::slice::from_ref(&record.suite_id))
        .map_err(|e| db_error("suite_names", e))?;
    let suite_name = suite_names.get(&record.suite_id).cloned().unwrap_or_default();
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.created_by));
    Ok(run_to_wire(record, counts, suite_name, &names))
}

fn run_get_v1(
    ctx: &HandlerContext,
    project_id: &str,
    run_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (record, role) = require_project(org, project_id, ProjectRole::Viewer)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (run, _counts) = runs::get_run(&pool, run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    // Manager+ OR the run's creator (section C).
    let is_manager = matches!(role, Some(r) if r >= ProjectRole::Manager);
    if !is_manager && run.created_by != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "closing a run requires the manager role or run ownership",
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
                    &serde_json::json!({ "project_id": project_id, "run_id": run_id })
                        .to_string(),
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
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
    role: Option<ProjectRole>,
    pool: &crate::db::DbPool,
    item_id: &str,
) -> Result<(RunItemRecord, RunRecord), ProtocolError> {
    let item = runs::get_run_item(pool, item_id)
        .map_err(|e| db_error("item_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run item not found"))?;
    let (run, _counts) = runs::get_run(pool, &item.run_id)
        .map_err(|e| db_error("run_get", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    let is_manager = matches!(role, Some(r) if r >= ProjectRole::Manager);
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
    let (record, role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let (item, run) = load_owned_item(org, role, &pool, item_id)?;
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
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
    let attachments_json = normalize_attachments(attachments_json)?;
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
    let ok = runs::set_step(&pool, item_id, step_index, status, note.trim(), &attachments_json)
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let attachments_json = normalize_attachments(attachments_json)?;
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
        &serde_json::json!({ "status": final_status, "duration_secs": duration_secs })
            .to_string(),
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
    let my_roles =
        repository::member_roles_for_user(&org.user_id).map_err(|e| db_error("member_roles", e))?;
    let mut entries: Vec<MyWorkEntry> = Vec::new();
    for (project_id, role) in my_roles {
        // Claiming needs tester+; viewers have no work queue.
        if !matches!(ProjectRole::from_slug(&role), Some(r) if r >= ProjectRole::Tester) {
            continue;
        }
        let Some(record) = repository::get_project(&org.org_id, &project_id)
            .map_err(|e| db_error("get_project", e))?
        else {
            continue;
        };
        if record.status != "active"
            || !parse_modules_json(&record.modules_json)
                .iter()
                .any(|m| m == "tests")
        {
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
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    let record = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let comments = tasks::list_comments(&pool, task_id).map_err(|e| db_error("comments", e))?;
    let mut ids: Vec<String> = comments.iter().map(|c| c.author_user_id.clone()).collect();
    ids.push(record.created_by.clone());
    ids.push(record.assigned_to.clone());
    let names = repository::resolve_user_refs(&ids);
    let description_md = record.description_md.clone();
    let attachments = attachments_from_json(&record.attachments_json);
    Ok(ps(ProjectStudioPayload::TaskGetResponse {
        detail: TaskDetail {
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
    links_json: &str,
    attachments_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Tester tier: "Zgłoś usterkę" comes straight from the tester desk.
    let (record, role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let role = role.expect("tester gate never yields admin override");
    if !tasks::TASK_TYPES.contains(&task_type) {
        return Err(ProtocolError::bad_request(format!(
            "unknown task_type '{task_type}'"
        )));
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ProtocolError::bad_request("title must be 1..200 characters"));
    }
    if !tasks::TASK_PRIORITIES.contains(&priority) {
        return Err(ProtocolError::bad_request(format!(
            "unknown priority '{priority}'"
        )));
    }
    if !tasks::TASK_STATUSES.contains(&status) {
        return Err(ProtocolError::bad_request(format!(
            "unknown status '{status}'"
        )));
    }
    let severity = severity.trim();
    if task_type == "defect" {
        if !tasks::TASK_SEVERITIES.contains(&severity) {
            return Err(ProtocolError::bad_request(
                "a defect requires severity (low|medium|high|critical)",
            ));
        }
    } else if !severity.is_empty() {
        return Err(ProtocolError::bad_request("severity applies only to defects"));
    }
    if !assigned_to.is_empty()
        && repository::effective_role(project_id, assigned_to)
            .map_err(|e| db_error("member_role", e))?
            .is_none()
    {
        return Err(ProtocolError::bad_request(
            "assigned_to must be a project member",
        ));
    }
    let links_json = normalize_links(links_json)?;
    let attachments_json = normalize_attachments(attachments_json)?;
    let input = tasks::TaskInput {
        task_type,
        title,
        description_md,
        severity,
        priority,
        status,
        assigned_to,
        due_date: due_date.trim(),
        links_json: &links_json,
        attachments_json: &attachments_json,
    };
    let pool = open_project_pool(project_id)?;
    let (task_id, task_no, assignment_changed) = match task_id {
        None => {
            let (id, no) =
                tasks::create_task(&pool, &input, &org.user_id).map_err(|e| db_error("task_create", e))?;
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "task.created",
                "task",
                &id,
                &serde_json::json!({ "title": title, "task_type": task_type }).to_string(),
            );
            (id, no, !assigned_to.is_empty())
        }
        Some(id) => {
            let existing = tasks::get_task(&pool, id)
                .map_err(|e| db_error("task_get", e))?
                .ok_or_else(|| ProtocolError::not_found("task not found"))?;
            // Edit: the author, the assigned tester, or any editor+.
            let allowed = existing.created_by == org.user_id
                || existing.assigned_to == org.user_id
                || role >= ProjectRole::Editor;
            if !allowed {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "only the author, the assignee or an editor may edit this task",
                ));
            }
            let changed = assigned_to != existing.assigned_to && !assigned_to.is_empty();
            if !tasks::update_task(&pool, id, &input).map_err(|e| db_error("task_update", e))? {
                return Err(ProtocolError::not_found("task not found"));
            }
            activity::record(
                &pool,
                &org.user_id,
                "user",
                "task.updated",
                "task",
                id,
                &serde_json::json!({ "title": title, "status": status }).to_string(),
            );
            (id.to_string(), existing.task_no, changed)
        }
    };
    if assignment_changed && assigned_to != org.user_id {
        notifications::notify(
            &org.org_id,
            assigned_to,
            project_id,
            "task_assigned",
            "Przypisano Ci zadanie",
            &format!("#{task_no} „{title}”"),
            &serde_json::json!({ "project_id": project_id, "task_id": task_id }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskSaveResponse { task_id, task_no }))
}

fn task_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let role = role.expect("tester gate never yields admin override");
    let pool = open_project_pool(project_id)?;
    let task = tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    // Manager anytime; the author only while nobody commented.
    let allowed = role >= ProjectRole::Manager
        || (task.created_by == org.user_id && task.comment_count == 0);
    if !allowed {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "deleting requires the manager role (or authorship of an uncommented task)",
        ));
    }
    let ok = tasks::delete_task(&pool, task_id).map_err(|e| db_error("task_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task.deleted",
            "task",
            task_id,
            &serde_json::json!({ "title": task.title }).to_string(),
        );
    }
    Ok(ps(ProjectStudioPayload::TaskDeleteResult { ok }))
}

fn task_comment_add_v1(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    body_md: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let body = body_md.trim();
    if body.is_empty() || body.chars().count() > 8000 {
        return Err(ProtocolError::bad_request("comment must be 1..8000 characters"));
    }
    let pool = open_project_pool(project_id)?;
    tasks::get_task(&pool, task_id)
        .map_err(|e| db_error("task_get", e))?
        .ok_or_else(|| ProtocolError::not_found("task not found"))?;
    let comment =
        tasks::add_comment(&pool, task_id, &org.user_id, body).map_err(|e| db_error("comment_add", e))?;
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "task_comment.added",
        "task",
        task_id,
        "{}",
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
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let body = body_md.trim();
    if body.is_empty() || body.chars().count() > 8000 {
        return Err(ProtocolError::bad_request("comment must be 1..8000 characters"));
    }
    let pool = open_project_pool(project_id)?;
    // Author-scoped UPDATE: another user's comment simply does not match.
    let ok = tasks::edit_comment(&pool, comment_id, &org.user_id, body)
        .map_err(|e| db_error("comment_edit", e))?;
    if !ok {
        return Err(ProtocolError::not_found("comment not found"));
    }
    activity::record(
        &pool,
        &org.user_id,
        "user",
        "task_comment.edited",
        "task_comment",
        comment_id,
        "{}",
    );
    Ok(ps(ProjectStudioPayload::TaskCommentEditResult { ok }))
}

fn task_comment_delete_v1(
    ctx: &HandlerContext,
    project_id: &str,
    comment_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_project(org, project_id, ProjectRole::Tester)?;
    require_active(&record)?;
    let role = role.expect("tester gate never yields admin override");
    let pool = open_project_pool(project_id)?;
    let comment = tasks::get_comment(&pool, comment_id)
        .map_err(|e| db_error("comment_get", e))?
        .ok_or_else(|| ProtocolError::not_found("comment not found"))?;
    if comment.author_user_id != org.user_id && role < ProjectRole::Manager {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the author or a manager may delete a comment",
        ));
    }
    let ok = tasks::delete_comment(&pool, comment_id).map_err(|e| db_error("comment_delete", e))?;
    if ok {
        activity::record(
            &pool,
            &org.user_id,
            "user",
            "task_comment.deleted",
            "task_comment",
            comment_id,
            "{}",
        );
    }
    Ok(ps(ProjectStudioPayload::TaskCommentDeleteResult { ok }))
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
    require_active(&record)?;
    if kind != "manual" {
        return Err(ProtocolError::bad_request(
            "only kind 'manual' is available in this phase",
        ));
    }
    if source_ids.is_empty() {
        return Err(ProtocolError::bad_request("select at least one source"));
    }
    if instructions.chars().count() > generation::MAX_INSTRUCTIONS_CHARS {
        return Err(ProtocolError::bad_request("instructions exceed 4000 characters"));
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

    // Agent resolution: explicit request > project 'generator_manual'
    // binding > the seeded system agent.
    let resolved_agent_id = match agent_id.filter(|a| !a.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None => {
            let bound: Option<String> = repository::get_setting(&pool, "agents")
                .map_err(|e| db_error("settings_get", e))?
                .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
                .and_then(|map| map.get("generator_manual").cloned())
                .filter(|a| !a.is_empty());
            bound.unwrap_or_else(|| generation::GENERATOR_MANUAL_AGENT_ID.to_string())
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

    let prompt =
        generation::build_generation_prompt(&record.name, &source_meta, instructions, max_cases);
    let principal =
        crate::agents::AgentPrincipal::new(Some(org.user_id.clone()), Some(org.org_id.clone()));
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
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
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    let pool = open_project_pool(project_id)?;
    generation::reconcile_running(&ctx.state.db, &pool, &org.org_id, project_id);
    let record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    let names = repository::resolve_user_refs(std::slice::from_ref(&record.started_by));
    let pending = generation::pending_cases(&pool, gen_id)
        .map_err(|e| db_error("pending_cases", e))?;
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
    let (record, role) = require_project(org, project_id, ProjectRole::Viewer)?;
    require_active(&record)?;
    let pool = open_project_pool(project_id)?;
    let generation_record = generation::get_generation(&pool, gen_id)
        .map_err(|e| db_error("generation_get", e))?
        .ok_or_else(|| ProtocolError::not_found("generation not found"))?;
    let is_manager = matches!(role, Some(r) if r >= ProjectRole::Manager);
    if !is_manager && generation_record.started_by != org.user_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "cancelling requires the manager role or generation ownership",
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
    Ok(ps(ProjectStudioPayload::GenerationCancelResult { ok: true }))
}

fn generation_review_v1(
    ctx: &HandlerContext,
    project_id: &str,
    gen_id: &str,
    accept_case_ids: &[String],
    reject_case_ids: &[String],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _role) = require_project(org, project_id, ProjectRole::Editor)?;
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
    let (record, _role) = require_project(org, project_id, ProjectRole::Manager)?;
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
    let ok =
        generation::delete_generation(&pool, gen_id).map_err(|e| db_error("generation_delete", e))?;
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

fn notifications_list_v1(
    ctx: &HandlerContext,
    only_unread: bool,
    before_id: Option<&str>,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let limit = limit.clamp(1, 100);
    let (rows, unread_count, has_more) =
        notifications::list(&org.user_id, only_unread, before_id, limit)
            .map_err(|e| db_error("notifications_list", e))?;
    let notifications_wire = rows
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
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (_record, _role) = require_project(org, project_id, ProjectRole::Viewer)?;
    if !reports::REPORT_KINDS.contains(&report) {
        return Err(ProtocolError::bad_request(format!(
            "unknown report '{report}'"
        )));
    }
    let pool = open_project_pool(project_id)?;
    let mut rows_json = reports::run_report(&pool, report, from_date, to_date, suite_id)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // tester_stats rows carry raw user ids — enrich with display names here
    // (the core user directory is not reachable from the per-project SQL).
    if report == "tester_stats" {
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
            rows_json = serde_json::Value::Array(rows).to_string();
        }
    }
    Ok(ps(ProjectStudioPayload::ReportQueryResponse { rows_json }))
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
