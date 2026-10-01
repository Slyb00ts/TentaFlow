// ============ File: project_studio/models/access.rs — canonical function matrix and access evaluation ============

use std::collections::HashSet;

use anyhow::{bail, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use tentaflow_protocol::project_studio::access::{
    ProjectAccessWire, ProjectArea, ProjectAreaAccessWire, ProjectAreaGrantWire,
    ProjectFunctionWire, ProjectPermissionLevel,
};

use super::{MemberInput, MemberRecord, ProjectRecord};

/// Defaults are project-owned rows after creation; changing one project never
/// changes the template or another project's catalogue.
pub fn default_project_functions() -> Vec<ProjectFunctionWire> {
    use ProjectPermissionLevel::{Admin as A, None as N, Read as R, Write as W};
    let columns = [
        (
            "pm",
            "PM / Product Owner",
            "Project planning and acceptance",
        ),
        ("analyst", "Analyst", "Requirements and acceptance criteria"),
        (
            "designer",
            "UI / UX Designer",
            "Interface and experience design",
        ),
        ("developer", "Developer", "Implementation and code review"),
        ("tester", "Tester", "Manual and automated testing"),
        (
            "security",
            "Security",
            "Security review and vulnerability triage",
        ),
        ("devops", "DevOps", "Environments and delivery integrations"),
        (
            "release_manager",
            "Release Manager",
            "Release planning and approval",
        ),
        (
            "observer",
            "Observer / Client",
            "Progress and published results",
        ),
    ];
    let matrix = [
        (ProjectArea::Tasks, [A, W, W, W, W, W, R, R, R]),
        (ProjectArea::Board, [A, W, W, W, W, W, R, R, R]),
        (ProjectArea::Sprints, [A, R, R, R, R, R, R, R, R]),
        (ProjectArea::Roadmap, [A, W, R, R, R, R, R, R, R]),
        (ProjectArea::Modules, [A, R, R, R, R, R, W, R, N]),
        (ProjectArea::Changelog, [W, W, R, W, W, R, R, A, R]),
        (ProjectArea::Releases, [W, R, R, R, R, R, R, A, R]),
        (ProjectArea::Tests, [R, R, N, R, A, W, R, R, N]),
        (ProjectArea::Environments, [R, N, R, W, W, W, A, R, N]),
        (ProjectArea::Repos, [N, N, N, W, R, R, A, R, N]),
        (ProjectArea::Knowledge, [W, W, W, W, R, R, R, R, R]),
        (ProjectArea::Docs, [W, W, W, W, R, R, R, R, R]),
        (ProjectArea::Chat, [W, W, W, W, W, W, W, W, W]),
        (ProjectArea::Security, [R, N, N, R, R, A, R, R, N]),
        (
            ProjectArea::SecurityConfidential,
            [N, N, N, N, N, A, N, N, N],
        ),
        (ProjectArea::Settings, [W, N, N, N, N, N, W, N, N]),
    ];
    columns
        .iter()
        .enumerate()
        .map(|(column, (id, name, description))| ProjectFunctionWire {
            function_id: (*id).to_string(),
            name: (*name).to_string(),
            description: (*description).to_string(),
            builtin: true,
            grants: matrix
                .iter()
                .map(|(area, levels)| ProjectAreaGrantWire {
                    area: *area,
                    level: levels[column],
                })
                .collect(),
        })
        .collect()
}

pub fn function_level(
    functions: &[String],
    catalogue: &[ProjectFunctionWire],
    area: ProjectArea,
) -> ProjectPermissionLevel {
    catalogue
        .iter()
        .filter(|function| functions.contains(&function.function_id))
        .flat_map(|function| &function.grants)
        .filter(|grant| grant.area == area)
        .map(|grant| grant.level)
        .max()
        .unwrap_or_default()
}

pub fn validate_member_input(
    input: &MemberInput,
    catalogue: &[ProjectFunctionWire],
    now: DateTime<Utc>,
) -> Result<MemberInput> {
    if input.user_id.trim().is_empty() {
        bail!("member user_id is required");
    }
    if input.functions.len() > catalogue.len() {
        bail!("too many member functions");
    }
    let mut unique = HashSet::new();
    for id in &input.functions {
        if !unique.insert(id) {
            bail!("duplicate member function: {id}");
        }
        if !catalogue.iter().any(|function| function.function_id == *id) {
            bail!("unknown project function: {id}");
        }
    }
    let expires_at = input
        .expires_at
        .as_ref()
        .map(|value| -> Result<String> {
            let instant = DateTime::parse_from_rfc3339(value)
                .map_err(|_| anyhow::anyhow!("membership expiry must be an RFC3339 instant"))?
                .with_timezone(&Utc);
            if instant <= now {
                bail!("membership expiry must be in the future");
            }
            Ok(instant.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        })
        .transpose()?;
    Ok(MemberInput {
        user_id: input.user_id.clone(),
        functions: input.functions.clone(),
        project_admin: input.project_admin,
        expires_at,
    })
}

pub fn evaluate_project_access(
    project: &ProjectRecord,
    member: Option<&MemberRecord>,
    catalogue: &[ProjectFunctionWire],
    app_admin: bool,
    now: DateTime<Utc>,
) -> ProjectAccessWire {
    use ProjectPermissionLevel::{Admin, None, Read, Write};
    let active_member =
        member.filter(|member| member.project_id == project.project_id && member.is_active_at(now));
    let functions = active_member.map_or_else(Vec::new, |member| member.functions.clone());
    let project_admin = active_member.is_some_and(|member| member.project_admin);
    let has_access = active_member.is_some() || app_admin;
    let enabled_modules: Vec<String> =
        serde_json::from_str(&project.modules_json).unwrap_or_default();
    let archived = project.status != "active";
    let areas = ProjectArea::ALL
        .into_iter()
        .map(|area| {
            let module = match area {
                ProjectArea::Tasks | ProjectArea::Board | ProjectArea::Sprints => "tasks",
                ProjectArea::Knowledge | ProjectArea::Repos => "knowledge",
                ProjectArea::Tests | ProjectArea::Environments => "tests",
                ProjectArea::Security | ProjectArea::SecurityConfidential => "security",
                ProjectArea::Settings => "",
                _ => area.slug(),
            };
            let enabled = has_access
                && (module.is_empty() || enabled_modules.iter().any(|value| value == module));
            let mut level = function_level(&functions, catalogue, area);
            if area != ProjectArea::SecurityConfidential {
                if project_admin {
                    level = Admin;
                } else if app_admin {
                    level = level.max(Read);
                }
            }
            if !enabled {
                level = None;
            }
            ProjectAreaAccessWire {
                area,
                level,
                enabled,
            }
        })
        .collect();
    let mut access = ProjectAccessWire {
        has_access,
        project_admin,
        app_admin,
        is_owner: member.is_some_and(|member| member.user_id == project.owner_user_id),
        archived,
        functions,
        expires_at: member.and_then(|member| member.expires_at.clone()),
        enabled_modules,
        areas,
        can_create_tasks: false,
        can_manage_members: has_access && project_admin && !archived,
        can_manage_settings: false,
    };
    access.can_create_tasks = has_access
        && !archived
        && access
            .enabled_modules
            .iter()
            .any(|module| module == "tasks");
    access.can_manage_settings = access.allows(ProjectArea::Settings, Write);
    access
}
