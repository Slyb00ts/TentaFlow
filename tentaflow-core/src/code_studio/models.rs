// ===== File: code_studio/models.rs — row types and enums of the Code Studio registry =====
//
// Plain records mirroring the SQLite rows of migration 125, plus the small
// lattices the authorization gates compare against. Every enum here is parsed
// from and rendered to the exact strings the table CHECK constraints allow, so
// a value that round-trips through the database cannot become unknown.

/// Workspace member role. Ordered lattice: viewer < editor < owner — a gate
/// requiring `Editor` accepts editor and owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkspaceRole {
    Viewer,
    Editor,
    Owner,
}

impl WorkspaceRole {
    pub fn slug(self) -> &'static str {
        match self {
            WorkspaceRole::Viewer => "viewer",
            WorkspaceRole::Editor => "editor",
            WorkspaceRole::Owner => "owner",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "viewer" => Some(WorkspaceRole::Viewer),
            "editor" => Some(WorkspaceRole::Editor),
            "owner" => Some(WorkspaceRole::Owner),
            _ => None,
        }
    }
}

/// How the workspace executes code.
///
/// `ProcessSandbox` uses the native OS confinement mechanism; `Container`
/// uses a container runtime. `TrustedNative` deliberately provides no host
/// isolation and runs with the TentaFlow service user rights. The
/// mode is immutable after creation — everything downstream (mount profiles,
/// autonomy ceiling, egress enforcement) is derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    Container,
    ProcessSandbox,
    TrustedNative,
}

impl ExecMode {
    pub fn slug(self) -> &'static str {
        match self {
            ExecMode::Container => "container",
            ExecMode::ProcessSandbox => "process_sandbox",
            ExecMode::TrustedNative => "trusted_native",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "container" => Some(ExecMode::Container),
            "process_sandbox" => Some(ExecMode::ProcessSandbox),
            "trusted_native" => Some(ExecMode::TrustedNative),
            _ => None,
        }
    }
}

/// How network policy is REALLY enforced on the owner node. Computed from node
/// capabilities at creation time; `Unrestricted` promises neither filtering nor
/// host auditing, and exists so the UI can say so instead of implying a
/// guarantee the node cannot keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressEnforcement {
    ProcessSandbox,
    Namespace,
    Firewall,
    Unrestricted,
}

impl EgressEnforcement {
    pub fn slug(self) -> &'static str {
        match self {
            EgressEnforcement::ProcessSandbox => "process_sandbox",
            EgressEnforcement::Namespace => "namespace",
            EgressEnforcement::Firewall => "firewall",
            EgressEnforcement::Unrestricted => "unrestricted",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "process_sandbox" => Some(EgressEnforcement::ProcessSandbox),
            "namespace" => Some(EgressEnforcement::Namespace),
            "firewall" => Some(EgressEnforcement::Firewall),
            "unrestricted" => Some(EgressEnforcement::Unrestricted),
            _ => None,
        }
    }
}

/// What the agent may do without asking. Ordered: plan < normal < auto_edit <
/// autonomous. A session never exceeds its workspace's `autonomy_ceiling`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutonomyMode {
    Plan,
    Normal,
    AutoEdit,
    Autonomous,
}

impl AutonomyMode {
    pub fn slug(self) -> &'static str {
        match self {
            AutonomyMode::Plan => "plan",
            AutonomyMode::Normal => "normal",
            AutonomyMode::AutoEdit => "auto_edit",
            AutonomyMode::Autonomous => "autonomous",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "plan" => Some(AutonomyMode::Plan),
            "normal" => Some(AutonomyMode::Normal),
            "auto_edit" => Some(AutonomyMode::AutoEdit),
            "autonomous" => Some(AutonomyMode::Autonomous),
            _ => None,
        }
    }
}

/// Lifecycle of a workspace. `Provisioning` is not cosmetic: the directory,
/// the runtime database and the clone are created by a saga, and a crash in
/// the middle leaves `Error` with a resumable step list rather than a
/// half-built workspace pretending to be usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceStatus {
    Provisioning,
    Active,
    Error,
    Archived,
    Deleted,
}

impl WorkspaceStatus {
    pub fn slug(self) -> &'static str {
        match self {
            WorkspaceStatus::Provisioning => "provisioning",
            WorkspaceStatus::Active => "active",
            WorkspaceStatus::Error => "error",
            WorkspaceStatus::Archived => "archived",
            WorkspaceStatus::Deleted => "deleted",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "provisioning" => Some(WorkspaceStatus::Provisioning),
            "active" => Some(WorkspaceStatus::Active),
            "error" => Some(WorkspaceStatus::Error),
            "archived" => Some(WorkspaceStatus::Archived),
            "deleted" => Some(WorkspaceStatus::Deleted),
            _ => None,
        }
    }
}

/// Outcome of one provisioning step. `Compensated` is distinct from `Failed`:
/// it means the step ran, failed, and its effect was undone — so a resume must
/// redo it, while a `Failed` step may still hold a partial effect to inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaStepStatus {
    Pending,
    Done,
    Failed,
    Compensated,
}

impl SagaStepStatus {
    pub fn slug(self) -> &'static str {
        match self {
            SagaStepStatus::Pending => "pending",
            SagaStepStatus::Done => "done",
            SagaStepStatus::Failed => "failed",
            SagaStepStatus::Compensated => "compensated",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "pending" => Some(SagaStepStatus::Pending),
            "done" => Some(SagaStepStatus::Done),
            "failed" => Some(SagaStepStatus::Failed),
            "compensated" => Some(SagaStepStatus::Compensated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceRecord {
    pub id: String,
    pub org_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub slug: String,
    pub node_id: String,
    pub exec_mode: String,
    pub container_image: Option<String>,
    pub egress_enforcement: String,
    pub repo_kind: String,
    pub repo_url: Option<String>,
    pub repo_auth_kind: Option<String>,
    /// HANDLE into the node-local vault, never the material itself.
    pub secret_ref: Option<String>,
    pub ssh_host_fingerprint: Option<String>,
    pub default_branch: Option<String>,
    pub target_branch: Option<String>,
    pub autonomy_ceiling: String,
    pub egress_policy: String,
    pub index_enabled: bool,
    pub quota_disk_bytes: Option<i64>,
    pub quota_sessions: Option<i64>,
    pub status: String,
    pub status_detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Everything the caller chooses when creating a workspace. The directory is
/// NOT among them: the system derives it from the id, so a user can never
/// point a workspace at an arbitrary host path.
#[derive(Debug, Clone)]
pub struct NewWorkspace {
    pub id: String,
    pub org_id: String,
    pub owner_user_id: String,
    pub name: String,
    pub slug: String,
    pub node_id: String,
    pub exec_mode: ExecMode,
    pub container_image: Option<String>,
    pub egress_enforcement: EgressEnforcement,
    pub repo_kind: String,
    pub repo_url: Option<String>,
    pub repo_auth_kind: Option<String>,
    pub secret_ref: Option<String>,
    pub ssh_host_fingerprint: Option<String>,
    pub default_branch: Option<String>,
    pub target_branch: Option<String>,
    pub autonomy_ceiling: AutonomyMode,
    pub egress_policy: String,
    pub index_enabled: bool,
    pub quota_disk_bytes: Option<i64>,
    pub quota_sessions: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceMemberRecord {
    pub workspace_id: String,
    pub user_id: String,
    pub role: String,
    pub added_by: String,
    pub added_at: String,
}

#[derive(Debug, Clone)]
pub struct SagaStepRecord {
    pub workspace_id: String,
    pub step: String,
    pub status: String,
    pub detail: Option<String>,
    pub updated_at: String,
}
