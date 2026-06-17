// ===== File: ml_studio/models.rs — ML Studio domain types (projects, types) =====

use serde::{Deserialize, Serialize};

/// The fixed set of ML Studio project types. The string slug is the stable
/// machine value persisted in `projects.project_type` and branched on by
/// flows/handlers; UI labels live in `label_pl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectType {
    Recognition,
    FtLlm,
    FtVisionAudio,
    TabularAnomaly,
    Distillation,
}

impl ProjectType {
    /// All types in wizard display order.
    pub const ALL: [ProjectType; 5] = [
        ProjectType::Recognition,
        ProjectType::FtLlm,
        ProjectType::FtVisionAudio,
        ProjectType::TabularAnomaly,
        ProjectType::Distillation,
    ];

    /// Stable machine slug stored in SQLite and carried on the wire.
    pub fn slug(self) -> &'static str {
        match self {
            ProjectType::Recognition => "recognition",
            ProjectType::FtLlm => "ft_llm",
            ProjectType::FtVisionAudio => "ft_vision_audio",
            ProjectType::TabularAnomaly => "tabular_anomaly",
            ProjectType::Distillation => "distillation",
        }
    }

    /// Polish UI label shown in the project wizard / cards.
    pub fn label_pl(self) -> &'static str {
        match self {
            ProjectType::Recognition => "Rozpoznawanie obrazu",
            ProjectType::FtLlm => "Fine-tuning LLM",
            ProjectType::FtVisionAudio => "Fine-tuning vision/audio",
            ProjectType::TabularAnomaly => "Dane tabelaryczne i anomalie",
            ProjectType::Distillation => "Destylacja modelu",
        }
    }

    /// Short Polish description for the wizard tile.
    pub fn description_pl(self) -> &'static str {
        match self {
            ProjectType::Recognition => {
                "Detekcja i klasyfikacja obiektow na zdjeciach z anotacja i ewaluacja mAP."
            }
            ProjectType::FtLlm => {
                "Trening LLM metodami SFT/LoRA/QLoRA/DoRA oraz DPO z eksportem GGUF."
            }
            ProjectType::FtVisionAudio => {
                "Fine-tuning modeli wizyjnych i audio (np. ASR) z metrykami modalnymi."
            }
            ProjectType::TabularAnomaly => {
                "AutoML dla danych tabelarycznych oraz wykrywanie anomalii."
            }
            ProjectType::Distillation => {
                "Destylacja wiedzy z modelu nauczyciela do mniejszego modelu ucznia."
            }
        }
    }

    /// Parses a machine slug back into a `ProjectType`.
    pub fn from_slug(slug: &str) -> Option<ProjectType> {
        ProjectType::ALL.into_iter().find(|t| t.slug() == slug)
    }
}

/// Per-project membership role. The string slug is the stable value persisted
/// in `project_members.role`. `owner` is the project creator (source of truth in
/// `projects.owner_user_id`); `editor`/`viewer` are the roles an owner may grant
/// through an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectRole {
    Owner,
    Editor,
    Viewer,
}

impl ProjectRole {
    /// Stable machine slug stored in SQLite and carried on the wire.
    pub fn slug(self) -> &'static str {
        match self {
            ProjectRole::Owner => "owner",
            ProjectRole::Editor => "editor",
            ProjectRole::Viewer => "viewer",
        }
    }

    /// Parses a machine slug back into a `ProjectRole`.
    pub fn from_slug(slug: &str) -> Option<ProjectRole> {
        match slug {
            "owner" => Some(ProjectRole::Owner),
            "editor" => Some(ProjectRole::Editor),
            "viewer" => Some(ProjectRole::Viewer),
            _ => None,
        }
    }

    /// Roles an owner may assign through an invitation or role change. Excludes
    /// `owner`, which is fixed to the project creator.
    pub fn from_grantable_slug(slug: &str) -> Option<ProjectRole> {
        match ProjectRole::from_slug(slug) {
            Some(ProjectRole::Editor) => Some(ProjectRole::Editor),
            Some(ProjectRole::Viewer) => Some(ProjectRole::Viewer),
            _ => None,
        }
    }
}

/// Membership lifecycle state. Inviting a user creates an immediately `active`
/// member (there is no acceptance step — only Power Users are invited and they
/// gain access at once), so `active` is the only state a membership row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberStatus {
    Active,
}

impl MemberStatus {
    /// Stable machine slug stored in SQLite and carried on the wire.
    pub fn slug(self) -> &'static str {
        match self {
            MemberStatus::Active => "active",
        }
    }
}

/// One membership row from `project_members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMember {
    pub project_id: String,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub invited_by: String,
    pub created_at: String,
}

/// Full project record as stored in `projects`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub project_type: String,
    pub status: String,
    pub owner_user_id: String,
    pub org_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One dataset row from `datasets`, including the stored profiling JSON. The
/// `profile_json` carries a serialized `profile::TableProfile`; an empty
/// dataset (no successful profile) keeps the `'{}'` default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub dataset_id: String,
    pub project_id: String,
    pub name: String,
    pub kind: String,
    pub row_count: u64,
    pub column_count: u32,
    pub profile_json: String,
    pub created_at: String,
}

/// One admin-managed mesh resource grant (§11.3). A record of an allocation,
/// not live usage. `subject_kind` is one of `user`/`group`/`project`;
/// `resource_kind` is one of `gpu`/`cpu`/`ram`. `resource_ref` identifies the
/// card (e.g. GPU name/index) and is empty for cpu/ram. `quota` is free-form
/// text (GPU count, hours, or empty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceGrant {
    pub grant_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    pub node_id: String,
    pub resource_kind: String,
    pub resource_ref: String,
    pub quota: String,
    pub granted_by: String,
    pub created_at: String,
}

/// One training-run row from `training_runs`, used by the project overview tab.
/// `model_id`/`started_at`/`finished_at` are NULL until the run produces a model
/// or changes state, hence the `Option<String>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingRunSummary {
    pub run_id: String,
    pub model_id: Option<String>,
    pub status: String,
    pub config_json: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One model row from `models`, used by the project overview tab. `metrics_json`
/// carries the serialized metric snapshot for the model card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    pub model_id: String,
    pub name: String,
    pub framework: String,
    pub base_model: String,
    pub status: String,
    pub metrics_json: String,
    pub created_at: String,
}

/// Allowed `subject_kind` values for a resource grant.
pub const GRANT_SUBJECT_KINDS: [&str; 3] = ["user", "group", "project"];

/// Allowed `resource_kind` values for a resource grant.
pub const GRANT_RESOURCE_KINDS: [&str; 3] = ["gpu", "cpu", "ram"];

/// Project plus its per-project KPIs (dataset/model counts), used to build
/// list/detail responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project: Project,
    pub model_count: u32,
    pub dataset_count: u32,
    pub training_count: u32,
    /// Role of the requesting user within this project (`owner`/`editor`/`viewer`).
    pub role: String,
    /// Convenience flag: the requesting user is the project owner.
    pub is_owner: bool,
}
