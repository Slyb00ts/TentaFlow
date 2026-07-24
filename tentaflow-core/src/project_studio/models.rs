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
