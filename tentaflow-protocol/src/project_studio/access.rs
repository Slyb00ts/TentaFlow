// ============ File: project_studio/access.rs — project functions and authoritative area access ============

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPermissionLevel {
    #[default]
    None,
    Read,
    Write,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectArea {
    Tasks,
    Board,
    Sprints,
    Roadmap,
    Modules,
    Changelog,
    Releases,
    Tests,
    Environments,
    Repos,
    Knowledge,
    Docs,
    Chat,
    Security,
    #[serde(rename = "security.confidential")]
    SecurityConfidential,
    Settings,
}

impl ProjectArea {
    pub const ALL: [Self; 16] = [
        Self::Tasks,
        Self::Board,
        Self::Sprints,
        Self::Roadmap,
        Self::Modules,
        Self::Changelog,
        Self::Releases,
        Self::Tests,
        Self::Environments,
        Self::Repos,
        Self::Knowledge,
        Self::Docs,
        Self::Chat,
        Self::Security,
        Self::SecurityConfidential,
        Self::Settings,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Board => "board",
            Self::Sprints => "sprints",
            Self::Roadmap => "roadmap",
            Self::Modules => "modules",
            Self::Changelog => "changelog",
            Self::Releases => "releases",
            Self::Tests => "tests",
            Self::Environments => "environments",
            Self::Repos => "repos",
            Self::Knowledge => "knowledge",
            Self::Docs => "docs",
            Self::Chat => "chat",
            Self::Security => "security",
            Self::SecurityConfidential => "security.confidential",
            Self::Settings => "settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAreaGrantWire {
    pub area: ProjectArea,
    pub level: ProjectPermissionLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFunctionWire {
    pub function_id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub grants: Vec<ProjectAreaGrantWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAreaAccessWire {
    pub area: ProjectArea,
    pub level: ProjectPermissionLevel,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAccessWire {
    pub has_access: bool,
    pub project_admin: bool,
    pub app_admin: bool,
    pub is_owner: bool,
    pub archived: bool,
    pub functions: Vec<String>,
    pub expires_at: Option<String>,
    pub enabled_modules: Vec<String>,
    pub areas: Vec<ProjectAreaAccessWire>,
    pub can_create_tasks: bool,
    pub can_manage_members: bool,
    pub can_manage_settings: bool,
}

impl ProjectAccessWire {
    pub fn level(&self, area: ProjectArea) -> ProjectPermissionLevel {
        self.areas
            .iter()
            .find(|entry| entry.area == area && entry.enabled)
            .map_or(ProjectPermissionLevel::None, |entry| entry.level)
    }

    pub fn allows(&self, area: ProjectArea, minimum: ProjectPermissionLevel) -> bool {
        self.has_access
            && (!self.archived || minimum <= ProjectPermissionLevel::Read)
            && self
                .areas
                .iter()
                .any(|entry| entry.area == area && entry.enabled && entry.level >= minimum)
    }
}

/// Nested access operations keep the parent payload's pinned variants intact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectAccessPayload {
    CatalogueGetRequest {
        project_id: String,
    },
    CatalogueGetResponse {
        functions: Vec<ProjectFunctionWire>,
    },
    FunctionSaveRequest {
        project_id: String,
        function: ProjectFunctionWire,
    },
    FunctionSaveResult {
        ok: bool,
    },
    FunctionDeleteRequest {
        project_id: String,
        function_id: String,
    },
    FunctionDeleteResult {
        ok: bool,
    },
    MemberAccessSetRequest {
        project_id: String,
        user_id: String,
        functions: Vec<String>,
        project_admin: bool,
        expires_at: Option<String>,
    },
    MemberAccessSetResult {
        ok: bool,
    },
}
