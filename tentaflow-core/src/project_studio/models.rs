// ============ File: project_studio/models.rs — persisted rows and project access invariants ============

mod access;
pub use access::{
    default_project_functions, evaluate_project_access, function_level, validate_member_input,
};

// =============================================================================
// Central registry rows (projects.db)
// =============================================================================

#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub project_id: String,
    pub org_id: String,
    pub key_prefix: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub template: String,
    pub modules_json: String,
    pub owner_user_id: String,
    pub dir_path: String,
    pub created_at: String,
    pub updated_at: String,
    pub parent_id: Option<String>,
    pub path: String,
    pub depth: u32,
    pub is_private: bool,
    pub inherit_modules: bool,
    pub inherit_task_types: bool,
    pub module_disabled_json: String,
    pub lifecycle: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskIndexSnapshot {
    pub task_id: String,
    pub task_no: u32,
    pub task_key: String,
    pub task_type: String,
    pub title: String,
    pub severity: String,
    pub priority: String,
    pub status: String,
    pub assigned_to: String,
    pub due_date: String,
    pub parent_task_id: Option<String>,
    pub links_json: String,
    pub comment_count: u32,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub resolution: Option<String>,
    pub resolution_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskIndexEvent {
    pub revision: i64,
    pub task_id: String,
    pub op: String,
    pub snapshot_json: String,
}

#[derive(Debug, Clone)]
pub struct IndexedTask {
    pub project_id: String,
    pub project_name: String,
    pub org_id: String,
    pub revision: i64,
    pub snapshot: TaskIndexSnapshot,
}

#[derive(Debug, Clone)]
pub struct TaskIndexFilter {
    pub task_type: String,
    pub status: String,
    pub assigned_to: String,
    pub search: String,
    pub severity: String,
    pub include_archived: bool,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct TaskIndexStatus {
    pub project_id: String,
    pub source_revision: i64,
    pub applied_revision: i64,
    pub lag: i64,
    pub indexed_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectTaskCounts {
    pub own_open: u32,
    pub descendant_open: u32,
    pub my_open: u32,
    pub overdue: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectImportNode {
    pub project_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub description: String,
    pub template: String,
    pub modules_json: String,
    pub owner_user_id: String,
    pub dir_path: String,
    pub key_prefix: String,
    pub is_private: bool,
    pub inherit_modules: bool,
    pub inherit_task_types: bool,
    pub status: String,
    pub lifecycle: String,
    pub functions: Vec<tentaflow_protocol::project_studio::access::ProjectFunctionWire>,
    pub members: Vec<MemberInput>,
}

#[derive(Debug, Clone)]
pub struct ProjectImportJournal {
    pub operation_id: String,
    pub org_id: String,
    pub manifest_sha256: String,
    pub nodes: Vec<ProjectImportNode>,
}

#[derive(Debug, Clone)]
pub struct ProjectDeletionAdmission {
    pub project: ProjectRecord,
    pub operation_id: String,
    pub actor_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskLocation {
    pub task_id: String,
    pub org_id: String,
    pub project_id: String,
    pub current_key: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskKeyAlias {
    pub org_id: String,
    pub alias_key: String,
    pub task_id: String,
    pub target_project_id: String,
    pub current_key: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskEventAlias {
    pub task_id: String,
    pub origin_project_id: String,
    pub origin_event_id: i64,
    pub current_project_id: String,
    pub current_event_id: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskLinkAlias {
    pub relation_id: String,
    pub origin_project_id: String,
    pub origin_link_id: i64,
    pub current_project_id: String,
    pub current_link_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRelationRoute {
    pub relation_id: String,
    pub owning_project_id: String,
    pub link_id: i64,
    pub source_task_id: String,
    pub target_task_id: String,
    pub source_project_id: String,
    pub target_project_id: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct TaskTransferJournal {
    pub operation_id: String,
    pub org_id: String,
    pub source_project_id: String,
    pub destination_project_id: String,
    pub actor_user_id: String,
    pub task_ids: Vec<String>,
    pub consent_wider_access: bool,
    pub phase: String,
    pub sha_manifest_json: String,
    pub key_map_json: String,
    pub event_map_json: String,
}

#[derive(Debug, Clone)]
pub struct MemberRecord {
    pub project_id: String,
    pub user_id: String,
    pub functions: Vec<String>,
    pub project_admin: bool,
    pub expires_at: Option<String>,
    pub invited_by: String,
    pub created_at: String,
}

impl MemberRecord {
    pub fn is_active_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.expires_at.as_ref().is_none_or(|expires_at| {
            chrono::DateTime::parse_from_rfc3339(expires_at).is_ok_and(|expires| expires > now)
        })
    }

    pub fn is_active(&self) -> bool {
        self.is_active_at(chrono::Utc::now())
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemberInput {
    pub user_id: String,
    pub functions: Vec<String>,
    pub project_admin: bool,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreatorGrantRecord {
    pub user_id: String,
    pub org_id: String,
    pub granted_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ChatRecord {
    pub chat_id: String,
    pub project_id: String,
    pub user_id: String,
    pub title: String,
    pub session_id: String,
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// Per-project rows (project.db)
// =============================================================================

#[derive(Debug, Clone)]
pub struct SourceRecord {
    pub source_id: String,
    pub kind: String,
    pub name: String,
    pub status: String,
    pub config_json: String,
    pub error: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

pub fn source_write_area(kind: &str) -> tentaflow_protocol::project_studio::access::ProjectArea {
    use tentaflow_protocol::project_studio::access::ProjectArea;
    if matches!(kind, "git" | "zip") {
        ProjectArea::Repos
    } else {
        ProjectArea::Knowledge
    }
}

/// Source row plus the aggregates the list screen needs (file/chunk counters
/// and the newest ingest job).
#[derive(Debug, Clone)]
pub struct SourceListItem {
    pub record: SourceRecord,
    pub file_count: u32,
    pub chunk_count: u32,
    pub last_job: Option<IngestJobRecord>,
}

#[derive(Debug, Clone)]
pub struct SourceFileRecord {
    pub file_id: String,
    pub source_id: String,
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub mime: String,
    pub status: String,
    pub error: String,
    pub chunk_count: u32,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct IngestJobRecord {
    pub job_id: String,
    pub source_id: String,
    pub status: String,
    pub files_total: u32,
    pub files_done: u32,
    pub chunks_done: u32,
    pub error: String,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActivityRecord {
    pub id: i64,
    pub actor_user_id: String,
    pub actor_kind: String,
    pub action: String,
    pub object_type: String,
    pub object_id: String,
    pub details_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct TagRecord {
    pub tag_id: String,
    pub name: String,
}

/// KPI counters for the overview screen (per-project part; `member_count` and
/// `my_chat_count` come from the central registry).
#[derive(Debug, Clone, Default)]
pub struct ProjectKpis {
    pub sources_total: u32,
    pub sources_ready: u32,
    pub files_total: u32,
    pub chunks_total: u32,
    pub open_ingest_jobs: u32,
}

/// F2 KPI counters (manual tests module) for the overview screen.
/// `my_run_items_pending` is caller-scoped: items assigned to the caller or
/// claimable from the pool inside running runs.
#[derive(Debug, Clone, Default)]
pub struct ProjectF2Kpis {
    pub cases_total: u32,
    pub cases_approved: u32,
    pub suites_total: u32,
    pub runs_open: u32,
    pub my_run_items_pending: u32,
    pub tasks_open: u32,
    pub defects_open: u32,
    pub generations_running: u32,
}

// =============================================================================
// F2 rows: manual test cases, suites, runs, tasks, generations (project.db)
// =============================================================================

#[derive(Debug, Clone)]
pub struct TestCaseRecord {
    pub case_id: String,
    pub kind: String,
    pub title: String,
    pub priority: String,
    pub status: String,
    pub status_reason: String,
    pub review_state: String,
    pub origin: String,
    pub generation_run_id: String,
    pub linked_sources_json: String,
    pub attachments_json: String,
    pub language: String,
    pub current_version: u32,
    pub content_json: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Case row plus the aggregates the list screen needs (tags + latest verdict).
#[derive(Debug, Clone)]
pub struct CaseListItem {
    pub record: TestCaseRecord,
    pub tag_ids: Vec<String>,
    pub last_result: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CaseVersionRecord {
    pub version: u32,
    pub content_json: String,
    pub change_note: String,
    pub created_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct SuiteRecord {
    pub suite_id: String,
    pub name: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub run_id: String,
    pub run_no: u32,
    pub name: String,
    pub suite_id: String,
    pub run_type: String,
    pub environment_id: String,
    pub env_note: String,
    pub assignment_mode: String,
    pub status: String,
    pub created_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Run result counters, always computed as SQL aggregates over run items
/// (never denormalized).
#[derive(Debug, Clone, Default)]
pub struct RunCounts {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub blocked: u32,
    pub skipped: u32,
    pub pending: u32,
    pub in_progress: u32,
}

#[derive(Debug, Clone)]
pub struct RunItemRecord {
    pub item_id: String,
    pub run_id: String,
    pub case_id: String,
    pub case_title: String,
    pub case_version: u32,
    pub position: u32,
    pub assigned_to: String,
    pub status: String,
    pub result_note: String,
    pub tester_config: String,
    pub duration_secs: u32,
    pub attachments_json: String,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
    pub steps_total: u32,
    pub steps_done: u32,
}

#[derive(Debug, Clone)]
pub struct RunStepRecord {
    pub step_index: u32,
    pub action: String,
    pub expected: String,
    pub status: String,
    pub note: String,
    pub attachments_json: String,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task_id: String,
    pub task_no: u32,
    pub task_key: String,
    pub task_type: String,
    pub title: String,
    pub description_md: String,
    pub severity: String,
    pub priority: String,
    pub status: String,
    pub assigned_to: String,
    pub due_date: String,
    pub parent_task_id: Option<String>,
    pub links_json: String,
    pub attachments_json: String,
    pub comment_count: u32,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub resolution: Option<String>,
    pub resolution_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskCommentRecord {
    pub comment_id: String,
    pub task_id: String,
    pub author_user_id: String,
    pub body_md: String,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub mention_user_ids_json: String,
}

#[derive(Debug, Clone)]
pub struct TaskTypeRecord {
    pub type_id: String,
    pub name: String,
    pub description: String,
    pub sort_order: i32,
    pub built_in: bool,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct TaskEventRecord {
    pub event_id: i64,
    pub task_id: String,
    pub at: String,
    pub actor_kind: String,
    pub actor_id: String,
    pub kind: String,
    pub before_json: String,
    pub after_json: String,
}

#[derive(Debug, Clone)]
pub struct TaskLinkRecord {
    pub link_id: i64,
    pub relation_id: String,
    pub source_task_id: String,
    pub target_task_id: String,
    pub source_project_id: String,
    pub target_project_id: String,
    pub kind: String,
    pub lag_days: i32,
    pub other_task_key: String,
    pub other_task_title: String,
}

#[derive(Debug, Clone)]
pub struct TaskStatusDurationRecord {
    pub status: String,
    pub entered_at: String,
    pub left_at: Option<String>,
    pub seconds: u64,
}

#[derive(Debug, Clone)]
pub struct GenerationRunRecord {
    pub gen_id: String,
    pub kind: String,
    pub status: String,
    pub agent_id: String,
    pub agent_run_id: String,
    pub source_ids_json: String,
    pub instructions: String,
    pub requested_count: u32,
    pub max_cases: u32,
    pub cases_generated: u32,
    pub cases_accepted: u32,
    pub cases_rejected: u32,
    pub error: String,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

// =============================================================================
// F3 rows: environments, build profiles, automated runs, artifacts (project.db)
// =============================================================================

/// Test environment. `secret_enc` holds the SettingsCipher ciphertext and never
/// leaves this layer — the wire only ever carries `has_secret`.
#[derive(Debug, Clone)]
pub struct EnvironmentRecord {
    pub environment_id: String,
    pub name: String,
    pub env_type: String,
    pub base_url: String,
    pub auth_type: String,
    pub secret_enc: String,
    pub extra_headers_json: String,
    pub host_allowlist_json: String,
    pub approval_status: String,
    pub approval_reason: String,
    pub is_private_address: bool,
    pub justification: String,
    pub requested_by: String,
    pub decided_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub decided_at: Option<String>,
}

/// Build/test recipe of one code source (git/zip), at most one per source.
#[derive(Debug, Clone)]
pub struct BuildProfileRecord {
    pub profile_id: String,
    pub source_id: String,
    pub toolchain: String,
    pub base_image: String,
    pub install_cmd: String,
    pub test_cmd: String,
    pub workdir: String,
    pub proposed_by: String,
}

/// Runner binding + watchdog state + perf aggregates of an automated run.
#[derive(Debug, Clone)]
pub struct AutoRunMetaRecord {
    pub run_id: String,
    pub environment_id: String,
    pub runner_service_id: String,
    pub runner_endpoint: String,
    pub runner_job_id: String,
    pub perf_profile_json: String,
    pub perf_summary_json: String,
    pub perf_timeline_json: String,
    pub last_poll_at: String,
    pub failed_polls: u32,
    pub watchdog_deadline_ms: i64,
}

/// One artifact produced by a runner, stored under
/// `<dir_path>/runs/<run_id>/<rel_path>`.
#[derive(Debug, Clone)]
pub struct RunArtifactRecord {
    pub artifact_id: String,
    pub run_id: String,
    pub item_id: String,
    pub name: String,
    pub kind: String,
    pub rel_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub mime: String,
}

/// One item of an automated run: the shared `test_run_items` row joined with
/// the case's kind/language and its artifact list.
#[derive(Debug, Clone)]
pub struct AutoRunItemRecord {
    pub item_id: String,
    pub case_id: String,
    pub case_title: String,
    pub kind: String,
    pub language: String,
    pub position: u32,
    pub status: String,
    pub duration_ms: u64,
    pub message: String,
    pub steps_total: u32,
    pub steps_done: u32,
}

/// F3 KPI counters (environments + automated runs) for the overview screen.
#[derive(Debug, Clone, Default)]
pub struct ProjectF3Kpis {
    pub environments_approved: u32,
    pub environments_pending: u32,
    pub auto_runs_open: u32,
}

// =============================================================================
// F4 rows: run schedules, trigger history, ML Studio links (project.db)
// =============================================================================

/// One run schedule. `next_run_at` is `None` for a schedule that will never
/// fire again (finished one-shot, disabled, breaker tripped) — the due query
/// compares on it, and an empty string would sort before every timestamp.
#[derive(Debug, Clone)]
pub struct ScheduleRecord {
    pub schedule_id: String,
    pub name: String,
    pub enabled: bool,
    pub auto_disabled: bool,
    pub run_type: String,
    pub suite_id: String,
    pub case_ids_json: String,
    pub environment_id: String,
    pub runner_service_id: String,
    pub perf_profile_json: String,
    pub assignment_mode: String,
    pub assignees_json: String,
    pub schedule_kind: String,
    pub schedule_expr: String,
    pub timezone: String,
    pub next_run_at: Option<String>,
    pub last_trigger_at: String,
    pub last_run_id: String,
    pub last_status: String,
    pub last_reason: String,
    pub consecutive_failures: u32,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One trigger attempt. Attempts that started nothing are recorded too, so an
/// admin can tell "never fired" from "fired and refused".
#[derive(Debug, Clone)]
pub struct ScheduleRunRecord {
    pub trigger_id: String,
    pub schedule_id: String,
    pub scheduled_for: String,
    pub fired_at: String,
    pub outcome: String,
    pub reason: String,
    pub run_id: String,
    pub run_status: String,
    pub actor: String,
}

/// Link between this project and an ML Studio project.
#[derive(Debug, Clone)]
pub struct MlLinkRecord {
    pub link_id: String,
    pub ml_project_id: String,
    pub label: String,
    pub origin: String,
    pub sync_permissions: bool,
    pub role_map_json: String,
    pub last_sync_at: String,
    pub last_sync_result: String,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// F4 KPI counters (schedules + ML links) for the overview screen.
#[derive(Debug, Clone, Default)]
pub struct ProjectF4Kpis {
    pub schedules_enabled: u32,
    /// Enabled schedules that cannot fire: the breaker tripped, or the bound
    /// environment is not approved.
    pub schedules_blocked: u32,
    pub ml_links: u32,
}

/// Personal notification row from the CENTRAL registry (projects.db) —
/// always queried WHERE user_id = caller.
#[derive(Debug, Clone)]
pub struct NotificationRecord {
    pub notification_id: String,
    pub project_id: String,
    pub project_name: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link_json: String,
    pub read_at: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate_project_access(
        project: &ProjectRecord,
        member: Option<&MemberRecord>,
        catalogue: &[tentaflow_protocol::project_studio::access::ProjectFunctionWire],
        app_admin: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> tentaflow_protocol::project_studio::access::ProjectAccessWire {
        super::evaluate_project_access(project, member, catalogue, app_admin, now, false)
    }
    use chrono::{TimeZone, Utc};
    use tentaflow_protocol::project_studio::access::{
        ProjectArea, ProjectPermissionLevel as Level,
    };

    fn project() -> ProjectRecord {
        ProjectRecord {
            project_id: "p".to_string(),
            org_id: "o".to_string(),
            key_prefix: "AC".to_string(),
            name: "Access".to_string(),
            description: String::new(),
            status: "active".to_string(),
            template: "custom".to_string(),
            modules_json: "[\"tasks\",\"tests\",\"knowledge\",\"docs\",\"chat\",\"security\"]"
                .to_string(),
            owner_user_id: "owner".to_string(),
            dir_path: "unused".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            parent_id: None,
            path: "/p".to_string(),
            depth: 1,
            is_private: false,
            inherit_modules: false,
            inherit_task_types: false,
            module_disabled_json: "[]".to_string(),
            lifecycle: "active".to_string(),
            ended_at: None,
        }
    }

    fn member(functions: &[&str], project_admin: bool) -> MemberRecord {
        MemberRecord {
            project_id: "p".to_string(),
            user_id: "member".to_string(),
            functions: functions.iter().map(|value| value.to_string()).collect(),
            project_admin,
            expires_at: None,
            invited_by: "owner".to_string(),
            created_at: String::new(),
        }
    }

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, 12, 0, 0)
            .single()
            .expect("instant")
    }

    #[test]
    fn functions_take_the_maximum_grant_for_each_area() {
        let catalogue = default_project_functions();
        assert_eq!(catalogue.len(), 9);
        let developer = evaluate_project_access(
            &project(),
            Some(&member(&["developer"], false)),
            &catalogue,
            false,
            now(),
        );
        assert_eq!(developer.level(ProjectArea::Tests), Level::Read);
        assert_eq!(developer.level(ProjectArea::Repos), Level::Write);
        let combined = evaluate_project_access(
            &project(),
            Some(&member(&["developer", "tester"], false)),
            &catalogue,
            false,
            now(),
        );
        assert_eq!(combined.level(ProjectArea::Tests), Level::Admin);
        assert_eq!(combined.level(ProjectArea::Repos), Level::Write);
        assert!(combined.allows(ProjectArea::Tests, Level::Write));
        assert!(!combined.allows(ProjectArea::Settings, Level::Write));
    }

    #[test]
    fn project_admin_requires_explicit_confidential_grants_and_enabled_modules() {
        let catalogue = default_project_functions();
        let admin = member(&["developer"], true);
        let access = evaluate_project_access(&project(), Some(&admin), &catalogue, false, now());
        assert_eq!(access.level(ProjectArea::Tests), Level::Admin);
        assert_eq!(access.level(ProjectArea::SecurityConfidential), Level::None);
        assert!(access.can_manage_members);
        let security_admin = member(&["developer", "security"], true);
        let explicit =
            evaluate_project_access(&project(), Some(&security_admin), &catalogue, false, now());
        assert_eq!(
            explicit.level(ProjectArea::SecurityConfidential),
            Level::Admin
        );
        let mut disabled = project();
        disabled.modules_json = "[\"knowledge\"]".to_string();
        let access =
            evaluate_project_access(&disabled, Some(&security_admin), &catalogue, false, now());
        assert!(!access.allows(ProjectArea::Tests, Level::Read));
        assert!(!access.allows(ProjectArea::SecurityConfidential, Level::Read));
        assert!(!access.can_create_tasks);
    }

    #[test]
    fn app_admin_inspection_does_not_grant_mutation_or_confidential_access() {
        let access =
            evaluate_project_access(&project(), None, &default_project_functions(), true, now());
        assert!(access.has_access);
        assert!(access.allows(ProjectArea::Knowledge, Level::Read));
        assert!(!access.allows(ProjectArea::Knowledge, Level::Write));
        assert!(!access.can_manage_members);
        assert!(!access.can_manage_settings);
        assert!(!access.allows(ProjectArea::SecurityConfidential, Level::Read));
        assert!(access.can_create_tasks);
    }

    #[test]
    fn membership_without_functions_can_create_tasks_but_has_no_content_grants() {
        let empty_member = member(&[], false);
        let access = evaluate_project_access(
            &project(),
            Some(&empty_member),
            &default_project_functions(),
            false,
            now(),
        );
        assert!(access.has_access);
        assert!(access.can_create_tasks);
        assert_eq!(access.level(ProjectArea::Tasks), Level::None);
        assert!(!access.allows(ProjectArea::Tasks, Level::Read));
        let no_member =
            evaluate_project_access(&project(), None, &default_project_functions(), false, now());
        assert!(!no_member.has_access);
        assert!(!no_member.can_create_tasks);
    }

    #[test]
    fn expiry_is_enforced_at_the_instant_and_invalid_expiry_fails_closed() {
        let mut temporary = member(&["tester"], true);
        temporary.expires_at = Some("2026-09-30T14:00:00+02:00".to_string());
        assert!(!temporary.is_active_at(now()));
        let access = evaluate_project_access(
            &project(),
            Some(&temporary),
            &default_project_functions(),
            false,
            now(),
        );
        assert!(!access.has_access);
        assert!(!access.project_admin);
        assert!(!access.can_create_tasks);
        temporary.expires_at = Some("2026-09-30T12:00:01Z".to_string());
        assert!(temporary.is_active_at(now()));
        temporary.expires_at = Some("invalid".to_string());
        assert!(!temporary.is_active_at(now()));
    }

    #[test]
    fn archived_project_access_is_read_only_and_ownership_is_not_admin() {
        let mut owned = member(&["observer"], false);
        owned.user_id = "owner".to_string();
        let access = evaluate_project_access(
            &project(),
            Some(&owned),
            &default_project_functions(),
            false,
            now(),
        );
        assert!(access.is_owner);
        assert!(!access.project_admin);
        assert!(!access.can_manage_members);
        let mut archived = project();
        archived.status = "archived".to_string();
        let access = evaluate_project_access(
            &archived,
            Some(&member(&["tester"], true)),
            &default_project_functions(),
            false,
            now(),
        );
        assert!(access.allows(ProjectArea::Tests, Level::Read));
        assert!(!access.allows(ProjectArea::Tests, Level::Write));
        assert!(!access.can_manage_members);
        assert!(!access.can_create_tasks);
        assert!(!access.can_manage_settings);
    }

    #[test]
    fn membership_validation_rejects_unknown_duplicate_and_expired_values() {
        let mut input = MemberInput {
            user_id: "member".to_string(),
            functions: vec!["developer".to_string()],
            project_admin: false,
            expires_at: Some("2026-10-01T14:00:00+02:00".to_string()),
        };
        let catalogue = default_project_functions();
        let validated = validate_member_input(&input, &catalogue, now()).expect("valid");
        assert_eq!(
            validated.expires_at.as_deref(),
            Some("2026-10-01T12:00:00Z")
        );
        input.functions.push("developer".to_string());
        assert!(validate_member_input(&input, &catalogue, now()).is_err());
        input.functions = vec!["unknown".to_string()];
        assert!(validate_member_input(&input, &catalogue, now()).is_err());
        input.functions.clear();
        input.expires_at = Some(now().to_rfc3339());
        assert!(validate_member_input(&input, &catalogue, now()).is_err());
    }
}
