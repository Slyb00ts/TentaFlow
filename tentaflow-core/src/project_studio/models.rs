// ===== File: project_studio/models.rs — row types + role hierarchy for Project Studio =====
//
// Plain data records mirroring the SQLite rows (central `projects.db` and the
// per-project `project.db`), plus the project role lattice used by every
// authorization gate in `dispatch/project_studio.rs`.

/// Project member role. Ordered lattice: viewer < tester < editor < manager <
/// owner — a gate requiring `Editor` accepts editor, manager and owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectRole {
    Viewer,
    Tester,
    Editor,
    Manager,
    Owner,
}

impl ProjectRole {
    pub fn slug(self) -> &'static str {
        match self {
            ProjectRole::Viewer => "viewer",
            ProjectRole::Tester => "tester",
            ProjectRole::Editor => "editor",
            ProjectRole::Manager => "manager",
            ProjectRole::Owner => "owner",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "viewer" => Some(ProjectRole::Viewer),
            "tester" => Some(ProjectRole::Tester),
            "editor" => Some(ProjectRole::Editor),
            "manager" => Some(ProjectRole::Manager),
            "owner" => Some(ProjectRole::Owner),
            _ => None,
        }
    }
}

// =============================================================================
// Central registry rows (projects.db)
// =============================================================================

#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub project_id: String,
    pub org_id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub template: String,
    pub modules_json: String,
    pub owner_user_id: String,
    pub dir_path: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct MemberRecord {
    pub project_id: String,
    pub user_id: String,
    pub role: String,
    pub invited_by: String,
    pub created_at: String,
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
    pub task_type: String,
    pub title: String,
    pub description_md: String,
    pub severity: String,
    pub priority: String,
    pub status: String,
    pub assigned_to: String,
    pub due_date: String,
    pub links_json: String,
    pub attachments_json: String,
    pub comment_count: u32,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct TaskCommentRecord {
    pub comment_id: String,
    pub task_id: String,
    pub author_user_id: String,
    pub body_md: String,
    pub created_at: String,
    pub edited_at: Option<String>,
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

    #[test]
    fn role_lattice_orders_viewer_to_owner() {
        assert!(ProjectRole::Viewer < ProjectRole::Tester);
        assert!(ProjectRole::Tester < ProjectRole::Editor);
        assert!(ProjectRole::Editor < ProjectRole::Manager);
        assert!(ProjectRole::Manager < ProjectRole::Owner);
        assert_eq!(
            ProjectRole::from_slug("manager"),
            Some(ProjectRole::Manager)
        );
        assert_eq!(ProjectRole::from_slug("root"), None);
        assert_eq!(ProjectRole::Owner.slug(), "owner");
    }
}
