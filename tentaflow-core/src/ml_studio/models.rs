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
    Rag,
    Distillation,
}

impl ProjectType {
    /// All six types in wizard display order.
    pub const ALL: [ProjectType; 6] = [
        ProjectType::Recognition,
        ProjectType::FtLlm,
        ProjectType::FtVisionAudio,
        ProjectType::TabularAnomaly,
        ProjectType::Rag,
        ProjectType::Distillation,
    ];

    /// Stable machine slug stored in SQLite and carried on the wire.
    pub fn slug(self) -> &'static str {
        match self {
            ProjectType::Recognition => "recognition",
            ProjectType::FtLlm => "ft_llm",
            ProjectType::FtVisionAudio => "ft_vision_audio",
            ProjectType::TabularAnomaly => "tabular_anomaly",
            ProjectType::Rag => "rag",
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
            ProjectType::Rag => "RAG (wyszukiwanie z kontekstem)",
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
            ProjectType::Rag => {
                "Budowa korpusu, chunking, embeddingi i indeks wektorowy z playgroundem."
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

/// Project plus its per-project KPIs (dataset/model counts), used to build
/// list/detail responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project: Project,
    pub model_count: u32,
    pub dataset_count: u32,
}
