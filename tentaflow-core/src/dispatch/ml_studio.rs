// ===== File: dispatch/ml_studio.rs — binary protocol handlers for ML Studio =====
//
// Projects slice: list/create/detail plus the fixed project-type catalogue.
// Identity (owner/org) comes from the request `HandlerContext` (UserSession +
// org context); ML Studio data lives in its own `ml_studio.db`.

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, MlStudioPayload, MlStudioProjectDetail, MlStudioProjectInviteResponse,
    MlStudioProjectMember, MlStudioProjectMemberRemoveResponse, MlStudioProjectMemberRoleSetResponse,
    MlStudioProjectMembersListResponse, MlStudioProjectSummary, MlStudioProjectTypeInfo,
    MlStudioProjectTypesListResponse, MlStudioProjectsListResponse, ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::ml_studio::models::{
    Dataset, ModelSummary, ProjectMember, ProjectRole, ProjectSummary, ProjectType, ResourceGrant,
    TrainingRunSummary,
};
use crate::ml_studio::build_recog_dataset;
use crate::ml_studio::profile::{self, TableProfile};
use crate::ml_studio::repository;
use crate::ml_studio::train_autogluon;
use crate::ml_studio::train_tabular::{self, Task};
use crate::services::rbac::OrgContext;
use crate::services_repo;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("ml_studio database error: {}", e))
}

fn to_detail(summary: ProjectSummary) -> MlStudioProjectDetail {
    let model_count = summary.model_count;
    let role = summary.role;
    let is_owner = summary.is_owner;
    let p = summary.project;
    MlStudioProjectDetail {
        project_id: p.project_id,
        name: p.name,
        description: p.description,
        project_type: p.project_type,
        status: p.status,
        owner_user_id: p.owner_user_id,
        org_id: p.org_id,
        model_count,
        role,
        is_owner,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

fn to_summary(summary: ProjectSummary) -> MlStudioProjectSummary {
    let model_count = summary.model_count;
    let dataset_count = summary.dataset_count;
    let training_count = summary.training_count;
    let role = summary.role;
    let is_owner = summary.is_owner;
    let p = summary.project;
    MlStudioProjectSummary {
        project_id: p.project_id,
        name: p.name,
        description: p.description,
        project_type: p.project_type,
        status: p.status,
        dataset_count,
        model_count,
        training_count,
        role,
        is_owner,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

/// Mapuje rekord członka na typ protokołu, rozwiązując `display_name` z mapy
/// nazw z katalogu CORE. Gdy nazwa nie jest znana (CORE niedostępne lub brak
/// wiersza), używa surowego `user_id` jako fallback, żeby chip nigdy nie był pusty.
fn to_member(
    member: ProjectMember,
    names: &std::collections::HashMap<String, String>,
) -> MlStudioProjectMember {
    let display_name = names
        .get(&member.user_id)
        .cloned()
        .unwrap_or_else(|| member.user_id.clone());
    MlStudioProjectMember {
        user_id: member.user_id,
        display_name,
        role: member.role,
        status: member.status,
        invited_by: member.invited_by,
        created_at: member.created_at,
    }
}

/// Maps repository authorization/validation failures to a protocol error. Owner-only
/// rejections surface as `PolicyDenied`; everything else (bad role, missing target,
/// owner-immutability) is a `BadRequest`.
fn action_err(e: anyhow::Error) -> ProtocolError {
    let msg = e.to_string();
    if msg.contains("only the project owner") {
        ProtocolError::new(ProtocolErrorCode::PolicyDenied, msg)
    } else {
        ProtocolError::bad_request(msg)
    }
}

#[handler(variant = "MlStudioProjectsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_projects_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectsListRequest(_)) => {}
        _ => return Err(ProtocolError::bad_request("expected MlStudioProjectsListRequest")),
    }
    let org = require_org(ctx)?;
    let projects = repository::list_projects(&org.user_id)
        .map_err(db_err)?
        .into_iter()
        .map(to_summary)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectsListResponse(
        MlStudioProjectsListResponse { projects },
    )))
}

#[handler(variant = "MlStudioProjectCreateRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectCreateRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioProjectCreateRequest")),
    };
    let org = require_org(ctx)?;
    let summary = repository::create_project(
        &org.user_id,
        &org.org_id,
        &payload.name,
        &payload.description,
        &payload.project_type,
    )
    .map_err(|e| ProtocolError::bad_request(format!("create project failed: {}", e)))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectCreateResponse(
        tentaflow_protocol::MlStudioProjectCreateResponse {
            project: to_detail(summary),
        },
    )))
}

#[handler(variant = "MlStudioProjectDetailRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectDetailRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioProjectDetailRequest")),
    };
    let org = require_org(ctx)?;
    let summary = repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectDetailResponse(
        tentaflow_protocol::MlStudioProjectDetailResponse {
            project: to_detail(summary),
        },
    )))
}

#[handler(variant = "MlStudioProjectTypesListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_types_list(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectTypesListRequest(_)) => {}
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectTypesListRequest",
            ))
        }
    }
    let types = ProjectType::ALL
        .into_iter()
        .map(|t| MlStudioProjectTypeInfo {
            slug: t.slug().to_string(),
            label: t.label_pl().to_string(),
            description: t.description_pl().to_string(),
        })
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectTypesListResponse(
        MlStudioProjectTypesListResponse { types },
    )))
}

/// Asserts the caller is the owner of `project_id`, returning `PolicyDenied`
/// otherwise. The repository re-checks this on every mutation; the handler-side
/// gate gives a clean rejection before any write is attempted.
fn require_project_owner(user_id: &str, project_id: &str) -> Result<(), ProtocolError> {
    match repository::member_role(project_id, user_id).map_err(db_err)? {
        Some(role) if role == ProjectRole::Owner.slug() => Ok(()),
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the project owner may manage members",
        )),
    }
}

/// Asserts the caller may WRITE data into `project_id` — i.e. is its owner or an
/// editor. Viewers and non-members are rejected with `PolicyDenied`. Read paths
/// (list/profile) stay open to every member; only data mutation needs this gate.
fn require_project_editor(user_id: &str, project_id: &str) -> Result<(), ProtocolError> {
    match repository::member_role(project_id, user_id).map_err(db_err)? {
        Some(role) if role == ProjectRole::Owner.slug() || role == ProjectRole::Editor.slug() => {
            Ok(())
        }
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "tylko właściciel lub edytor może wgrywać dane",
        )),
    }
}

/// Asserts the caller is an active member of `project_id` (any role). Read-only
/// overview endpoints use this: membership is the access boundary, non-members
/// get `NotFound` so a project's existence is not leaked.
fn require_project_member(user_id: &str, project_id: &str) -> Result<(), ProtocolError> {
    if repository::member_role(project_id, user_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "project not found",
        ));
    }
    Ok(())
}

#[handler(variant = "MlStudioProjectMembersListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_members_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectMembersListRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectMembersListRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    let rows = repository::list_members(&payload.project_id).map_err(db_err)?;
    let user_ids: Vec<String> = rows.iter().map(|m| m.user_id.clone()).collect();
    let names = repository::resolve_display_names(&user_ids);
    let members = rows.into_iter().map(|m| to_member(m, &names)).collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectMembersListResponse(
        MlStudioProjectMembersListResponse { members },
    )))
}

/// Asserts the invitee is a Power User or Admin in the CORE user directory
/// (`tentaflow.db.user_accounts`), the single source of truth for user roles.
/// ML Studio access is restricted to Power Users, so only Power Users / Admins
/// may be invited into a project. A missing user surfaces as a `BadRequest`.
fn require_invitee_power_user(invitee_user_id: &str) -> Result<(), ProtocolError> {
    let pool = crate::db::global_pool().ok_or_else(|| {
        ProtocolError::internal("core user directory unavailable")
    })?;
    match crate::db::repository::get_user_role(&pool, invitee_user_id).map_err(db_err)? {
        Some((role, is_admin)) if is_admin || role == "power_user" => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "można zapraszać tylko użytkowników Power User",
        )),
        None => Err(ProtocolError::bad_request("nie ma takiego użytkownika")),
    }
}

#[handler(variant = "MlStudioProjectInviteRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_invite(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectInviteRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioProjectInviteRequest")),
    };
    let org = require_org(ctx)?;
    require_project_owner(&org.user_id, &payload.project_id)?;
    require_invitee_power_user(&payload.invitee_user_id)?;
    let member = repository::invite_member(
        &payload.project_id,
        &org.user_id,
        &payload.invitee_user_id,
        &payload.role,
    )
    .map_err(action_err)?;
    let names = repository::resolve_display_names(std::slice::from_ref(&member.user_id));
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectInviteResponse(
        MlStudioProjectInviteResponse {
            member: to_member(member, &names),
        },
    )))
}

#[handler(variant = "MlStudioProjectMemberRemoveRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_member_remove(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectMemberRemoveRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectMemberRemoveRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    require_project_owner(&org.user_id, &payload.project_id)?;
    repository::remove_member(&payload.project_id, &org.user_id, &payload.user_id)
        .map_err(action_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectMemberRemoveResponse(
        MlStudioProjectMemberRemoveResponse {
            project_id: payload.project_id.clone(),
            user_id: payload.user_id.clone(),
        },
    )))
}

#[handler(variant = "MlStudioProjectMemberRoleSetRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_member_role_set(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectMemberRoleSetRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectMemberRoleSetRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    require_project_owner(&org.user_id, &payload.project_id)?;
    let member = repository::set_member_role(
        &payload.project_id,
        &org.user_id,
        &payload.user_id,
        &payload.role,
    )
    .map_err(action_err)?;
    let names = repository::resolve_display_names(std::slice::from_ref(&member.user_id));
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectMemberRoleSetResponse(
        MlStudioProjectMemberRoleSetResponse {
            member: to_member(member, &names),
        },
    )))
}

fn to_dataset_summary(d: &Dataset) -> tentaflow_protocol::DatasetSummary {
    tentaflow_protocol::DatasetSummary {
        dataset_id: d.dataset_id.clone(),
        project_id: d.project_id.clone(),
        name: d.name.clone(),
        kind: d.kind.clone(),
        row_count: d.row_count,
        column_count: d.column_count,
        created_at: d.created_at.clone(),
        profile_json: d.profile_json.clone(),
    }
}

/// Maps the internal `profile::TableProfile` to its protocol mirror. The wire
/// type carries `column_type` as the slug string the UI localises.
fn to_protocol_profile(p: TableProfile) -> tentaflow_protocol::TableProfile {
    tentaflow_protocol::TableProfile {
        format: p.format,
        row_count: p.row_count,
        scanned_rows: p.scanned_rows,
        column_count: p.column_count,
        truncated: p.truncated,
        columns: p
            .columns
            .into_iter()
            .map(|c| tentaflow_protocol::ColumnProfile {
                name: c.name,
                column_type: c.column_type.slug().to_string(),
                unique_count: c.unique_count,
                missing_ratio: c.missing_ratio,
                examples: c.examples,
                classes: c
                    .classes
                    .into_iter()
                    .map(|cc| tentaflow_protocol::ClassCount {
                        value: cc.value,
                        count: cc.count,
                    })
                    .collect(),
                unique_capped: c.unique_capped,
            })
            .collect(),
    }
}

/// Profiluje dataset COCO (zip): klasy z `categories` (po id rosnąco, pomijając
/// tło id==0), łączna liczba obrazów i wykryte splity (train/valid/test).
/// Waliduje obecność przynajmniej jednego `_annotations.coco.json`.
fn profile_coco_zip(zip_bytes: &[u8]) -> anyhow::Result<serde_json::Value> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| anyhow::anyhow!("nie jest poprawnym zip: {}", e))?;

    let mut classes: Vec<String> = Vec::new();
    let mut image_count: u64 = 0;
    let mut splits: Vec<String> = Vec::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let is_annot = rel
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with("_annotations.coco.json"))
            .unwrap_or(false);
        if !is_annot {
            continue;
        }
        if let Some(split) = rel.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
            splits.push(split.to_string());
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        let value: serde_json::Value = serde_json::from_slice(&buf)
            .map_err(|e| anyhow::anyhow!("_annotations.coco.json niepoprawny JSON: {}", e))?;
        if let Some(imgs) = value.get("images").and_then(|v| v.as_array()) {
            image_count += imgs.len() as u64;
        }
        if classes.is_empty() {
            if let Some(cats) = value.get("categories").and_then(|c| c.as_array()) {
                let mut cats: Vec<(i64, String)> = cats
                    .iter()
                    .filter_map(|c| {
                        Some((c.get("id")?.as_i64()?, c.get("name")?.as_str()?.to_string()))
                    })
                    .collect();
                cats.sort_by_key(|(id, _)| *id);
                let has_zero = cats.iter().any(|(id, _)| *id == 0);
                classes = cats
                    .into_iter()
                    .filter(|(id, _)| !(has_zero && *id == 0))
                    .map(|(_, name)| name)
                    .collect();
            }
        }
    }

    if splits.is_empty() {
        anyhow::bail!("zip nie zawiera żadnego _annotations.coco.json (format COCO)");
    }
    splits.sort();
    splits.dedup();
    Ok(serde_json::json!({
        "format": "coco",
        "classes": classes,
        "image_count": image_count,
        "splits": splits,
    }))
}

/// Profiluje dataset COCO leżący jako KATALOG na dysku (splity z
/// `_annotations.coco.json`). Zwraca ten sam kształt co `profile_coco_zip`.
pub(crate) fn profile_coco_dir(path: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    if !path.is_dir() {
        anyhow::bail!("ścieżka nie jest katalogiem: {}", path.display());
    }
    let mut classes: Vec<String> = Vec::new();
    let mut image_count: u64 = 0;
    let mut splits: Vec<String> = Vec::new();

    for split_entry in std::fs::read_dir(path)? {
        let split_dir = split_entry?.path();
        if !split_dir.is_dir() {
            continue;
        }
        let annot = split_dir.join("_annotations.coco.json");
        if !annot.is_file() {
            continue;
        }
        if let Some(name) = split_dir.file_name().and_then(|n| n.to_str()) {
            splits.push(name.to_string());
        }
        let buf = std::fs::read(&annot)?;
        let value: serde_json::Value = serde_json::from_slice(&buf)
            .map_err(|e| anyhow::anyhow!("{}: niepoprawny JSON: {}", annot.display(), e))?;
        if let Some(imgs) = value.get("images").and_then(|v| v.as_array()) {
            image_count += imgs.len() as u64;
        }
        if classes.is_empty() {
            if let Some(cats) = value.get("categories").and_then(|c| c.as_array()) {
                let mut cats: Vec<(i64, String)> = cats
                    .iter()
                    .filter_map(|c| {
                        Some((c.get("id")?.as_i64()?, c.get("name")?.as_str()?.to_string()))
                    })
                    .collect();
                cats.sort_by_key(|(id, _)| *id);
                let has_zero = cats.iter().any(|(id, _)| *id == 0);
                classes = cats
                    .into_iter()
                    .filter(|(id, _)| !(has_zero && *id == 0))
                    .map(|(_, name)| name)
                    .collect();
            }
        }
    }
    if splits.is_empty() {
        anyhow::bail!("katalog nie zawiera splitu z _annotations.coco.json (format COCO)");
    }
    splits.sort();
    Ok(serde_json::json!({
        "format": "coco",
        "classes": classes,
        "image_count": image_count,
        "splits": splits,
    }))
}

#[handler(variant = "MlStudioRecogDatasetRegisterRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_dataset_register(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogDatasetRegisterRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioRecogDatasetRegisterRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    let path = std::path::Path::new(payload.path.trim());
    let prof = profile_coco_dir(path)
        .map_err(|e| ProtocolError::bad_request(format!("COCO path nieprawidłowy: {}", e)))?;
    let profile_json =
        serde_json::to_string(&prof).map_err(|e| ProtocolError::internal(e.to_string()))?;
    let classes = prof
        .get("classes")
        .and_then(|c| c.as_array())
        .map(|a| a.len() as u32)
        .unwrap_or(0);
    let images = prof.get("image_count").and_then(|c| c.as_u64()).unwrap_or(0);

    let name = if payload.name.trim().is_empty() {
        path.file_name().and_then(|n| n.to_str()).unwrap_or("COCO")
    } else {
        payload.name.as_str()
    };
    // Dataset path-based: kind="coco_path", raw_data = sama ŚCIEŻKA (nie obrazy).
    let dataset = repository::create_dataset(
        &org.user_id,
        &payload.project_id,
        name,
        "coco_path",
        images,
        classes,
        &profile_json,
        payload.path.trim().as_bytes(),
    )
    .map_err(|e| ProtocolError::bad_request(format!("create dataset failed: {}", e)))?;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogDatasetRegisterResponse(
        tentaflow_protocol::MlStudioRecogDatasetRegisterResponse {
            dataset: to_dataset_summary(&dataset),
        },
    )))
}

#[handler(variant = "MlStudioSchemaGetRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_schema_get(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::SchemaGetRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioSchemaGetRequest")),
    };
    let org = require_org(ctx)?;
    require_project_member(&org.user_id, &payload.project_id)?;
    let schema_json = repository::schema_get(&payload.project_id).map_err(db_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::SchemaGetResponse(
        tentaflow_protocol::MlStudioSchemaGetResponse { schema_json },
    )))
}

#[handler(variant = "MlStudioSchemaSaveRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_schema_save(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::SchemaSaveRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioSchemaSaveRequest")),
    };
    let org = require_org(ctx)?;
    require_project_editor(&org.user_id, &payload.project_id)?;
    repository::schema_upsert(&payload.project_id, &payload.schema_json).map_err(db_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::SchemaSaveResponse(
        tentaflow_protocol::MlStudioSchemaSaveResponse { ok: true },
    )))
}

#[handler(variant = "MlStudioLookupDictsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_lookup_dicts_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::LookupDictsListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioLookupDictsListRequest")),
    };
    let org = require_org(ctx)?;
    require_project_member(&org.user_id, &payload.project_id)?;
    let dicts = repository::lookup_dicts_list(&payload.project_id).map_err(db_err)?;
    let arr: Vec<serde_json::Value> = dicts
        .into_iter()
        .map(|(dict_id, name, rows_json)| {
            serde_json::json!({
                "dictId": dict_id,
                "name": name,
                "rowsJson": rows_json,
            })
        })
        .collect();
    let dicts_json =
        serde_json::to_string(&arr).map_err(|e| ProtocolError::internal(e.to_string()))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::LookupDictsListResponse(
        tentaflow_protocol::MlStudioLookupDictsListResponse { dicts_json },
    )))
}

#[handler(variant = "MlStudioLookupDictSaveRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_lookup_dict_save(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::LookupDictSaveRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioLookupDictSaveRequest")),
    };
    let org = require_org(ctx)?;
    require_project_editor(&org.user_id, &payload.project_id)?;
    let dict_id = repository::lookup_dict_upsert(
        &payload.project_id,
        &payload.dict_id,
        &payload.name,
        &payload.rows_json,
    )
    .map_err(|e| ProtocolError::bad_request(format!("save lookup dict failed: {}", e)))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::LookupDictSaveResponse(
        tentaflow_protocol::MlStudioLookupDictSaveResponse { dict_id },
    )))
}

#[handler(variant = "MlStudioLookupDictDeleteRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_lookup_dict_delete(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::LookupDictDeleteRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioLookupDictDeleteRequest")),
    };
    let org = require_org(ctx)?;
    let project_id = repository::lookup_dict_project(&payload.dict_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "lookup dict not found"))?;
    require_project_editor(&org.user_id, &project_id)?;
    repository::lookup_dict_delete(&payload.dict_id).map_err(db_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::LookupDictDeleteResponse(
        tentaflow_protocol::MlStudioLookupDictDeleteResponse { ok: true },
    )))
}

/// Built-in CV models that ship in-core (not registered in `service_models`).
/// Verified present: `vision/detector_rfdetr.rs`, `vision/ocr_plate.rs`,
/// `vision/classifier_stan.rs`. A schema field may bind to these without any
/// service deployment.
const IN_CORE_MODELS: &[(&str, &str, &str)] = &[
    ("rfdetr-incore", "RF-DETR (wbudowany)", "detector"),
    ("ocr-plate-incore", "OCR tablic (wbudowany)", "ocr"),
    ("classifier-stan-incore", "Klasyfikator stanu (wbudowany)", "classifier"),
];

#[handler(variant = "MlStudioServiceModelsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_service_models_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ServiceModelsListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioServiceModelsListRequest")),
    };
    // Not project-scoped: any authenticated user (PowerUser policy) may list the
    // models a schema field can bind to.
    let _ = require_org(ctx)?;
    let filter = payload.capability.trim();

    let mut arr: Vec<serde_json::Value> = Vec::new();
    for m in repository::service_models_list().map_err(db_err)? {
        if !filter.is_empty() && m.capability != filter {
            continue;
        }
        arr.push(serde_json::json!({
            "id": m.id,
            "name": m.name,
            "capability": m.capability,
            "source": m.source,
        }));
    }
    for (id, name, capability) in IN_CORE_MODELS {
        if !filter.is_empty() && *capability != filter {
            continue;
        }
        arr.push(serde_json::json!({
            "id": id,
            "name": name,
            "capability": capability,
            "source": "in-core",
        }));
    }
    let models_json =
        serde_json::to_string(&arr).map_err(|e| ProtocolError::internal(e.to_string()))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ServiceModelsListResponse(
        tentaflow_protocol::MlStudioServiceModelsListResponse { models_json },
    )))
}

#[handler(variant = "MlStudioDatasetUploadRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_dataset_upload(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetUploadRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetUploadRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    let dataset = finalize_dataset_upload(
        &org.user_id,
        &payload.project_id,
        &payload.name,
        &payload.filename,
        &payload.bytes,
    )?;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetUploadResponse(
        tentaflow_protocol::MlStudioDatasetUploadResponse {
            dataset: to_dataset_summary(&dataset),
        },
    )))
}

/// Profiluje surowe bajty pliku (ZIP COCO albo tabela CSV/XLSX) i tworzy rekord
/// datasetu. Wspólne dla uploadu jednoramkowego i fragmentowanego (chunked).
fn finalize_dataset_upload(
    user_id: &str,
    project_id: &str,
    name: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<Dataset, ProtocolError> {
    // Dataset detekcji to ZIP COCO (obrazy + _annotations.coco.json), nie tabela.
    // Rozpoznajemy go po rozszerzeniu .zip i profilujemy osobno (klasy z COCO).
    let lower = filename.to_ascii_lowercase();
    let is_coco = lower.ends_with(".zip");
    let is_jsonl = lower.ends_with(".jsonl") || lower.ends_with(".json");
    let (kind, row_count, column_count, profile_json) = if is_coco {
        let prof = profile_coco_zip(bytes)
            .map_err(|e| ProtocolError::bad_request(format!("COCO profiling failed: {}", e)))?;
        let pj = serde_json::to_string(&prof).map_err(|e| ProtocolError::internal(e.to_string()))?;
        let classes = prof
            .get("classes")
            .and_then(|c| c.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0);
        let images = prof.get("image_count").and_then(|c| c.as_u64()).unwrap_or(0);
        ("coco".to_string(), images, classes, pj)
    } else if is_jsonl {
        // Dataset SFT/DPO/KD: JSON Lines. Nie profilujemy kolumn jak tabeli —
        // liczymy rekordy (niepuste linie) i zapamiętujemy klucze pierwszego
        // rekordu, żeby UI mogło pokazać schemat (np. prompt/completion).
        let text = std::str::from_utf8(bytes)
            .map_err(|_| ProtocolError::bad_request("JSONL must be valid UTF-8"))?;
        let mut records = 0u64;
        let mut keys: Vec<String> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                ProtocolError::bad_request(format!("invalid JSONL at record {}: {}", records + 1, e))
            })?;
            let obj = value.as_object().ok_or_else(|| {
                ProtocolError::bad_request(format!(
                    "JSONL record {} is not an object",
                    records + 1
                ))
            })?;
            if records == 0 {
                keys = obj.keys().cloned().collect();
            }
            records += 1;
        }
        if records == 0 {
            return Err(ProtocolError::bad_request("JSONL file has no records"));
        }
        let pj = serde_json::to_string(&serde_json::json!({
            "format": "jsonl",
            "record_count": records,
            "fields": keys,
        }))
        .map_err(|e| ProtocolError::internal(e.to_string()))?;
        ("jsonl".to_string(), records, keys.len() as u32, pj)
    } else {
        let table = profile::profile_table(bytes, filename)
            .map_err(|e| ProtocolError::bad_request(format!("profiling failed: {}", e)))?;
        let pj =
            serde_json::to_string(&table).map_err(|e| ProtocolError::internal(e.to_string()))?;
        (table.format.clone(), table.row_count, table.column_count, pj)
    };

    let dataset_name = if name.trim().is_empty() { filename } else { name };
    repository::create_dataset(
        user_id,
        project_id,
        dataset_name,
        &kind,
        row_count,
        column_count,
        &profile_json,
        bytes,
    )
    .map_err(|e| ProtocolError::bad_request(format!("create dataset failed: {}", e)))
}

// Akumulator fragmentów uploadu: upload_id → metadane + fragmenty wg seq. Wpis
// żyje tylko między pierwszym a ostatnim fragmentem; po finalizacji jest usuwany.
// Limity chronią przed zalaniem pamięci (DoS): rozmiar pojedynczego uploadu,
// łączny rozmiar wszystkich uploadów w toku, maksymalna liczba fragmentów oraz
// TTL kasujący porzucone (niedokończone) uploady.
const MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_UPLOAD_CHUNKS: u32 = 100_000;
const UPLOAD_TTL: std::time::Duration = std::time::Duration::from_secs(300);

struct UploadAccum {
    project_id: String,
    name: String,
    filename: String,
    total_chunks: u32,
    chunks: Vec<Option<Vec<u8>>>,
    received_bytes: u64,
    last_touch: std::time::Instant,
}

static UPLOAD_ACCUM: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, UploadAccum>>,
> = std::sync::OnceLock::new();

fn upload_accum() -> &'static std::sync::Mutex<std::collections::HashMap<String, UploadAccum>> {
    UPLOAD_ACCUM.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[handler(variant = "MlStudioDatasetUploadChunkRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_dataset_upload_chunk(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetUploadChunkRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetUploadChunkRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    if payload.total_chunks == 0 || payload.seq >= payload.total_chunks {
        return Err(ProtocolError::bad_request("invalid chunk seq/total"));
    }
    if payload.total_chunks > MAX_UPLOAD_CHUNKS {
        return Err(ProtocolError::bad_request("too many chunks"));
    }
    if payload.upload_id.trim().is_empty() || payload.upload_id.len() > 128 {
        return Err(ProtocolError::bad_request("invalid upload_id"));
    }

    let (received_chunks, received_bytes, complete_bytes) = {
        let mut map = upload_accum().lock().unwrap();

        // Usuń porzucone uploady (TTL) — zwalnia pamięć po przerwanych transferach.
        let now = std::time::Instant::now();
        map.retain(|_, e| now.duration_since(e.last_touch) < UPLOAD_TTL);

        // Globalny limit pamięci na wszystkie uploady w toku (poza tym właśnie
        // przyjmowanym, którego rozmiar dodajemy poniżej).
        let other_bytes: u64 = map
            .iter()
            .filter(|(k, _)| *k != &payload.upload_id)
            .map(|(_, e)| e.received_bytes)
            .sum();
        if other_bytes + payload.bytes.len() as u64 > MAX_TOTAL_UPLOAD_BYTES {
            return Err(ProtocolError::bad_request("server upload buffer full, retry later"));
        }

        let entry = map.entry(payload.upload_id.clone()).or_insert_with(|| UploadAccum {
            project_id: payload.project_id.clone(),
            name: payload.name.clone(),
            filename: payload.filename.clone(),
            total_chunks: payload.total_chunks,
            chunks: (0..payload.total_chunks).map(|_| None).collect(),
            received_bytes: 0,
            last_touch: now,
        });

        // Wszystkie fragmenty muszą zgadzać się co do całej niezmiennej metadanej
        // (projekt, nazwa, plik, liczba części) — inaczej dwa różne uploady o tym
        // samym upload_id mogłyby się przepleść i zapisać uszkodzony dataset.
        if entry.project_id != payload.project_id
            || entry.name != payload.name
            || entry.filename != payload.filename
            || entry.total_chunks != payload.total_chunks
        {
            map.remove(&payload.upload_id);
            return Err(ProtocolError::bad_request("chunk metadata mismatch for upload_id"));
        }
        entry.last_touch = now;

        let idx = payload.seq as usize;
        match &entry.chunks[idx] {
            // Powtórzony fragment z inną treścią = niespójny upload → odrzucamy.
            Some(existing) if existing.as_slice() != payload.bytes.as_slice() => {
                map.remove(&payload.upload_id);
                return Err(ProtocolError::bad_request("conflicting bytes for chunk seq"));
            }
            Some(_) => {}
            None => {
                entry.received_bytes += payload.bytes.len() as u64;
                if entry.received_bytes > MAX_UPLOAD_BYTES {
                    map.remove(&payload.upload_id);
                    return Err(ProtocolError::bad_request("upload exceeds size limit"));
                }
                entry.chunks[idx] = Some(payload.bytes.clone());
            }
        }

        let received_chunks = entry.chunks.iter().filter(|c| c.is_some()).count() as u32;
        let received_bytes = entry.received_bytes;

        // Komplet — sklejamy fragmenty po kolei i usuwamy wpis z akumulatora.
        let complete_bytes = if received_chunks == entry.total_chunks {
            let mut joined = Vec::with_capacity(entry.received_bytes as usize);
            for c in &entry.chunks {
                joined.extend_from_slice(c.as_ref().unwrap());
            }
            let meta = map.remove(&payload.upload_id).unwrap();
            Some((meta.name, meta.filename, joined))
        } else {
            None
        };
        (received_chunks, received_bytes, complete_bytes)
    };

    let dataset = if let Some((name, filename, bytes)) = complete_bytes {
        let ds =
            finalize_dataset_upload(&org.user_id, &payload.project_id, &name, &filename, &bytes)?;
        Some(to_dataset_summary(&ds))
    } else {
        None
    };

    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetUploadChunkResponse(
        tentaflow_protocol::MlStudioDatasetUploadChunkResponse {
            upload_id: payload.upload_id.clone(),
            received_chunks,
            received_bytes,
            dataset,
        },
    )))
}

// Staging accumulator for raw media uploads of recognition projects. Distinct
// from UPLOAD_ACCUM: on completion the reassembled file is WRITTEN TO DISK in the
// project staging dir (not turned into a dataset). The same DoS limits apply.
struct StageAccum {
    project_id: String,
    filename: String,
    total_chunks: u32,
    chunks: Vec<Option<Vec<u8>>>,
    received_bytes: u64,
    last_touch: std::time::Instant,
}

static STAGE_ACCUM: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, StageAccum>>,
> = std::sync::OnceLock::new();

fn stage_accum() -> &'static std::sync::Mutex<std::collections::HashMap<String, StageAccum>> {
    STAGE_ACCUM.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[handler(variant = "MlStudioRecogStageMediaRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_stage_media(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogStageMediaRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogStageMediaRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    if payload.total_chunks == 0 || payload.seq >= payload.total_chunks {
        return Err(ProtocolError::bad_request("invalid chunk seq/total"));
    }
    if payload.total_chunks > MAX_UPLOAD_CHUNKS {
        return Err(ProtocolError::bad_request("too many chunks"));
    }
    if payload.upload_id.trim().is_empty() || payload.upload_id.len() > 128 {
        return Err(ProtocolError::bad_request("invalid upload_id"));
    }

    let (received_chunks, received_bytes, complete) = {
        let mut map = stage_accum().lock().unwrap();

        let now = std::time::Instant::now();
        map.retain(|_, e| now.duration_since(e.last_touch) < UPLOAD_TTL);

        let other_bytes: u64 = map
            .iter()
            .filter(|(k, _)| *k != &payload.upload_id)
            .map(|(_, e)| e.received_bytes)
            .sum();
        if other_bytes + payload.bytes.len() as u64 > MAX_TOTAL_UPLOAD_BYTES {
            return Err(ProtocolError::bad_request("server upload buffer full, retry later"));
        }

        let entry = map.entry(payload.upload_id.clone()).or_insert_with(|| StageAccum {
            project_id: payload.project_id.clone(),
            filename: payload.filename.clone(),
            total_chunks: payload.total_chunks,
            chunks: (0..payload.total_chunks).map(|_| None).collect(),
            received_bytes: 0,
            last_touch: now,
        });

        if entry.project_id != payload.project_id
            || entry.filename != payload.filename
            || entry.total_chunks != payload.total_chunks
        {
            map.remove(&payload.upload_id);
            return Err(ProtocolError::bad_request("chunk metadata mismatch for upload_id"));
        }
        entry.last_touch = now;

        let idx = payload.seq as usize;
        match &entry.chunks[idx] {
            Some(existing) if existing.as_slice() != payload.bytes.as_slice() => {
                map.remove(&payload.upload_id);
                return Err(ProtocolError::bad_request("conflicting bytes for chunk seq"));
            }
            Some(_) => {}
            None => {
                entry.received_bytes += payload.bytes.len() as u64;
                if entry.received_bytes > MAX_UPLOAD_BYTES {
                    map.remove(&payload.upload_id);
                    return Err(ProtocolError::bad_request("upload exceeds size limit"));
                }
                entry.chunks[idx] = Some(payload.bytes.clone());
            }
        }

        let received_chunks = entry.chunks.iter().filter(|c| c.is_some()).count() as u32;
        let received_bytes = entry.received_bytes;

        let complete = if received_chunks == entry.total_chunks {
            let mut joined = Vec::with_capacity(entry.received_bytes as usize);
            for c in &entry.chunks {
                joined.extend_from_slice(c.as_ref().unwrap());
            }
            let meta = map.remove(&payload.upload_id).unwrap();
            Some((meta.filename, joined))
        } else {
            None
        };
        (received_chunks, received_bytes, complete)
    };

    let staged = if let Some((filename, bytes)) = complete {
        build_recog_dataset::stage_file(&payload.project_id, &filename, &bytes)
            .map_err(|e| ProtocolError::internal(format!("stage file failed: {}", e)))?;
        true
    } else {
        false
    };

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogStageMediaResponse(
        tentaflow_protocol::MlStudioRecogStageMediaResponse {
            upload_id: payload.upload_id.clone(),
            received_chunks,
            received_bytes,
            staged,
        },
    )))
}

#[handler(variant = "MlStudioRecogBuildDatasetRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_build_dataset(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogBuildDatasetRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogBuildDatasetRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    let dataset_name = if payload.dataset_name.trim().is_empty() {
        "dataset".to_string()
    } else {
        payload.dataset_name.trim().to_string()
    };

    // Building decodes HEIC and runs ffmpeg per video — minutes of work for many
    // files — so it runs as an async background job. Return a build id immediately;
    // the UI polls progress via RecogBuildStatus. Server-side caps + a per-project
    // single-build guard live in `spawn_build`.
    match build_recog_dataset::spawn_build(
        payload.project_id.clone(),
        org.user_id.clone(),
        dataset_name,
        payload.fps,
        payload.source_dir.clone(),
    ) {
        Ok(build_id) => Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogBuildDatasetResponse(
            tentaflow_protocol::MlStudioRecogBuildDatasetResponse {
                build_id,
                status: "running".to_string(),
                error: None,
            },
        ))),
        Err(e) => Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogBuildDatasetResponse(
            tentaflow_protocol::MlStudioRecogBuildDatasetResponse {
                build_id: String::new(),
                status: "failed".to_string(),
                error: Some(e.to_string()),
            },
        ))),
    }
}

#[handler(variant = "MlStudioRecogBuildStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_build_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogBuildStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogBuildStatusRequest")),
    };
    let _org = require_org(ctx)?;

    let prog = build_recog_dataset::build_progress(&payload.build_id)
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "build not found"))?;

    // Resolve the registered dataset summary once the build succeeded.
    let dataset = match prog.dataset_id.as_deref() {
        Some(id) => repository::get_dataset(&_org.user_id, id)
            .map_err(db_err)?
            .as_ref()
            .map(to_dataset_summary),
        None => None,
    };

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogBuildStatusResponse(
        tentaflow_protocol::MlStudioRecogBuildStatusResponse {
            build_id: payload.build_id.clone(),
            status: prog.status,
            files_total: prog.files_total,
            files_done: prog.files_done,
            frames_extracted: prog.frames_extracted,
            dataset,
            image_count: prog.image_count,
            category_count: prog.category_count,
            error: prog.error,
        },
    )))
}

#[handler(variant = "MlStudioDatasetsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_datasets_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetsListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetsListRequest")),
    };
    let org = require_org(ctx)?;
    let datasets = repository::list_datasets(&org.user_id, &payload.project_id)
        .map_err(|e| {
            if e.to_string().contains("not a member") {
                ProtocolError::new(ProtocolErrorCode::NotFound, "project not found")
            } else {
                db_err(e)
            }
        })?
        .iter()
        .map(to_dataset_summary)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetsListResponse(
        tentaflow_protocol::MlStudioDatasetsListResponse { datasets },
    )))
}

#[handler(variant = "MlStudioDatasetProfileRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_dataset_profile(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetProfileRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetProfileRequest")),
    };
    let org = require_org(ctx)?;
    let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;

    let table: TableProfile = serde_json::from_str(&dataset.profile_json)
        .map_err(|e| ProtocolError::internal(format!("stored profile is corrupt: {}", e)))?;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetProfileResponse(
        tentaflow_protocol::MlStudioDatasetProfileResponse {
            dataset: to_dataset_summary(&dataset),
            profile: to_protocol_profile(table),
        },
    )))
}

/// Zwraca WIERSZE datasetu (surowe linie JSONL) do podglądu/edycji w GUI. Generyczne
/// — działa dla {question,answer}, {prompt,chosen,rejected} i innych kształtów.
#[handler(variant = "MlStudioDatasetRowsRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_dataset_rows(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetRowsRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetRowsRequest")),
    };
    let org = require_org(ctx)?;
    let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
    // Dataset w trakcie generacji / nowo utworzony (row_count==0) nie ma jeszcze
    // raw_data — to NIE błąd, zwracamy 0 wierszy + pochodzenie (meta). Ale realny
    // błąd DB dla NIEPUSTEGO datasetu MUSI się wypropagować — inaczej GUI dostałoby
    // fałszywie pusty wynik i późniejszy zapis nadpisałby prawdziwe dane pustką.
    let raw = match repository::get_dataset_raw(&org.user_id, &payload.dataset_id) {
        Ok(r) => r,
        Err(_) if dataset.row_count == 0 => Vec::new(),
        Err(e) => return Err(db_err(e)),
    };
    let text = String::from_utf8_lossy(&raw);
    let all: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let total = all.len() as u32;
    let rows = match payload.limit {
        Some(n) if n > 0 => all.into_iter().take(n as usize).collect(),
        _ => all,
    };
    // Pochodzenie: distill_meta z profile_json (czym/jak wygenerowano) — do GUI.
    let meta = serde_json::from_str::<serde_json::Value>(&dataset.profile_json)
        .ok()
        .and_then(|p| p.get("distill_meta").cloned())
        .filter(|m| !m.is_null())
        .map(|m| m.to_string());
    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetRowsResponse(
        tentaflow_protocol::MlStudioDatasetRowsResponse {
            dataset_id: payload.dataset_id.clone(),
            kind: dataset.kind,
            total,
            rows,
            meta,
        },
    )))
}

/// Nadpisuje zawartość datasetu ręcznie edytowanymi wierszami (JSONL). Waliduje, że
/// każdy wiersz to poprawny obiekt JSON. Wymaga roli edytora projektu.
#[handler(variant = "MlStudioDatasetRowsSaveRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_dataset_rows_save(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DatasetRowsSaveRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioDatasetRowsSaveRequest")),
    };
    let org = require_org(ctx)?;
    let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
    require_project_editor(&org.user_id, &dataset.project_id)?;

    // Każdy wiersz musi być poprawnym obiektem JSON — raw_data trzymamy jako JSONL.
    let mut clean: Vec<String> = Vec::with_capacity(payload.rows.len());
    for (i, r) in payload.rows.iter().enumerate() {
        let t = r.trim();
        if t.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(t).map_err(|e| {
            ProtocolError::bad_request(format!("wiersz {} nie jest poprawnym JSON: {}", i + 1, e))
        })?;
        if !v.is_object() {
            return Err(ProtocolError::bad_request(format!(
                "wiersz {} nie jest obiektem JSON",
                i + 1
            )));
        }
        clean.push(t.to_string());
    }
    let row_count = clean.len() as u64;
    let mut jsonl = clean.join("\n");
    if !jsonl.is_empty() {
        jsonl.push('\n');
    }
    repository::update_dataset_data(&org.user_id, &payload.dataset_id, row_count, jsonl.as_bytes())
        .map_err(db_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetRowsSaveResponse(
        tentaflow_protocol::MlStudioDatasetRowsSaveResponse {
            dataset_id: payload.dataset_id.clone(),
            row_count: row_count as u32,
        },
    )))
}

#[handler(variant = "MlStudioTabularTrainRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_tabular_train(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::TabularTrainRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioTabularTrainRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    let task = Task::from_slug(&payload.task)
        .ok_or_else(|| ProtocolError::bad_request("task must be 'classification' or 'regression'"))?;

    let raw = repository::get_dataset_raw(&org.user_id, &payload.dataset_id)
        .map_err(|e| ProtocolError::bad_request(format!("dataset unavailable: {}", e)))?;
    // parse_table selects its parser by file extension; the dataset's stored
    // `kind` ("csv"/"xlsx") is the original format, so reuse it as the suffix.
    let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
    let ext = if dataset.kind == "xlsx" { "xlsx" } else { "csv" };
    let filename = format!("{}.{}", payload.dataset_id, ext);

    // Wybór silnika per request. None/""/"rust" → wbudowany silnik Rust
    // (domyślny, kompatybilny wstecz); "autogluon" → zewnętrzny serwis HTTP.
    // Każda ścieżka produkuje wspólny `TrainOutcome`, więc zapis i odpowiedź są
    // współdzielone poniżej.
    let engine = payload.engine.as_deref().unwrap_or("").to_lowercase();
    let (outcome, engine_used) = match engine.as_str() {
        "" | "rust" => {
            let (headers, rows) = profile::parse_table(&raw, &filename)
                .map_err(|e| ProtocolError::bad_request(format!("parsing failed: {}", e)))?;
            let outcome =
                train_tabular::train_tabular(&headers, &rows, &payload.target_column, task)
                    .map_err(|e| ProtocolError::bad_request(format!("training failed: {}", e)))?;
            (outcome, "rust")
        }
        "autogluon" => {
            let endpoint = {
                let conn = ctx
                    .state
                    .db
                    .read()
                    .map_err(|_| ProtocolError::internal("db read"))?;
                let svcs = services_repo::services::list_by_category(
                    &conn,
                    "training",
                    Some("autogluon-training"),
                )
                .map_err(db_err)?;
                let svc = svcs.into_iter().next().ok_or_else(|| {
                    ProtocolError::bad_request(
                        "Silnik AutoGluon niedostępny — uruchom serwis „AutoGluon (Tabular AutoML)” w Serwisach",
                    )
                })?;
                svc.endpoint_url.ok_or_else(|| {
                    ProtocolError::bad_request("serwis AutoGluon bez endpointu")
                })?
            };
            let outcome = train_autogluon::train_via_service(
                &endpoint,
                &raw,
                &filename,
                &payload.target_column,
                task,
            )
            .map_err(|e| ProtocolError::bad_request(format!("training failed: {}", e)))?;
            (outcome, "autogluon")
        }
        _ => return Err(ProtocolError::bad_request("nieznany silnik")),
    };

    let best = outcome
        .leaderboard
        .first()
        .ok_or_else(|| ProtocolError::internal("training produced no models"))?;

    let config_json = serde_json::json!({
        "target": outcome.target_column,
        "task": outcome.task.slug(),
        "engine": engine_used,
        "feature_count": outcome.feature_names.len(),
        "train_rows": outcome.train_rows,
        "holdout_rows": outcome.holdout_rows,
        "class_labels": outcome.class_labels,
    })
    .to_string();
    let metrics_json = serde_json::json!({
        "model_name": best.model_name,
        "accuracy": best.accuracy,
        "f1_macro": best.f1_macro,
        "rmse": best.rmse,
        "r2": best.r2,
        "train_secs": best.train_secs,
    })
    .to_string();

    let history: Vec<(i64, String, f64)> = outcome
        .best_loss_curve
        .iter()
        .enumerate()
        .map(|(step, loss)| (step as i64, "train_loss".to_string(), *loss))
        .collect();

    let (run_id, best_model_id) = repository::record_training_result(
        &payload.project_id,
        &best.model_name,
        &best.framework,
        &config_json,
        &metrics_json,
        &history,
    )
    .map_err(db_err)?;

    let leaderboard = outcome
        .leaderboard
        .iter()
        .map(|e| tentaflow_protocol::MlStudioTabularLeaderboardEntry {
            model_name: e.model_name.clone(),
            framework: e.framework.clone(),
            accuracy: e.accuracy,
            f1_macro: e.f1_macro,
            rmse: e.rmse,
            r2: e.r2,
            train_secs: e.train_secs,
        })
        .collect();

    Ok(MessageBody::MlStudioBody(MlStudioPayload::TabularTrainResponse(
        tentaflow_protocol::MlStudioTabularTrainResponse {
            run_id,
            best_model_id,
            best_model_name: outcome.best_model_name,
            task: outcome.task.slug().to_string(),
            target_column: outcome.target_column,
            train_rows: outcome.train_rows as u64,
            holdout_rows: outcome.holdout_rows as u64,
            leaderboard,
        },
    )))
}

fn to_grant(g: ResourceGrant) -> tentaflow_protocol::MlStudioResourceGrant {
    tentaflow_protocol::MlStudioResourceGrant {
        grant_id: g.grant_id,
        subject_kind: g.subject_kind,
        subject_id: g.subject_id,
        node_id: g.node_id,
        resource_kind: g.resource_kind,
        resource_ref: g.resource_ref,
        quota: g.quota,
        granted_by: g.granted_by,
        created_at: g.created_at,
    }
}

#[handler(variant = "MlStudioResourceGrantCreateRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn ml_studio_resource_grant_create(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantCreateRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioResourceGrantCreateRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    let grant = repository::create_grant(
        &payload.subject_kind,
        &payload.subject_id,
        &payload.node_id,
        &payload.resource_kind,
        &payload.resource_ref,
        &payload.quota,
        &org.user_id,
    )
    .map_err(|e| ProtocolError::bad_request(format!("create grant failed: {}", e)))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantCreateResponse(
        tentaflow_protocol::MlStudioResourceGrantCreateResponse {
            grant: to_grant(grant),
        },
    )))
}

#[handler(variant = "MlStudioResourceGrantsListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn ml_studio_resource_grants_list(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantsListRequest(_)) => {}
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioResourceGrantsListRequest",
            ))
        }
    }
    let grants = repository::list_grants()
        .map_err(db_err)?
        .into_iter()
        .map(to_grant)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantsListResponse(
        tentaflow_protocol::MlStudioResourceGrantsListResponse { grants },
    )))
}

#[handler(variant = "MlStudioResourceGrantRevokeRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn ml_studio_resource_grant_revoke(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantRevokeRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioResourceGrantRevokeRequest",
            ))
        }
    };
    let revoked = repository::revoke_grant(&payload.grant_id).map_err(db_err)?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ResourceGrantRevokeResponse(
        tentaflow_protocol::MlStudioResourceGrantRevokeResponse {
            grant_id: payload.grant_id.clone(),
            revoked,
        },
    )))
}

#[handler(variant = "MlStudioProjectResourcesRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_resources(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectResourcesRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectResourcesRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    // A project member (any role) may see the resources allocated to the
    // project. Non-members are rejected — membership is the access boundary.
    if repository::member_role(&payload.project_id, &org.user_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "project not found",
        ));
    }
    let grants = repository::list_grants_for_project(&payload.project_id)
        .map_err(db_err)?
        .into_iter()
        .map(to_grant)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectResourcesResponse(
        tentaflow_protocol::MlStudioProjectResourcesResponse { grants },
    )))
}

fn to_training_run(r: TrainingRunSummary) -> tentaflow_protocol::MlStudioTrainingRunSummary {
    tentaflow_protocol::MlStudioTrainingRunSummary {
        run_id: r.run_id,
        model_id: r.model_id,
        status: r.status,
        config_json: r.config_json,
        started_at: r.started_at,
        finished_at: r.finished_at,
    }
}

fn to_model(m: ModelSummary) -> tentaflow_protocol::MlStudioModelSummary {
    tentaflow_protocol::MlStudioModelSummary {
        model_id: m.model_id,
        name: m.name,
        framework: m.framework,
        base_model: m.base_model,
        status: m.status,
        metrics_json: m.metrics_json,
        created_at: m.created_at,
    }
}

#[handler(variant = "MlStudioTrainingRunsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_training_runs_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::TrainingRunsListRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioTrainingRunsListRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    require_project_member(&org.user_id, &payload.project_id)?;
    let runs = repository::list_training_runs(&payload.project_id)
        .map_err(db_err)?
        .into_iter()
        .map(to_training_run)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::TrainingRunsListResponse(
        tentaflow_protocol::MlStudioTrainingRunsListResponse { runs },
    )))
}

#[handler(variant = "MlStudioJobsOverviewRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_jobs_overview(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::MlStudioBody(MlStudioPayload::JobsOverviewRequest(_)) => {}
        _ => return Err(ProtocolError::bad_request("expected MlStudioJobsOverviewRequest")),
    };
    let org = require_org(ctx)?;
    let rows = repository::list_active_runs_for_user(&org.user_id).map_err(db_err)?;

    let mut jobs = Vec::with_capacity(rows.len());
    for r in rows {
        // kind/variant z config_json runu (tolerancyjnie — brak = "").
        let cfg = serde_json::from_str::<serde_json::Value>(&r.config_json).ok();
        let kind = cfg
            .as_ref()
            .and_then(|c| c.get("kind")?.as_str().map(String::from))
            .unwrap_or_default();
        let variant = cfg
            .as_ref()
            .and_then(|c| c.get("variant")?.as_str().map(String::from))
            .unwrap_or_default();

        // Live-view z serwisu treningowego (tylko lokalne joby mają wpis; brak =
        // defaulty). total_epochs bierzemy z serwisu, a gdy 0 — z config_json.
        let lv = crate::ml_studio::live_view::fetch_local_live_view(&r.run_id).await;
        let total_epochs = if lv.total_epochs > 0 {
            lv.total_epochs
        } else {
            generic_total_epochs(&r.config_json)
        };

        jobs.push(tentaflow_protocol::TrainingJobInfo {
            run_id: r.run_id,
            project_id: r.project_id,
            project_name: r.project_name,
            kind,
            variant,
            status: r.status,
            epoch: lv.epoch,
            total_epochs,
            eta_s: lv.eta_s,
            elapsed_s: lv.elapsed_s,
            gpu_mem_mb: lv.gpu_mem_mb,
            stage: lv.stage,
            started_at: r.started_at.unwrap_or_default(),
        });
    }

    let gpu = tokio::task::spawn_blocking(crate::ml_studio::live_view::gpu_stats)
        .await
        .unwrap_or_else(|_| tentaflow_protocol::GpuStats {
            name: String::new(),
            mem_used_mb: 0,
            mem_total_mb: 0,
            util_pct: 0,
        });

    Ok(MessageBody::MlStudioBody(MlStudioPayload::JobsOverviewResponse(
        tentaflow_protocol::MlStudioJobsOverviewResponse { jobs, gpu },
    )))
}

/// Rekoncyliacja statusu inferencji przy ODCZYCIE. `inference_status` w metrykach
/// to snapshot z chwili deployu, a serwis żyje asynchronicznie (późny fail przy
/// wolnym loadzie, restart, usunięcie). Dla deployu LOKALNEGO nadpisujemy status
/// ŻYWYM stanem serwisu (match po sparsowanym `gguf_path`), żeby UI nie kierował
/// chatu do modelu, który nigdy nie wstał albo padł po deployu. Deploy ZDALNY
/// (inny węzeł) zostawiamy ze snapshotem — jego serwis nie żyje w naszym rejestrze.
fn reconcile_local_inference_status(
    ctx: &HandlerContext,
    models: Vec<ModelSummary>,
) -> Vec<ModelSummary> {
    use crate::services_repo::services::ServiceStatus;
    let local_node = ctx.state.local_node_id.to_string();
    let live: std::collections::HashMap<String, &'static str> = ctx
        .state
        .db
        .read()
        .ok()
        .and_then(|conn| crate::services_repo::services::list_all(&conn).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.engine_id == "llama-cpp" || r.engine_id == "mlx")
        .filter_map(|r| {
            let mf = serde_json::from_str::<serde_json::Value>(&r.config_json)
                .ok()?
                .get("model_file")?
                .as_str()?
                .to_string();
            let s = match r.status {
                ServiceStatus::Running => "deployed",
                ServiceStatus::Deploying | ServiceStatus::Starting => "deploying",
                _ => "failed",
            };
            Some((mf, s))
        })
        .collect();

    models
        .into_iter()
        .map(|mut m| {
            let Ok(mut metrics) = serde_json::from_str::<serde_json::Value>(&m.metrics_json) else {
                return m;
            };
            let Some(obj) = metrics.as_object_mut() else {
                return m;
            };
            // Tylko modele KIEDYŚ deployowane (mają linkage) i LOKALNIE.
            if !obj.contains_key("inference_model_name") {
                return m;
            }
            let node = obj.get("inference_node").and_then(|v| v.as_str()).unwrap_or("");
            if !node.is_empty() && node != local_node {
                return m;
            }
            // Match po RZECZYWISTEJ ścieżce serwowanej (`inference_model_file` z
            // deployu; fallback `gguf_path` dla starszych metryk). Bez tego deploy
            // lokalny artefaktu przeniesionego ze zdalnego węzła (gguf_path=zdalna
            // ścieżka) byłby błędnie oznaczany jako failed. Brak żywego serwisu =
            // serwis padł/usunięty → failed.
            let real = obj
                .get("inference_model_file")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("gguf_path").and_then(|v| v.as_str()))
                .and_then(|g| live.get(g).copied())
                .unwrap_or("failed");
            obj.insert("inference_status".to_string(), serde_json::json!(real));
            m.metrics_json = metrics.to_string();
            m
        })
        .collect()
}

#[handler(variant = "MlStudioModelsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_models_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ModelsListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioModelsListRequest")),
    };
    let org = require_org(ctx)?;
    require_project_member(&org.user_id, &payload.project_id)?;
    let models = repository::list_models(&payload.project_id).map_err(db_err)?;
    let models = reconcile_local_inference_status(ctx, models)
        .into_iter()
        .map(to_model)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ModelsListResponse(
        tentaflow_protocol::MlStudioModelsListResponse { models },
    )))
}

#[handler(variant = "MlStudioProjectGrantsListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_project_grants_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ProjectGrantsListRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioProjectGrantsListRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    require_project_member(&org.user_id, &payload.project_id)?;
    let grants = repository::list_grants_for_project(&payload.project_id)
        .map_err(db_err)?
        .into_iter()
        .map(to_grant)
        .collect();
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectGrantsListResponse(
        tentaflow_protocol::MlStudioProjectGrantsListResponse { grants },
    )))
}

#[handler(variant = "MlStudioFtTrainStartRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_ft_train_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtTrainStartRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtTrainStartRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    // Cele FT: SFT/DPO/KD. KD wymaga modelu-nauczyciela.
    if !matches!(payload.objective.as_str(), "sft" | "dpo" | "kd") {
        return Err(ProtocolError::bad_request("objective must be 'sft', 'dpo' or 'kd'"));
    }
    if payload.objective == "kd"
        && payload
            .teacher_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return Err(ProtocolError::bad_request(
            "objective 'kd' wymaga teacher_model",
        ));
    }

    // Węzeł docelowy (mesh): pusty/local → trening lokalny; inny → zlecenie na B.
    let local_node = ctx.state.local_node_id.to_string();
    let target_node = payload
        .target_node_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != local_node)
        .map(str::to_string);

    // Konfiguracja runu zapisana w `training_runs.config_json` — niesie pełny
    // opis treningu (do wglądu w UI i do `total_steps` w statusie).
    let hp = &payload.hyperparams;
    let config_json = serde_json::json!({
        "base_model": payload.base_model,
        "method": payload.method,
        "objective": payload.objective,
        "teacher_model": payload.teacher_model,
        "dataset_id": payload.dataset_id,
        "merge_adapter": payload.merge_adapter,
        "node_id": target_node.clone().unwrap_or_else(|| local_node.clone()),
        "num_gpus": payload.num_gpus,
        "hyperparams": {
            "learning_rate": hp.learning_rate,
            "batch_size": hp.batch_size,
            "grad_accum_steps": hp.grad_accum_steps,
            "epochs": hp.epochs,
            "lora_r": hp.lora_r,
            "lora_alpha": hp.lora_alpha,
            "lora_dropout": hp.lora_dropout,
            "max_seq_len": hp.max_seq_len,
        },
    })
    .to_string();

    let run_id = repository::create_training_run(&payload.project_id, &config_json).map_err(db_err)?;

    match target_node {
        None => {
            // Trening LOKALNY — task w tle (jak dotąd), z multi-GPU wg num_gpus.
            crate::ml_studio::train_llm::spawn_ft_training(
                run_id.clone(),
                payload.project_id.clone(),
                org.user_id.clone(),
                payload.dataset_id.clone(),
                payload.base_model.clone(),
                payload.method.clone(),
                payload.objective.clone(),
                payload.teacher_model.clone(),
                payload.hyperparams.clone(),
                payload.merge_adapter,
                payload.num_gpus,
            );
            Ok(MessageBody::MlStudioBody(MlStudioPayload::FtTrainStartResponse(
                tentaflow_protocol::MlStudioFtTrainStartResponse {
                    run_id,
                    status: "running".to_string(),
                },
            )))
        }
        Some(target) => {
            // Trening ZDALNY (mesh): dataset → blob hash → spec → transfer → MlTrainStart.
            let raw = repository::get_dataset_raw(&org.user_id, &payload.dataset_id).map_err(db_err)?;
            if raw.is_empty() {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request("dataset pusty"));
            }
            let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
            let dataset_hash = crate::ml_studio::train_recognition::blob_content_hash(&raw);
            // Multi-rig (dist.nnodes>1): A = orkiestrator + worker rank-1, B = master
            // rank-0. master_addr = LAN-IP B z rejestru mesh (mDNS) — „mamy IP, bo po
            // nich się łączymy". Pojedynczy węzeł (dist None/nnodes<=1) → B trenuje sam.
            let multi_rig = payload.dist.as_ref().map(|d| d.nnodes > 1).unwrap_or(false);
            let nnodes = payload.dist.as_ref().map(|d| d.nnodes).unwrap_or(1);
            let master_port = payload
                .dist
                .as_ref()
                .map(|d| d.master_port)
                .filter(|p| *p >= 1024)
                .unwrap_or(29500);
            let (dist_json, a_dist_opt) = if multi_rig {
                let b_ip = ctx
                    .state
                    .mesh_peer_store
                    .get(&target)
                    .and_then(|p| pick_lan_ipv4(&p.addresses))
                    .ok_or_else(|| {
                        let _ = repository::update_training_run_status(&run_id, "failed");
                        ProtocolError::bad_request(format!(
                            "multi-rig: brak adresu LAN węzła {} w rejestrze mesh",
                            target
                        ))
                    })?;
                // rdzv_id WSPÓLNY dla wszystkich węzłów (run_id) — inaczej każdy
                // węzeł utworzyłby osobną grupę rendezvous i nigdy by się nie spiął.
                let b = serde_json::json!({
                    "nnodes": nnodes, "node_rank": 0, "master_addr": b_ip,
                    "master_port": master_port, "rdzv_id": run_id,
                });
                let a = serde_json::json!({
                    "nnodes": nnodes, "node_rank": 1, "master_addr": b_ip,
                    "master_port": master_port, "rdzv_id": run_id,
                });
                (Some(b), Some(a))
            } else {
                (None, None)
            };
            let spec_json = serde_json::json!({
                "kind": "llm",
                "dataset": format!("mesh:{}", dataset_hash),
                "dataset_hash": dataset_hash,
                "dataset_kind": dataset.kind,
                "base_model": payload.base_model,
                "method": payload.method,
                "objective": payload.objective,
                "teacher_model": payload.teacher_model,
                "merge_adapter": payload.merge_adapter,
                "num_gpus": payload.num_gpus,
                "dist": dist_json,
                "output_dir": format!("ml_studio/{}/{}", payload.project_id, run_id),
                "hyperparams": {
                    "lr": hp.learning_rate,
                    "batch_size": hp.batch_size,
                    "grad_accum": hp.grad_accum_steps,
                    "epochs": hp.epochs,
                    "lora_r": hp.lora_r,
                    "lora_alpha": hp.lora_alpha,
                    "lora_dropout": hp.lora_dropout,
                    "max_seq_len": hp.max_seq_len,
                },
            })
            .to_string();

            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            // Fail-closed: brak mesh_security = nie potrafimy zweryfikować zaufania,
            // więc NIE wolno wysłać datasetu ani zlecić treningu nieznanemu peerowi.
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                let _ = repository::update_training_run_status(&run_id, "failed");
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&target) {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", target)));
            }
            let zip_bytes = crate::ml_studio::train_recognition::zip_single_file("dataset.bin", &raw)
                .map_err(|e| {
                    let _ = repository::update_training_run_status(&run_id, "failed");
                    ProtocolError::internal(format!("zip datasetu: {}", e))
                })?;
            let _ = repository::update_training_run_status(&run_id, "syncing");
            crate::ml_studio::train_recognition::spawn_mesh_push_and_train(
                iroh,
                target,
                run_id.clone(),
                zip_bytes,
                dataset_hash,
                spec_json,
            );
            // Multi-rig: A dołącza jako worker rank-1 (lokalne GPU) do rendezvous
            // hostowanego przez B (rank-0). Model zapisuje rank-0 (na B); tu tylko
            // współliczymy gradienty. Dataset A czyta lokalnie (B dostał przez mesh).
            if let Some(a_dist) = a_dist_opt {
                crate::ml_studio::train_llm::spawn_ft_local_worker(
                    run_id.clone(),
                    org.user_id.clone(),
                    payload.dataset_id.clone(),
                    payload.base_model.clone(),
                    payload.method.clone(),
                    payload.objective.clone(),
                    payload.teacher_model.clone(),
                    payload.hyperparams.clone(),
                    a_dist,
                );
            }
            Ok(MessageBody::MlStudioBody(MlStudioPayload::FtTrainStartResponse(
                tentaflow_protocol::MlStudioFtTrainStartResponse {
                    run_id,
                    status: "syncing".to_string(),
                },
            )))
        }
    }
}

#[handler(variant = "MlStudioDistillGenerateRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_distill_generate(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DistillGenerateRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioDistillGenerateRequest",
            ))
        }
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id).map_err(db_err)?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    if payload.teacher_model.trim().is_empty() {
        return Err(ProtocolError::bad_request("teacher_model jest wymagany"));
    }

    let dataset_id = crate::ml_studio::distill::spawn_distill_generation(
        ctx.state.router.clone(),
        org.user_id.clone(),
        payload.clone(),
    )
    .map_err(|e| ProtocolError::internal(e.to_string()))?;

    Ok(MessageBody::MlStudioBody(
        MlStudioPayload::DistillGenerateResponse(
            tentaflow_protocol::MlStudioDistillGenerateResponse {
                dataset_id,
                status: "generating".to_string(),
            },
        ),
    ))
}

#[handler(variant = "MlStudioDistillGenerateStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_distill_generate_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::DistillGenerateStatusRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioDistillGenerateStatusRequest",
            ))
        }
    };
    // Autoryzacja: postep widoczny TYLKO dla usera z dostepem do datasetu.
    // get_dataset jest auth-scoped (None gdy user nie jest czlonkiem projektu) —
    // bez tego dowolny user moglby pollowac status cudzego datasetu po id.
    let org = require_org(ctx)?;
    if repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .is_none()
    {
        return Err(ProtocolError::not_found("dataset not found"));
    }
    let resp = match crate::ml_studio::distill::distill_status(&payload.dataset_id) {
        Some(p) => tentaflow_protocol::MlStudioDistillGenerateStatusResponse {
            status: p.status,
            total: p.total,
            done: p.done,
            error: p.error,
            samples: p.samples,
        },
        None => tentaflow_protocol::MlStudioDistillGenerateStatusResponse {
            status: "unknown".to_string(),
            total: 0,
            done: 0,
            error: None,
            samples: Vec::new(),
        },
    };
    Ok(MessageBody::MlStudioBody(
        MlStudioPayload::DistillGenerateStatusResponse(resp),
    ))
}

#[handler(variant = "MlStudioFtTrainStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_ft_train_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtTrainStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtTrainStatusRequest")),
    };
    let org = require_org(ctx)?;
    let mut run = repository::get_training_run(&payload.run_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "run not found"))?;
    require_project_member(&org.user_id, &run.project_id)?;

    // Faza transferu datasetu przez mesh (trening zdalny) — pasek B/s.
    if let Some(sp) = crate::ml_studio::train_recognition::recog_sync_progress(&payload.run_id) {
        match sp.phase.as_str() {
            "error" => {
                let _ = repository::update_training_run_status(&payload.run_id, "failed");
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::FtTrainStatusResponse(
                    tentaflow_protocol::MlStudioFtTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "failed".to_string(),
                        step: 0,
                        total_steps: 0,
                        train_loss: None,
                        eval_loss: None,
                        error: sp.error.clone(),
                        loss_curve: Vec::new(),
                        sync_phase: Some("error".to_string()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: 0,
                    },
                )));
            }
            "training" => {
                if run.status != "running" {
                    let _ = repository::update_training_run_status(&payload.run_id, "running");
                    run.status = "running".to_string();
                }
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
            }
            _ => {
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::FtTrainStatusResponse(
                    tentaflow_protocol::MlStudioFtTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "syncing".to_string(),
                        step: 0,
                        total_steps: 0,
                        train_loss: None,
                        eval_loss: None,
                        error: None,
                        loss_curve: Vec::new(),
                        sync_phase: Some(sp.phase.clone()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: sp.rate_bps,
                    },
                )));
            }
        }
    }

    // Run ZDALNY (node_id != local i running): odpytaj B przez mesh + zapisz metryki.
    let local_node = ctx.state.local_node_id.to_string();
    let run_node = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("node_id")?.as_str().map(String::from));
    if let Some(node) = run_node.filter(|n| *n != local_node) {
        if run.status == "running" {
            if let Some(iroh) = ctx.state.quic_mesh.clone() {
                let cmd = tentaflow_protocol::mesh::MeshCommandType::MlTrainStatus {
                    run_id: payload.run_id.clone(),
                };
                let mut ok = false;
                if let Ok(resp) = iroh.send_command_and_wait(&node, cmd, 30).await {
                    if resp.ok {
                        if let tentaflow_protocol::mesh::MeshCommandResponsePayload::MlTrainStatusResult {
                            status_json,
                        } = resp.payload
                        {
                            sync_remote_ft_status(&payload.run_id, &run, &status_json);
                            if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                                run = updated;
                            }
                            ok = true;
                        }
                    }
                }
                // Węzeł nieosiągalny / zgubił job → po progu domknij run jako failed.
                if !ok
                    && crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, false)
                {
                    let _ = repository::set_training_run_error(
                        &payload.run_id,
                        "węzeł treningowy nieosiągalny — trening przerwany",
                    );
                    let _ = repository::update_training_run_status(&payload.run_id, "failed");
                    if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                        run = updated;
                    }
                } else if ok {
                    crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, true);
                }
            }
        }
    }

    let curve = repository::loss_curve_for_run(&payload.run_id).map_err(db_err)?;
    let loss_curve: Vec<tentaflow_protocol::MlStudioLossPoint> = curve
        .iter()
        .map(|(step, train, eval)| tentaflow_protocol::MlStudioLossPoint {
            step: (*step).max(0) as u64,
            train_loss: *train,
            eval_loss: *eval,
        })
        .collect();

    // step = ostatni (największy) krok z krzywej; total_steps z config gdy znamy.
    let step = curve.iter().map(|(s, _, _)| *s).max().unwrap_or(0).max(0) as u64;
    let total_steps = total_steps_from_config(&run.config_json);
    let last = loss_curve.last();
    let train_loss = last.and_then(|p| p.train_loss);
    let eval_loss = last.and_then(|p| p.eval_loss);
    // Błąd treningu (np. z węzła B) zapisany w config_json.$.error.
    let run_error = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("error")?.as_str().map(String::from))
        .filter(|_| run.status == "failed");

    Ok(MessageBody::MlStudioBody(MlStudioPayload::FtTrainStatusResponse(
        tentaflow_protocol::MlStudioFtTrainStatusResponse {
            run_id: payload.run_id.clone(),
            status: run.status,
            step,
            total_steps,
            train_loss,
            eval_loss,
            error: run_error,
            loss_curve,
            sync_phase: None,
            sync_bytes_sent: 0,
            sync_bytes_total: 0,
            sync_rate_bps: 0,
        },
    )))
}

/// Wybiera najlepszy adres LAN IPv4 peera dla rendezvous treningu (master_addr).
/// Preferuje sieci prywatne LAN (192.168/16, 10/8) ponad mostki kontenerowe
/// (172.16/12 — typowo docker), odrzuca loopback i link-local (169.254). NCCL/
/// rendezvous to bezpośrednie TCP poza meshem, więc adres musi być realnie LAN.
fn pick_lan_ipv4(addresses: &[std::net::IpAddr]) -> Option<String> {
    let v4: Vec<std::net::Ipv4Addr> = addresses
        .iter()
        .filter_map(|a| match a {
            std::net::IpAddr::V4(v) => Some(*v),
            _ => None,
        })
        .filter(|v| !v.is_loopback() && !v.is_link_local() && !v.is_unspecified())
        .collect();
    let score = |v: &std::net::Ipv4Addr| -> u8 {
        let o = v.octets();
        if o[0] == 192 && o[1] == 168 {
            0
        } else if o[0] == 10 {
            1
        } else if v.is_private() {
            2
        } else {
            3
        }
    };
    v4.into_iter().min_by_key(|v| score(v)).map(|v| v.to_string())
}

/// Zapisuje metryki/stan zdalnego treningu LLM (z węzła B) do bazy A. Po sukcesie
/// rejestruje model; przy błędzie zapisuje komunikat w config_json.$.error.
fn sync_remote_ft_status(run_id: &str, run: &repository::TrainingRunRow, status_json: &str) {
    let st: serde_json::Value = match serde_json::from_str(status_json) {
        Ok(v) => v,
        Err(_) => return,
    };
    let status = st.get("status").and_then(|v| v.as_str()).unwrap_or("running");
    let step = st.get("step").and_then(|v| v.as_i64()).unwrap_or(0);
    let train_loss = st.get("train_loss").and_then(|v| v.as_f64());
    let eval_loss = st.get("eval_loss").and_then(|v| v.as_f64());
    if let Some(l) = train_loss {
        let _ = repository::record_training_metric(run_id, step, "train_loss", l);
    }
    if let Some(l) = eval_loss {
        let _ = repository::record_training_metric(run_id, step, "eval_loss", l);
    }
    match status {
        "succeeded" => {
            let cfg: serde_json::Value =
                serde_json::from_str(&run.config_json).unwrap_or(serde_json::json!({}));
            let base_model = cfg.get("base_model").and_then(|v| v.as_str()).unwrap_or("");
            let method = cfg.get("method").and_then(|v| v.as_str()).unwrap_or("lora");
            let node = cfg.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let artifact = st.get("artifact_path").and_then(|v| v.as_str()).unwrap_or("");
            let metrics_json = serde_json::json!({
                "train_loss": train_loss,
                "eval_loss": eval_loss,
                "step": step,
                "artifact_path": artifact,
                "node_id": node,
            })
            .to_string();
            let model_name = format!("{}-{}", base_model, method);
            if let Ok(model_id) =
                repository::insert_model(&run.project_id, &model_name, "huggingface", base_model, &metrics_json)
            {
                let _ = repository::set_training_run_model(run_id, &model_id);
            }
            let _ = repository::update_training_run_status(run_id, "succeeded");
        }
        "failed" => {
            if let Some(err) = st.get("error").and_then(|v| v.as_str()).filter(|e| !e.is_empty()) {
                let _ = repository::set_training_run_error(run_id, err);
            }
            let _ = repository::update_training_run_status(run_id, "failed");
        }
        _ => {}
    }
}

/// Wylicza `total_steps` z `config_json` runu: epochs × ceil(rows/(batch×accum)).
/// Liczba wierszy nie jest w configu, więc bez niej zwracamy 0 (UI traktuje 0
/// jako „nieznane") — krok bieżący i krzywa wystarczają do paska postępu.
fn total_steps_from_config(_config_json: &str) -> u64 {
    0
}

#[handler(variant = "MlStudioRecogTrainStartRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_recog_train_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStartRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogTrainStartRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    if !matches!(
        payload.variant.as_str(),
        "nano" | "small" | "medium" | "base" | "large"
    ) {
        return Err(ProtocolError::bad_request(
            "variant must be nano|small|medium|base|large",
        ));
    }

    // Mesh-distributed: węzeł docelowy. Pusty/local → trening lokalny.
    let local_node = ctx.state.local_node_id.to_string();
    let target_node = payload
        .target_node_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != local_node)
        .map(str::to_string);

    let hp = &payload.hyperparams;
    let config_json = serde_json::json!({
        "kind": "recognition",
        "variant": payload.variant,
        "dataset_id": payload.dataset_id,
        "node_id": target_node.clone().unwrap_or_else(|| local_node.clone()),
        "hyperparams": {
            "epochs": hp.epochs,
            "batch_size": hp.batch_size,
            "grad_accum": hp.grad_accum,
            "learning_rate": hp.learning_rate,
            "resolution": hp.resolution,
            "early_stopping": hp.early_stopping,
        },
    })
    .to_string();

    let run_id = repository::create_training_run(&payload.project_id, &config_json).map_err(db_err)?;

    let target_was_remote = target_node.is_some();
    match target_node {
        None => {
            // Trening LOKALNY — task w tle (jak dotąd).
            crate::ml_studio::train_recognition::spawn_recog_training(
                run_id.clone(),
                payload.project_id.clone(),
                org.user_id.clone(),
                payload.dataset_id.clone(),
                payload.variant.clone(),
                payload.hyperparams.clone(),
            );
        }
        Some(target) => {
            // Trening ZDALNY (Node B): budujemy spec, wysyłamy komendę mesh.
            // Dataset musi być dostępny na B pod tą samą ścieżką (coco_path / NAS).
            let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
            if dataset.kind != "coco_path" {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request(
                    "trening zdalny wymaga datasetu COCO przez ścieżkę (coco_path) widoczną na węźle B",
                ));
            }
            let raw = repository::get_dataset_raw(&org.user_id, &payload.dataset_id).map_err(db_err)?;
            let dataset_dir = String::from_utf8_lossy(&raw).trim().to_string();
            // Content-hash datasetu (detekcja wspólnego zasobu na B). Liczony z
            // adnotacji COCO u A; B porówna ze swoim — zgodność = ten sam zasób.
            let dataset_hash = crate::ml_studio::train_recognition::coco_content_hash(
                std::path::Path::new(&dataset_dir),
            )
            .map_err(|e| ProtocolError::bad_request(format!("hash datasetu: {}", e)))?;
            let classes: Vec<String> = serde_json::from_str::<serde_json::Value>(&dataset.profile_json)
                .ok()
                .and_then(|p| p.get("classes")?.as_array().cloned())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            // Dataset dostarczany do B PRZEZ MESH (content-addr po hashu). B robi
            // dedup: gdy ma już ten hash (wspólny zasób), przerywa transfer.
            let spec_json = serde_json::json!({
                "kind": "recognition",
                "dataset_dir": format!("mesh:{}", dataset_hash),
                "dataset_hash": dataset_hash,
                "class_names": classes,
                "variant": payload.variant,
                "output_dir": format!("recog/{}/{}", payload.project_id, run_id),
                "hyperparams": {
                    "epochs": hp.epochs,
                    "batch_size": hp.batch_size,
                    "grad_accum": hp.grad_accum,
                    "lr": hp.learning_rate,
                    "resolution": hp.resolution,
                    "early_stopping": hp.early_stopping,
                },
            })
            .to_string();

            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            // Fail-closed: brak mesh_security = nie potrafimy zweryfikować zaufania,
            // więc NIE wolno wysłać datasetu ani zlecić treningu nieznanemu peerowi.
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                let _ = repository::update_training_run_status(&run_id, "failed");
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&target) {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", target)));
            }

            // Transfer datasetu + start treningu biegną ASYNCHRONICZNIE w tle —
            // zip dużego datasetu i przesył chunków przez mesh trwa, więc NIE
            // blokujemy RPC. Postęp (bytes/total/rate) odpytuje UI przez
            // RecogTrainStatus (faza "syncing" → pasek B/s). Błąd transferu =
            // STALL (rate→0 przez 30s), nie sztywny deadline. Run startuje w
            // statusie "syncing"; status handler przełączy go na "running" gdy
            // transfer się zmaterializuje i trening ruszy na B.
            let _ = repository::update_training_run_status(&run_id, "syncing");
            crate::ml_studio::train_recognition::spawn_mesh_dataset_push_and_train(
                iroh,
                target,
                run_id.clone(),
                dataset_dir,
                dataset_hash,
                spec_json,
            );
        }
    }

    let start_status = if target_was_remote { "syncing" } else { "running" };
    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStartResponse(
        tentaflow_protocol::MlStudioRecogTrainStartResponse {
            run_id,
            status: start_status.to_string(),
        },
    )))
}

#[handler(variant = "MlStudioRecogTrainStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_recog_train_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogTrainStatusRequest")),
    };
    let org = require_org(ctx)?;
    let mut run = repository::get_training_run(&payload.run_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "run not found"))?;
    require_project_member(&org.user_id, &run.project_id)?;

    // Faza transferu datasetu przez mesh (trening zdalny, przed startem na B):
    // zwróć postęp B/s zamiast odpytywać B. "error" → run failed; "training" →
    // transfer skończony, przełącz run na "running" i poniżej odpytaj B.
    if let Some(sp) = crate::ml_studio::train_recognition::recog_sync_progress(&payload.run_id) {
        match sp.phase.as_str() {
            "error" => {
                let _ = repository::update_training_run_status(&payload.run_id, "failed");
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStatusResponse(
                    tentaflow_protocol::MlStudioRecogTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "failed".to_string(),
                        epoch: 0,
                        total_epochs: recog_total_epochs(&run.config_json),
                        train_loss: None,
                        map50: None,
                        map50_95: None,
                        error: sp.error.clone(),
                        curve: Vec::new(),
                        sync_phase: Some("error".to_string()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: 0,
                        eta_s: 0.0,
                        elapsed_s: 0.0,
                        gpu_mem_mb: 0.0,
                        stage: String::new(),
                    },
                )));
            }
            "training" => {
                if run.status != "running" {
                    let _ = repository::update_training_run_status(&payload.run_id, "running");
                    run.status = "running".to_string();
                }
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
                // przejście do bloku odpytania B poniżej
            }
            _ => {
                // "zipping" | "syncing" | "starting" — zwróć pasek postępu transferu.
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStatusResponse(
                    tentaflow_protocol::MlStudioRecogTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "syncing".to_string(),
                        epoch: 0,
                        total_epochs: recog_total_epochs(&run.config_json),
                        train_loss: None,
                        map50: None,
                        map50_95: None,
                        error: None,
                        curve: Vec::new(),
                        sync_phase: Some(sp.phase.clone()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: sp.rate_bps,
                        eta_s: 0.0,
                        elapsed_s: 0.0,
                        gpu_mem_mb: 0.0,
                        stage: String::new(),
                    },
                )));
            }
        }
    }

    // Run ZDALNY (node_id != local i wciąż running): odpytaj Node B przez mesh,
    // zapisz metryki w bazie A, domknij run po stronie A po sukcesie/błędzie.
    let local_node = ctx.state.local_node_id.to_string();
    let run_node = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("node_id")?.as_str().map(String::from));
    if let Some(node) = run_node.filter(|n| *n != local_node) {
        if run.status == "running" {
            if let Some(iroh) = ctx.state.quic_mesh.clone() {
                let cmd = tentaflow_protocol::mesh::MeshCommandType::MlTrainStatus {
                    run_id: payload.run_id.clone(),
                };
                let mut ok = false;
                if let Ok(resp) = iroh.send_command_and_wait(&node, cmd, 30).await {
                    if resp.ok {
                        if let tentaflow_protocol::mesh::MeshCommandResponsePayload::MlTrainStatusResult {
                            status_json,
                        } = resp.payload
                        {
                            sync_remote_recog_status(&org.user_id, &payload.run_id, &run, &status_json);
                            if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                                run = updated;
                            }
                            ok = true;
                        }
                    }
                }
                if !ok
                    && crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, false)
                {
                    let _ = repository::set_training_run_error(
                        &payload.run_id,
                        "węzeł treningowy nieosiągalny — trening przerwany",
                    );
                    let _ = repository::update_training_run_status(&payload.run_id, "failed");
                    if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                        run = updated;
                    }
                } else if ok {
                    crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, true);
                }
            }
        }
    }

    let curve_raw = repository::recog_curve_for_run(&payload.run_id).map_err(db_err)?;
    let curve: Vec<tentaflow_protocol::MlStudioRecogMetricPoint> = curve_raw
        .iter()
        .map(|(epoch, loss, map50)| tentaflow_protocol::MlStudioRecogMetricPoint {
            epoch: (*epoch).max(0) as u64,
            train_loss: *loss,
            map50: *map50,
        })
        .collect();

    let epoch = curve_raw.iter().map(|(e, _, _)| *e).max().unwrap_or(0).max(0) as u64;
    let total_epochs = recog_total_epochs(&run.config_json);
    // Błąd treningu (np. z węzła B) zapisany w config_json.$.error — zwróć go do UI.
    let run_error = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("error")?.as_str().map(String::from))
        .filter(|_| run.status == "failed");
    let last = curve.last();
    let train_loss = last.and_then(|p| p.train_loss);
    let map50 = last.and_then(|p| p.map50);

    // Live-view z serwisu (tylko lokalny job ma wpis; zdalny/sprzątnięty = default).
    let lv = crate::ml_studio::live_view::fetch_local_live_view(&payload.run_id).await;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogTrainStatusResponse(
        tentaflow_protocol::MlStudioRecogTrainStatusResponse {
            run_id: payload.run_id.clone(),
            status: run.status,
            epoch,
            total_epochs,
            train_loss,
            map50,
            // mAP@50:95 nie jest w krzywej (tylko map50/loss); finalne metryki są
            // w `models.metrics_json` po sukcesie. Tu zwracamy None.
            map50_95: None,
            error: run_error,
            curve,
            sync_phase: None,
            sync_bytes_sent: 0,
            sync_bytes_total: 0,
            sync_rate_bps: 0,
            eta_s: lv.eta_s,
            elapsed_s: lv.elapsed_s,
            gpu_mem_mb: lv.gpu_mem_mb,
            stage: lv.stage,
        },
    )))
}

#[handler(variant = "MlStudioClassifierTrainStartRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_classifier_train_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::ClassifierTrainStartRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioClassifierTrainStartRequest")),
    };
    let org = require_org(ctx)?;
    repository::get_project(&org.user_id, &payload.project_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "project not found"))?;
    require_project_editor(&org.user_id, &payload.project_id)?;

    if !matches!(
        payload.variant.as_str(),
        "mobilenetv4" | "efficientnet_b0" | "resnet50"
    ) {
        return Err(ProtocolError::bad_request(
            "variant must be mobilenetv4|efficientnet_b0|resnet50",
        ));
    }
    if payload.attribute.trim().is_empty() {
        return Err(ProtocolError::bad_request("attribute nie może być puste"));
    }
    if payload.values.len() < 2 {
        return Err(ProtocolError::bad_request(
            "klasyfikator wymaga co najmniej 2 wartości atrybutu",
        ));
    }
    // Treść atrybutu i wartości trafia po stronie serwisu do metadanych/ścieżek —
    // whitelist znaków, bez pustych i `.`/`..`/separatorów ścieżek (path traversal).
    let is_valid_ml_name = |s: &str| -> bool {
        let t = s.trim();
        if t.is_empty() || t == "." || t == ".." {
            return false;
        }
        let n = s.chars().count();
        n >= 1
            && n <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ' '))
    };
    if !is_valid_ml_name(&payload.attribute) {
        return Err(ProtocolError::bad_request(
            "attribute zawiera niedozwolone znaki (dozwolone: [A-Za-z0-9_.- ], 1-64 znaki)",
        ));
    }
    for value in &payload.values {
        if !is_valid_ml_name(value) {
            return Err(ProtocolError::bad_request(
                "wartość atrybutu zawiera niedozwolone znaki (dozwolone: [A-Za-z0-9_.- ], 1-64 znaki)",
            ));
        }
    }

    // Mesh-distributed: węzeł docelowy. Pusty/local → trening lokalny.
    let local_node = ctx.state.local_node_id.to_string();
    let target_node = {
        let t = payload.target_node_id.trim();
        if t.is_empty() || t == local_node {
            None
        } else {
            Some(t.to_string())
        }
    };

    let hp = &payload.hyperparams;
    let config_json = serde_json::json!({
        "kind": "classifier",
        "attribute": payload.attribute,
        "source_class": payload.source_class,
        "variant": payload.variant,
        "values": payload.values,
        "dataset_id": payload.dataset_id,
        "node_id": target_node.clone().unwrap_or_else(|| local_node.clone()),
        "hyperparams": {
            "epochs": hp.epochs,
            "batch_size": hp.batch_size,
            "learning_rate": hp.learning_rate,
            "image_size": hp.image_size,
            "freeze_backbone": hp.freeze_backbone,
        },
    })
    .to_string();

    let run_id = repository::create_training_run(&payload.project_id, &config_json).map_err(db_err)?;

    let target_was_remote = target_node.is_some();
    match target_node {
        None => {
            // Trening LOKALNY — task w tle.
            crate::ml_studio::train_classifier::spawn_classifier_training(
                run_id.clone(),
                payload.project_id.clone(),
                org.user_id.clone(),
                payload.dataset_id.clone(),
                payload.attribute.clone(),
                payload.source_class.clone(),
                payload.variant.clone(),
                payload.values.clone(),
                payload.hyperparams.clone(),
            );
        }
        Some(target) => {
            // Trening ZDALNY (Node B): dataset COCO → mesh (content-addr po hashu),
            // po zmaterializowaniu start treningu klasyfikatora na B (kind="classifier").
            let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
            if dataset.kind != "coco_path" {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request(
                    "trening zdalny wymaga datasetu COCO przez ścieżkę (coco_path) widoczną na węźle B",
                ));
            }
            let raw = repository::get_dataset_raw(&org.user_id, &payload.dataset_id).map_err(db_err)?;
            let dataset_dir = String::from_utf8_lossy(&raw).trim().to_string();
            let dataset_hash = crate::ml_studio::train_recognition::coco_content_hash(
                std::path::Path::new(&dataset_dir),
            )
            .map_err(|e| ProtocolError::bad_request(format!("hash datasetu: {}", e)))?;
            let spec_json = serde_json::json!({
                "kind": "classifier",
                "dataset_dir": format!("mesh:{}", dataset_hash),
                "dataset_hash": dataset_hash,
                "attribute": payload.attribute,
                "source_class": payload.source_class,
                "values": payload.values,
                "variant": payload.variant,
                "output_dir": format!("classifier/{}/{}", payload.project_id, run_id),
                "hyperparams": {
                    "epochs": hp.epochs,
                    "batch_size": hp.batch_size,
                    "learning_rate": hp.learning_rate,
                    "image_size": hp.image_size,
                    "freeze_backbone": hp.freeze_backbone,
                },
            })
            .to_string();

            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                let _ = repository::update_training_run_status(&run_id, "failed");
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&target) {
                let _ = repository::update_training_run_status(&run_id, "failed");
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", target)));
            }

            let _ = repository::update_training_run_status(&run_id, "syncing");
            crate::ml_studio::train_recognition::spawn_mesh_dataset_push_and_train(
                iroh,
                target,
                run_id.clone(),
                dataset_dir,
                dataset_hash,
                spec_json,
            );
        }
    }

    let start_status = if target_was_remote { "syncing" } else { "running" };
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ClassifierTrainStartResponse(
        tentaflow_protocol::MlStudioClassifierTrainStartResponse {
            run_id,
            status: start_status.to_string(),
        },
    )))
}

#[handler(variant = "MlStudioGenericTrainStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_generic_train_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioGenericTrainStatusRequest")),
    };
    let org = require_org(ctx)?;
    let mut run = repository::get_training_run(&payload.run_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "run not found"))?;
    require_project_member(&org.user_id, &run.project_id)?;

    // Faza transferu datasetu przez mesh (trening zdalny, przed startem na B):
    // zwróć postęp B/s zamiast odpytywać B. Współdzielony rejestr postępu z recog.
    if let Some(sp) = crate::ml_studio::train_recognition::recog_sync_progress(&payload.run_id) {
        match sp.phase.as_str() {
            "error" => {
                let _ = repository::update_training_run_status(&payload.run_id, "failed");
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusResponse(
                    tentaflow_protocol::MlStudioGenericTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "failed".to_string(),
                        epoch: 0,
                        total_epochs: generic_total_epochs(&run.config_json),
                        curve: Vec::new(),
                        error: sp.error.clone().unwrap_or_default(),
                        sync_phase: Some("error".to_string()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: 0,
                        eta_s: 0.0,
                        elapsed_s: 0.0,
                        gpu_mem_mb: 0.0,
                        stage: String::new(),
                    },
                )));
            }
            "training" => {
                if run.status != "running" {
                    let _ = repository::update_training_run_status(&payload.run_id, "running");
                    run.status = "running".to_string();
                }
                crate::ml_studio::train_recognition::clear_recog_sync(&payload.run_id);
            }
            _ => {
                return Ok(MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusResponse(
                    tentaflow_protocol::MlStudioGenericTrainStatusResponse {
                        run_id: payload.run_id.clone(),
                        status: "syncing".to_string(),
                        epoch: 0,
                        total_epochs: generic_total_epochs(&run.config_json),
                        curve: Vec::new(),
                        error: String::new(),
                        sync_phase: Some(sp.phase.clone()),
                        sync_bytes_sent: sp.bytes_sent,
                        sync_bytes_total: sp.bytes_total,
                        sync_rate_bps: sp.rate_bps,
                        eta_s: 0.0,
                        elapsed_s: 0.0,
                        gpu_mem_mb: 0.0,
                        stage: String::new(),
                    },
                )));
            }
        }
    }

    // Run ZDALNY (node_id != local i wciąż running): odpytaj Node B przez mesh,
    // zapisz metryki w bazie A, domknij run po stronie A po sukcesie/błędzie.
    let local_node = ctx.state.local_node_id.to_string();
    let run_node = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("node_id")?.as_str().map(String::from));
    if let Some(node) = run_node.filter(|n| *n != local_node) {
        if run.status == "running" {
            if let Some(iroh) = ctx.state.quic_mesh.clone() {
                let cmd = tentaflow_protocol::mesh::MeshCommandType::MlTrainStatus {
                    run_id: payload.run_id.clone(),
                };
                let mut ok = false;
                if let Ok(resp) = iroh.send_command_and_wait(&node, cmd, 30).await {
                    if resp.ok {
                        if let tentaflow_protocol::mesh::MeshCommandResponsePayload::MlTrainStatusResult {
                            status_json,
                        } = resp.payload
                        {
                            sync_remote_classifier_status(&payload.run_id, &run, &status_json);
                            if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                                run = updated;
                            }
                            ok = true;
                        }
                    }
                }
                if !ok
                    && crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, false)
                {
                    let _ = repository::set_training_run_error(
                        &payload.run_id,
                        "węzeł treningowy nieosiągalny — trening przerwany",
                    );
                    let _ = repository::update_training_run_status(&payload.run_id, "failed");
                    if let Ok(Some(updated)) = repository::get_training_run(&payload.run_id) {
                        run = updated;
                    }
                } else if ok {
                    crate::ml_studio::train_recognition::note_remote_poll(&payload.run_id, true);
                }
            }
        }
    }

    let curve_raw = repository::generic_curve_for_run(&payload.run_id).map_err(db_err)?;
    let curve: Vec<tentaflow_protocol::GenericMetricPoint> = curve_raw
        .iter()
        .map(|(epoch, name, value)| tentaflow_protocol::GenericMetricPoint {
            epoch: (*epoch).max(0) as i32,
            metric_name: name.clone(),
            value: *value as f32,
        })
        .collect();

    let epoch = curve_raw.iter().map(|(e, _, _)| *e).max().unwrap_or(0).max(0) as i32;
    let total_epochs = generic_total_epochs(&run.config_json);
    // Błąd treningu (np. z węzła B) zapisany w config_json.$.error — zwróć go do UI.
    let run_error = serde_json::from_str::<serde_json::Value>(&run.config_json)
        .ok()
        .and_then(|c| c.get("error")?.as_str().map(String::from))
        .filter(|_| run.status == "failed")
        .unwrap_or_default();

    // Live-view z serwisu (tylko lokalny job ma wpis; zdalny/sprzątnięty = default).
    let lv = crate::ml_studio::live_view::fetch_local_live_view(&payload.run_id).await;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::GenericTrainStatusResponse(
        tentaflow_protocol::MlStudioGenericTrainStatusResponse {
            run_id: payload.run_id.clone(),
            status: run.status,
            epoch,
            total_epochs,
            curve,
            error: run_error,
            sync_phase: None,
            sync_bytes_sent: 0,
            sync_bytes_total: 0,
            sync_rate_bps: 0,
            eta_s: lv.eta_s,
            elapsed_s: lv.elapsed_s,
            gpu_mem_mb: lv.gpu_mem_mb,
            stage: lv.stage,
        },
    )))
}

/// Synchronizuje status zdalnego runu klasyfikatora (z Node B) do bazy A: zapisuje
/// metryki per epoka i po sukcesie rejestruje model (framework="classifier-timm").
fn sync_remote_classifier_status(
    run_id: &str,
    run: &repository::TrainingRunRow,
    status_json: &str,
) {
    let st: serde_json::Value = match serde_json::from_str(status_json) {
        Ok(v) => v,
        Err(_) => return,
    };
    let status = st.get("status").and_then(|v| v.as_str()).unwrap_or("running");
    let epoch = st.get("epoch").and_then(|v| v.as_i64()).unwrap_or(0);
    let train_loss = st.get("train_loss").and_then(|v| v.as_f64());
    let val_acc = st.get("val_acc").and_then(|v| v.as_f64());
    let val_macro_f1 = st.get("val_macro_f1").and_then(|v| v.as_f64());
    if let Some(v) = train_loss {
        let _ = repository::record_training_metric(run_id, epoch, "train_loss", v);
    }
    if let Some(v) = val_acc {
        let _ = repository::record_training_metric(run_id, epoch, "val_acc", v);
    }
    if let Some(v) = val_macro_f1 {
        let _ = repository::record_training_metric(run_id, epoch, "val_macro_f1", v);
    }

    match status {
        "succeeded" => {
            let cfg: serde_json::Value =
                serde_json::from_str(&run.config_json).unwrap_or(serde_json::json!({}));
            let attribute = cfg.get("attribute").and_then(|v| v.as_str()).unwrap_or("");
            let source_class = cfg.get("source_class").and_then(|v| v.as_str()).unwrap_or("");
            let variant = cfg.get("variant").and_then(|v| v.as_str()).unwrap_or("mobilenetv4");
            let values = cfg.get("values").cloned().unwrap_or(serde_json::json!([]));
            let metrics_json = serde_json::json!({
                "task": "classifier",
                "attribute": attribute,
                "source_class": source_class,
                "values": values,
                "val_acc": val_acc,
                "val_macro_f1": val_macro_f1,
                "onnx_path": st.get("onnx_path").and_then(|v| v.as_str()).unwrap_or(""),
                "checkpoint_path": st.get("checkpoint_path").and_then(|v| v.as_str()).unwrap_or(""),
            })
            .to_string();
            let model_name = format!("classifier-{}-{}", attribute, variant);
            if let Ok(model_id) = repository::insert_model(
                &run.project_id,
                &model_name,
                "classifier-timm",
                variant,
                &metrics_json,
            ) {
                let _ = repository::set_training_run_model(run_id, &model_id);
            }
            let _ = repository::update_training_run_status(run_id, "succeeded");
        }
        "failed" => {
            if let Some(err) = st.get("error").and_then(|v| v.as_str()).filter(|e| !e.is_empty()) {
                let _ = repository::set_training_run_error(run_id, err);
            }
            let _ = repository::update_training_run_status(run_id, "failed");
        }
        _ => {}
    }
}

/// Odczytuje `hyperparams.epochs` z `config_json` runu (total dla paska postępu
/// generycznego statusu). 0 gdy nieznane.
fn generic_total_epochs(config_json: &str) -> i32 {
    serde_json::from_str::<serde_json::Value>(config_json)
        .ok()
        .and_then(|v| v.get("hyperparams")?.get("epochs")?.as_i64())
        .unwrap_or(0) as i32
}

/// Wspólny resolver: dataset recognition (coco_path) → katalog na dysku + check
/// członkostwa w projekcie. Zwraca (dataset_dir, ()).
fn resolve_recog_dataset_dir(
    owner_user_id: &str,
    dataset_id: &str,
) -> Result<std::path::PathBuf, ProtocolError> {
    let dataset = repository::get_dataset(owner_user_id, dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
    require_project_member(owner_user_id, &dataset.project_id)?;
    if dataset.kind != "coco_path" {
        return Err(ProtocolError::bad_request(
            "edycja anotacji dostępna dla datasetu COCO przez ścieżkę (coco_path)",
        ));
    }
    let raw = repository::get_dataset_raw(owner_user_id, dataset_id).map_err(db_err)?;
    Ok(std::path::PathBuf::from(String::from_utf8_lossy(&raw).trim().to_string()))
}

#[handler(variant = "MlStudioRecogImagesListRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_images_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogImagesListRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogImagesListRequest")),
    };
    let org = require_org(ctx)?;
    let dir = resolve_recog_dataset_dir(&org.user_id, &payload.dataset_id)?;
    let (images_json, categories_json) = crate::ml_studio::coco_annotate::list_images(&dir)
        .map_err(|e| ProtocolError::bad_request(format!("lista obrazów: {}", e)))?;
    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogImagesListResponse(
        tentaflow_protocol::MlStudioRecogImagesListResponse {
            images_json,
            categories_json,
        },
    )))
}

#[handler(variant = "MlStudioRecogImageRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_image(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogImageRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogImageRequest")),
    };
    let org = require_org(ctx)?;
    let dir = resolve_recog_dataset_dir(&org.user_id, &payload.dataset_id)?;
    match crate::ml_studio::coco_annotate::get_image(&dir, &payload.image_id) {
        Ok((image_b64, mime, orig_width, orig_height, annotations_json)) => {
            Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogImageResponse(
                tentaflow_protocol::MlStudioRecogImageResponse {
                    image_b64,
                    mime,
                    orig_width,
                    orig_height,
                    annotations_json,
                    error: None,
                },
            )))
        }
        Err(e) => Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogImageResponse(
            tentaflow_protocol::MlStudioRecogImageResponse {
                image_b64: String::new(),
                mime: String::new(),
                orig_width: 0,
                orig_height: 0,
                annotations_json: "[]".to_string(),
                error: Some(e.to_string()),
            },
        ))),
    }
}

#[handler(variant = "MlStudioRecogSaveAnnotationsRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_save_annotations(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogSaveAnnotationsRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogSaveAnnotationsRequest")),
    };
    let org = require_org(ctx)?;
    let dir = resolve_recog_dataset_dir(&org.user_id, &payload.dataset_id)?;
    let (ok, error) = match crate::ml_studio::coco_annotate::save_annotations(
        &dir,
        &payload.image_id,
        &payload.annotations_json,
        payload.approve,
    ) {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogSaveAnnotationsResponse(
        tentaflow_protocol::MlStudioRecogSaveAnnotationsResponse { ok, error },
    )))
}

#[handler(variant = "MlStudioRecogAutolabelRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_autolabel_dataset(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogAutolabelRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogAutolabelRequest")),
    };
    let org = require_org(ctx)?;
    // resolve_recog_dataset_dir already checks coco_path kind + project membership.
    let dir = resolve_recog_dataset_dir(&org.user_id, &payload.dataset_id)?;
    let dataset = repository::get_dataset(&org.user_id, &payload.dataset_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "dataset not found"))?;
    require_project_editor(&org.user_id, &dataset.project_id)?;

    // Decoding + per-image inference is minutes of work for a large dataset, so it
    // runs as an async background job; the UI polls progress via AutolabelStatus.
    // The threshold/mode validation + per-dataset single-job guard live in
    // `spawn_autolabel`, which also returns a clear error when the vision feature
    // is not compiled in.
    match crate::ml_studio::autolabel_recog_dataset::spawn_autolabel(
        payload.dataset_id.clone(),
        dataset.project_id.clone(),
        org.user_id.clone(),
        dir,
        payload.threshold,
        payload.mode.clone(),
    ) {
        Ok(job_id) => Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogAutolabelResponse(
            tentaflow_protocol::MlStudioRecogAutolabelResponse {
                job_id,
                status: "running".to_string(),
                error: None,
            },
        ))),
        Err(e) => Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogAutolabelResponse(
            tentaflow_protocol::MlStudioRecogAutolabelResponse {
                job_id: String::new(),
                status: "failed".to_string(),
                error: Some(e.to_string()),
            },
        ))),
    }
}

#[handler(variant = "MlStudioRecogAutolabelStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_recog_autolabel_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogAutolabelStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogAutolabelStatusRequest")),
    };
    let org = require_org(ctx)?;

    let prog = crate::ml_studio::autolabel_recog_dataset::autolabel_progress(&payload.job_id)
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "job not found"))?;

    // A job id alone must not expose progress: verify the caller is a member of the
    // project the job belongs to (mirrors how the start handler authorizes via the
    // dataset's project). Unknown project => NotFound, so job existence is not leaked.
    if prog.project_id.is_empty() {
        return Err(ProtocolError::new(ProtocolErrorCode::NotFound, "job not found"));
    }
    require_project_member(&org.user_id, &prog.project_id)?;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogAutolabelStatusResponse(
        tentaflow_protocol::MlStudioRecogAutolabelStatusResponse {
            status: prog.status,
            images_total: prog.images_total,
            images_done: prog.images_done,
            detections: prog.detections,
            skipped_unknown: prog.skipped_unknown,
            error: prog.error,
        },
    )))
}

/// Synchronizuje status zdalnego treningu recognition (z Node B) do bazy A:
/// zapisuje metryki per epoka, a po `succeeded` rejestruje model (checkpoint na
/// B, `node_id` w metrykach) i domyka run. Idempotentne przez guard `running`.
fn sync_remote_recog_status(
    owner_user_id: &str,
    run_id: &str,
    run: &repository::TrainingRunRow,
    status_json: &str,
) {
    let st: serde_json::Value = match serde_json::from_str(status_json) {
        Ok(v) => v,
        Err(_) => return,
    };
    let status = st.get("status").and_then(|v| v.as_str()).unwrap_or("running");
    let epoch = st.get("epoch").and_then(|v| v.as_i64()).unwrap_or(0);
    let train_loss = st.get("train_loss").and_then(|v| v.as_f64());
    let map50 = st.get("map50").and_then(|v| v.as_f64());
    let map50_95 = st.get("map50_95").and_then(|v| v.as_f64());
    if let Some(loss) = train_loss {
        let _ = repository::record_training_metric(run_id, epoch, "train_loss", loss);
    }
    if let Some(m) = map50 {
        let _ = repository::record_training_metric(run_id, epoch, "map50", m);
    }

    match status {
        "succeeded" => {
            let cfg: serde_json::Value =
                serde_json::from_str(&run.config_json).unwrap_or(serde_json::json!({}));
            let variant = cfg.get("variant").and_then(|v| v.as_str()).unwrap_or("base");
            let node = cfg.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let checkpoint = st.get("artifact_path").and_then(|v| v.as_str()).unwrap_or("");
            // class_names z datasetu projektu (do późniejszej detekcji).
            let class_names: Vec<String> = cfg
                .get("dataset_id")
                .and_then(|v| v.as_str())
                .and_then(|did| repository::get_dataset(owner_user_id, did).ok().flatten())
                .and_then(|d| serde_json::from_str::<serde_json::Value>(&d.profile_json).ok())
                .and_then(|p| p.get("classes")?.as_array().cloned())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let metrics_json = serde_json::json!({
                "train_loss": train_loss,
                "map50": map50,
                "map50_95": map50_95,
                "epoch": epoch,
                "checkpoint_path": checkpoint,
                "variant": variant,
                "class_names": class_names,
                "node_id": node,
            })
            .to_string();
            let model_name = format!("rfdetr-{}", variant);
            if let Ok(model_id) = repository::insert_model(
                &run.project_id,
                &model_name,
                "rfdetr",
                &format!("RF-DETR {} @{}", variant, node),
                &metrics_json,
            ) {
                let _ = repository::set_training_run_model(run_id, &model_id);
            }
            let _ = repository::update_training_run_status(run_id, "succeeded");
        }
        "failed" => {
            if let Some(err) = st.get("error").and_then(|v| v.as_str()).filter(|e| !e.is_empty()) {
                let _ = repository::set_training_run_error(run_id, err);
            }
            let _ = repository::update_training_run_status(run_id, "failed");
        }
        _ => {}
    }
}

/// Odczytuje `hyperparams.epochs` z `config_json` runu recognition (total dla
/// paska postępu). 0 gdy nieznane.
fn recog_total_epochs(config_json: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(config_json)
        .ok()
        .and_then(|v| v.get("hyperparams")?.get("epochs")?.as_u64())
        .unwrap_or(0)
}

#[handler(variant = "MlStudioRecogDetectRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_recog_detect(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RecogDetectRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioRecogDetectRequest")),
    };
    let org = require_org(ctx)?;
    let model = repository::get_model(&payload.model_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "model not found"))?;
    require_project_member(&org.user_id, &model.project_id)?;

    let metrics: serde_json::Value = serde_json::from_str(&model.metrics_json)
        .map_err(|e| ProtocolError::internal(format!("stored metrics corrupt: {}", e)))?;
    let checkpoint = metrics
        .get("checkpoint_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProtocolError::bad_request("model bez checkpoint_path"))?
        .to_string();
    let variant = metrics
        .get("variant")
        .and_then(|v| v.as_str())
        .unwrap_or("base")
        .to_string();
    let class_names: Vec<String> = metrics
        .get("class_names")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if class_names.is_empty() {
        return Err(ProtocolError::bad_request("model bez class_names"));
    }

    // Węzeł, na którym żyje checkpoint (zapisany przy treningu). Pusty/local →
    // detekcja lokalna. Inny → komenda mesh MlDetect do tego węzła (checkpoint
    // tam, my tu nie mamy pliku). Cała komunikacja A↔B przez mesh — A nigdy nie
    // woła zdalnego serwisu bezpośrednio.
    let local_node = ctx.state.local_node_id.to_string();
    let model_node = metrics
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != local_node)
        .map(str::to_string);

    let (detections_json, width, height, error) = match model_node {
        Some(node) => {
            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            // Fail-closed: brak mesh_security = nie weryfikujemy zaufania → odmowa.
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&node) {
                return Err(ProtocolError::bad_request(format!(
                    "peer {} is not trusted",
                    node
                )));
            }
            let class_names_json = serde_json::to_string(&class_names).unwrap_or_else(|_| "[]".into());
            let cmd = tentaflow_protocol::mesh::MeshCommandType::MlDetect {
                checkpoint_path: checkpoint,
                class_names_json,
                variant,
                threshold: payload.threshold,
                image_b64: payload.image_b64.clone(),
            };
            match iroh.send_command_and_wait(&node, cmd, 120).await {
                Ok(resp) => {
                    if let tentaflow_protocol::mesh::MeshCommandResponsePayload::MlDetectResult {
                        detections_json,
                        width,
                        height,
                        error,
                    } = resp.payload
                    {
                        (detections_json, width, height, error)
                    } else if !resp.ok {
                        ("[]".to_string(), 0, 0, resp.error.or(Some("remote detect failed".into())))
                    } else {
                        ("[]".to_string(), 0, 0, Some("unexpected mesh detect response".into()))
                    }
                }
                Err(e) => ("[]".to_string(), 0, 0, Some(format!("mesh detect: {}", e))),
            }
        }
        None => {
            let outcome = crate::ml_studio::train_recognition::run_detect(
                checkpoint,
                class_names,
                variant,
                payload.threshold,
                payload.image_b64.clone(),
            )
            .await;
            match outcome {
                Ok((dj, w, h)) => (dj, w, h, None),
                Err(e) => ("[]".to_string(), 0, 0, Some(e.to_string())),
            }
        }
    };

    Ok(MessageBody::MlStudioBody(MlStudioPayload::RecogDetectResponse(
        tentaflow_protocol::MlStudioRecogDetectResponse {
            detections_json,
            width,
            height,
            error,
        },
    )))
}

#[handler(variant = "MlStudioFtExportRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_ft_export_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtExportRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtExportRequest")),
    };
    let org = require_org(ctx)?;
    let model = repository::get_model(&payload.model_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "model not found"))?;
    require_project_member(&org.user_id, &model.project_id)?;

    // Formaty wspierane przez serwis ml-training: f16/q8_0 wprost z konwertera,
    // K-quanty (Q4_K_M itd.) przez llama-quantize (konwersja f16 → docelowy typ).
    const ALLOWED_OUTTYPES: [&str; 8] = [
        "f16", "q8_0", "q2_k", "q3_k_m", "q4_k_s", "q4_k_m", "q5_k_m", "q6_k",
    ];
    if !ALLOWED_OUTTYPES.contains(&payload.outtype.as_str()) {
        return Err(ProtocolError::bad_request(
            "outtype: f16/q8_0/q2_k/q3_k_m/q4_k_s/q4_k_m/q5_k_m/q6_k",
        ));
    }

    // Adapter (artefakt FT) zapisany jest w `metrics_json.artifact_path` przez
    // trening LLM — bez niego nie ma czego eksportować.
    let metrics: serde_json::Value = serde_json::from_str(&model.metrics_json)
        .map_err(|e| ProtocolError::internal(format!("stored metrics are corrupt: {}", e)))?;
    let adapter_path = metrics
        .get("artifact_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProtocolError::bad_request("model nie ma artefaktu do eksportu"))?
        .to_string();

    // Węzeł, na którym żyje adapter (zapisany przy treningu). Pusty/local →
    // eksport lokalny; inny → eksport przez mesh na właścicielu adaptera.
    let local_node = ctx.state.local_node_id.to_string();
    let model_node = metrics
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != local_node)
        .map(str::to_string);

    // Preflight (mesh/trust) PRZED oznaczeniem `running` — inaczej nieudany
    // preflight zostawiłby model na zawsze w `running` (task piszący `failed`
    // nigdy nie wystartował).
    match model_node {
        Some(node) => {
            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            // Fail-closed: brak mesh_security = nie weryfikujemy zaufania → odmowa.
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&node) {
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", node)));
            }
            // Preflight OK → dopiero teraz oznacz `running` i odpal task.
            let running_metrics = set_export_status_running(&metrics);
            repository::update_model_metrics(&payload.model_id, &running_metrics).map_err(db_err)?;
            crate::ml_studio::export_llm::spawn_ft_export_mesh(
                iroh,
                node,
                payload.model_id.clone(),
                model.base_model.clone(),
                adapter_path,
                payload.outtype.clone(),
                running_metrics,
            );
        }
        None => {
            let running_metrics = set_export_status_running(&metrics);
            repository::update_model_metrics(&payload.model_id, &running_metrics).map_err(db_err)?;
            crate::ml_studio::export_llm::spawn_ft_export(
                payload.model_id.clone(),
                model.base_model.clone(),
                adapter_path,
                payload.outtype.clone(),
                running_metrics,
            );
        }
    }

    Ok(MessageBody::MlStudioBody(MlStudioPayload::FtExportResponse(
        tentaflow_protocol::MlStudioFtExportResponse {
            model_id: payload.model_id.clone(),
            status: "running".to_string(),
        },
    )))
}

#[handler(variant = "MlStudioFtExportStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub fn ml_studio_ft_export_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtExportStatusRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtExportStatusRequest")),
    };
    let org = require_org(ctx)?;
    let model = repository::get_model(&payload.model_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "model not found"))?;
    require_project_member(&org.user_id, &model.project_id)?;

    let metrics: serde_json::Value =
        serde_json::from_str(&model.metrics_json).unwrap_or_else(|_| serde_json::json!({}));
    let status = metrics
        .get("export_status")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let gguf_path = metrics
        .get("gguf_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let size_bytes = metrics.get("gguf_size_bytes").and_then(|v| v.as_u64());
    let error = metrics
        .get("export_error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(MessageBody::MlStudioBody(MlStudioPayload::FtExportStatusResponse(
        tentaflow_protocol::MlStudioFtExportStatusResponse {
            model_id: payload.model_id.clone(),
            status,
            gguf_path,
            size_bytes,
            error,
        },
    )))
}

/// Po zaakceptowanym deployu (`service_manifest_deploy` Ok) serwis inferencji
/// ładuje się ASYNCHRONICZNIE, więc Ok nie znaczy „serwuje". Odpytujemy realny
/// status serwisu (match po `engine_id` + `model_file` w config_json) przez krótkie
/// okno, żeby `inference_status` odzwierciedlał prawdę zamiast optymistycznego
/// „deployed". „deploying" gdy w oknie nie osiągnął stanu terminalnego — UI dopyta.
async fn resolve_inference_deploy_status(
    ctx: &HandlerContext,
    engine_id: &str,
    model_file: &str,
) -> (String, Option<String>) {
    use crate::services_repo::services::ServiceStatus;
    for i in 0..8 {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        let row = ctx.state.db.read().ok().and_then(|conn| {
            crate::services_repo::services::list_all(&conn).ok().and_then(|rows| {
                rows.into_iter().find(|r| {
                    // Match po SPARSOWANYM polu `model_file` (nie substring na surowym
                    // JSON): na Windows ścieżki mają `\`, które w JSON są escapowane
                    // (`C:\\...`), więc `contains` na surowcu chybia poprawny serwis.
                    r.engine_id == engine_id
                        && serde_json::from_str::<serde_json::Value>(&r.config_json)
                            .ok()
                            .and_then(|c| {
                                c.get("model_file")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                            })
                            .as_deref()
                            == Some(model_file)
                })
            })
        });
        match row.map(|r| (r.status, r.health_last_err)) {
            Some((ServiceStatus::Running, _)) => return ("deployed".to_string(), None),
            Some((ServiceStatus::Failed, err)) | Some((ServiceStatus::Degraded, err)) => {
                return (
                    "failed".to_string(),
                    Some(err.unwrap_or_else(|| "serwis inferencji nie wystartował".to_string())),
                );
            }
            _ => {}
        }
    }
    // Okno minęło bez stanu terminalnego (wolny load): fallback „deployed" —
    // deploy został przyjęty i najpewniej się zestawia. NIE „deploying", bo nic
    // nie synchronizuje metryk później → UI zostałby w nieskończonym pollingu.
    // Szybkie porażki (brak backendu/OOM/zły GGUF) i tak łapiemy w oknie powyżej.
    ("deployed".to_string(), None)
}

/// DEPLOY wytrenowanego modelu FT (lokalny GGUF po eksporcie) jako embedded
/// serwisu inferencji llama.cpp. Domyka cykl FT: trenuj→eksportuj→DEPLOY→używaj.
/// Reużywa istniejącego `service_manifest_deploy` (engine `llama-cpp`, `native`
/// embedded) — embedded.rs wykrywa absolutną ścieżkę `model_file` i ładuje GGUF
/// z dysku BEZ downloadu HF (model FT nie żyje w repo HF).
#[handler(variant = "MlStudioFtDeployRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_ft_deploy(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtDeployRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtDeployRequest")),
    };
    let org = require_org(ctx)?;
    let model = repository::get_model(&payload.model_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "model not found"))?;
    require_project_member(&org.user_id, &model.project_id)?;

    let metrics: serde_json::Value = serde_json::from_str(&model.metrics_json)
        .map_err(|e| ProtocolError::internal(format!("stored metrics are corrupt: {}", e)))?;

    // Deploy jest możliwy dopiero po udanym eksporcie do GGUF — `gguf_path` to
    // absolutna ścieżka pliku, a `export_status` musi być `succeeded`.
    let export_status = metrics
        .get("export_status")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    let gguf_path = metrics
        .get("gguf_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let gguf_path = match (export_status, gguf_path) {
        ("succeeded", Some(path)) => path.to_string(),
        _ => {
            return Err(ProtocolError::bad_request(
                "najpierw wyeksportuj model do GGUF",
            ))
        }
    };

    // Krótki, unikalny alias do routingu /v1 — bez slashy (segment ścieżki URL)
    // i bez kropek; pierwsze 8 znaków model_id wystarcza na unikalność w UI.
    let short_id = &payload.model_id[..payload.model_id.len().min(8)];
    let model_name = format!("ft-{}", short_id);

    // Format eksportu decyduje o silniku: katalog safetensors (eksport MLX) →
    // engine `mlx` (Apple); plik `.gguf` → `llama-cpp`. Detekcja po rozszerzeniu
    // ścieżki: brak `.gguf` = katalog modelu MLX.
    let is_mlx = !gguf_path.to_ascii_lowercase().ends_with(".gguf");
    let engine_id = if is_mlx { "mlx" } else { "llama-cpp" };

    // Węzeł, na którym ŻYJE artefakt (zapisany przy treningu/eksporcie). Węzeł
    // DOCELOWY deployu = `target_node_id` z requestu (UI) albo węzeł artefaktu.
    let local_node = ctx.state.local_node_id.to_string();
    let artifact_node = metrics
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| local_node.clone());
    let deploy_node = if payload.target_node_id.trim().is_empty() {
        artifact_node.clone()
    } else {
        payload.target_node_id.trim().to_string()
    };

    // Fail-closed: każdy zdalny węzeł (artefaktu lub docelowy) musi być zaufany.
    // Bez `mesh_security` odmawiamy (service_manifest_deploy pomija check gdy None).
    if artifact_node != local_node || deploy_node != local_node {
        let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
            ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
        })?;
        for node in [&artifact_node, &deploy_node] {
            if *node != local_node && !security.is_trusted(node) {
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", node)));
            }
        }
    }

    // DEPLOY LOKALNY (węzeł docelowy = ten węzeł): synchronicznie. Szybki, bez
    // transferu po sieci do innego węzła, więc nie grozi timeoutem przeglądarki.
    // Ewentualny PULL artefaktu z węzła zdalnego (artefakt na B, deploy tu) też tu.
    if deploy_node == local_node {
        let deploy_path = if artifact_node == local_node {
            gguf_path.clone()
        } else {
            if !is_mlx {
                return Err(ProtocolError::bad_request(
                    "transfer artefaktu między węzłami wspierany tylko dla modeli MLX (katalog)",
                ));
            }
            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport niedostępny na tym węźle")
            })?;
            let cmd = tentaflow_protocol::mesh::MeshCommandType::MlArtifactPushTo {
                src_path: gguf_path.clone(),
                target_node_id: deploy_node.clone(),
            };
            let resp = iroh
                .send_command_and_wait(&artifact_node, cmd, 600)
                .await
                .map_err(|e| ProtocolError::internal(format!("zlecenie transferu artefaktu: {}", e)))?;
            match resp.payload {
                tentaflow_protocol::mesh::MeshCommandResponsePayload::MlArtifactPushResult {
                    target_path,
                    error,
                } => {
                    if let Some(err) = error {
                        return Err(ProtocolError::internal(format!("transfer artefaktu: {}", err)));
                    }
                    target_path
                }
                _ if !resp.ok => {
                    return Err(ProtocolError::internal(
                        resp.error.unwrap_or_else(|| "transfer artefaktu nieudany".into()),
                    ))
                }
                _ => return Err(ProtocolError::internal("nieoczekiwana odpowiedź transferu artefaktu")),
            }
        };
        let config_json = serde_json::json!({
            "model_repo": model_name,
            "model_file": deploy_path,
            "ctx_size": 2048,
        })
        .to_string();
        let deploy_req = tentaflow_protocol::ServiceManifestDeployRequest {
            engine_id: engine_id.to_string(),
            deploy_method: "native".to_string(),
            node_id: deploy_node.clone(),
            config_json,
        };
        let (status, error) = match super::handlers::service_manifest_deploy(
            &MessageBody::DeploymentBody(tentaflow_protocol::DeploymentPayload::ReqStart(deploy_req)),
            ctx,
        )
        .await
        {
            // service_manifest_deploy Ok = deploy PRZYJĘTY; serwis ładuje się
            // asynchronicznie (health), więc Ok != „serwuje". Sprawdzamy realny
            // status serwisu, żeby inference_status nie kłamał „deployed" przy
            // nieudanym starcie (brak backendu, OOM, zły GGUF).
            Ok(_) => resolve_inference_deploy_status(ctx, engine_id, &deploy_path).await,
            Err(e) => ("failed".to_string(), Some(e.to_string())),
        };
        let mut merged = metrics.clone();
        if let Some(obj) = merged.as_object_mut() {
            obj.insert("inference_model_name".to_string(), serde_json::json!(model_name));
            // RZECZYWISTA ścieżka serwowana (po ew. transferze z węzła zdalnego) —
            // rekoncyliacja matchuje serwis po niej, bo `gguf_path` może wskazywać
            // oryginalną (zdalną) lokalizację artefaktu, nie plik faktycznie ładowany.
            obj.insert("inference_model_file".to_string(), serde_json::json!(deploy_path));
            obj.insert(
                "inference_status".to_string(),
                serde_json::json!(status),
            );
            if status == "deployed" {
                obj.insert("inference_node".to_string(), serde_json::json!(deploy_node));
            }
        }
        repository::update_model_metrics(&payload.model_id, &merged.to_string()).map_err(db_err)?;
        return Ok(MessageBody::MlStudioBody(MlStudioPayload::FtDeployResponse(
            tentaflow_protocol::MlStudioFtDeployResponse {
                model_id: payload.model_id.clone(),
                model_name,
                status,
                error,
            },
        )));
    }

    // DEPLOY ZDALNY (węzeł docelowy ≠ ten węzeł): transfer artefaktu może iść
    // dziesiątki sekund, a zdalny embedded przy starcie przebudowuje serwis i ACK
    // bywa opóźniony — synchroniczne czekanie zawsze przebijało timeout przeglądarki.
    // DETACHUJEMY: zapisujemy stan „deploying" od razu, zwracamy natychmiast, a
    // transfer+deploy biegnie w tle. O porażce transferu decyduje WYŁĄCZNIE watchdog
    // STALL (0 B/s przez ARTIFACT_STALL_SECS) — trwający transfer NIGDY nie pada na
    // sztywny deadline. UI odpytuje metryki: pokazuje fazę/postęp, po sukcesie flip
    // na „Zapytaj", po błędzie cofa do „Wdróż".
    if !is_mlx && artifact_node != local_node {
        return Err(ProtocolError::bad_request(
            "transfer artefaktu między węzłami wspierany tylko dla modeli MLX (katalog)",
        ));
    }
    let iroh = ctx
        .state
        .quic_mesh
        .clone()
        .ok_or_else(|| ProtocolError::internal("mesh transport niedostępny na tym węźle"))?;

    // Optymistyczny zapis stanu — model NATYCHMIAST pokazuje się jako wdrażany pod
    // tą nazwą (UI flip na „Zapytaj"); węzeł inferencji = węzeł docelowy. Ten stan
    // jest BAZĄ dla wszystkich późniejszych zapisów w tle (zachowuje alias/węzeł).
    let mut deploy_state = metrics.clone();
    if let Some(obj) = deploy_state.as_object_mut() {
        obj.insert("inference_model_name".to_string(), serde_json::json!(model_name));
        obj.insert("inference_node".to_string(), serde_json::json!(deploy_node));
        obj.insert("inference_status".to_string(), serde_json::json!("transferring"));
        obj.remove("inference_error");
        obj.insert("inference_transfer_total".to_string(), serde_json::json!(0));
        obj.insert("inference_transfer_sent".to_string(), serde_json::json!(0));
        obj.insert("inference_transfer_rate".to_string(), serde_json::json!(0));
    }
    repository::update_model_metrics(&payload.model_id, &deploy_state.to_string()).map_err(db_err)?;

    let model_id = payload.model_id.clone();
    let model_name_bg = model_name.clone();
    let engine_id_bg = engine_id.to_string();
    tokio::spawn(async move {
        run_remote_deploy(
            iroh,
            model_id,
            model_name_bg,
            engine_id_bg,
            gguf_path,
            artifact_node,
            local_node,
            deploy_node,
        )
        .await;
    });

    Ok(MessageBody::MlStudioBody(MlStudioPayload::FtDeployResponse(
        tentaflow_protocol::MlStudioFtDeployResponse {
            model_id: payload.model_id.clone(),
            model_name,
            status: "deploying".to_string(),
            error: None,
        },
    )))
}

/// Read-modify-write metryk modelu: czyta ŚWIEŻY `metrics_json` z DB, aplikuje
/// `f` na obiekt i zapisuje. Dotyka tylko pól, które ruszy `f` — równoległy zapis
/// innych pól (np. eksport) NIE jest nadpisywany stałą migawką (chroni przed
/// wyścigiem, który zgłosił codex). Brak modelu / zły JSON → no-op.
fn update_inference_metrics<F>(model_id: &str, f: F)
where
    F: FnOnce(&mut serde_json::Map<String, serde_json::Value>),
{
    let Ok(Some(model)) = repository::get_model(model_id) else {
        return;
    };
    let mut v: serde_json::Value =
        serde_json::from_str(&model.metrics_json).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        f(obj);
    }
    let _ = repository::update_model_metrics(model_id, &v.to_string());
}

/// Ustawia `inference_status` (+ opcjonalny błąd) na ŚWIEŻO odczytanych metrykach.
/// Przy błędzie cofa alias/węzeł, by UI wróciło do „Wdróż" (a nie martwe „Zapytaj").
fn set_inference_status(model_id: &str, status: &str, error: Option<&str>) {
    update_inference_metrics(model_id, |obj| {
        obj.insert("inference_status".to_string(), serde_json::json!(status));
        match error {
            Some(e) => {
                obj.insert("inference_error".to_string(), serde_json::json!(e));
                obj.remove("inference_model_name");
                obj.remove("inference_node");
            }
            None => {
                obj.remove("inference_error");
            }
        }
    });
}

/// Detachowany transfer artefaktu + deploy zdalny (węzeł docelowy ≠ ten węzeł).
/// Faza „transferring" (z paskiem B/s gdy artefakt jest lokalny) → „deploying"
/// (zdalny serwis wstaje) → „deployed"/„failed". O porażce transferu decyduje
/// watchdog STALL wewnątrz `push_artifact_stream`, nie sztywny timeout.
#[allow(clippy::too_many_arguments)]
async fn run_remote_deploy(
    iroh: std::sync::Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    model_id: String,
    model_name: String,
    engine_id: String,
    gguf_path: String,
    artifact_node: String,
    local_node: String,
    deploy_node: String,
) {
    use crate::ml_studio::mesh_artifact;

    // Backstop na czekanie A→peer: właściwy timeout to watchdog STALL przy bajtach
    // (w `push_artifact_stream` na węźle-źródle) — zwraca błąd w ~30 s od realnego
    // zastoju. Ten deadline jest tylko zabezpieczeniem na „peer w ogóle nie
    // odpowiada"; ustawiony hojnie, by AKTYWNY transfer wielkiego modelu (do 16 GiB)
    // nigdy nie padł na sztywny limit — biegnie w tle, nie blokuje przeglądarki.
    const PEER_WAIT_BACKSTOP_SECS: u64 = 3600;

    // 1) Transfer artefaktu na węzeł docelowy. Gdy artefakt jest lokalny (A→C) —
    //    pchamy wprost i raportujemy B/s do metryk (ticker). Gdy artefakt na innym
    //    węźle (B→C) — zlecamy push węzłowi-źródłu; postęp bajtowy żyje tam, więc
    //    pokazujemy tylko fazę.
    let deploy_path = if artifact_node == local_node {
        // Ticker: co ~1.2 s przepisuje postęp z mapy in-memory do metryk modelu
        // (read-modify-write świeżego JSON), żeby UI rysowało pasek B/s przez polling.
        let prog_id = model_id.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_t = stop.clone();
        let ticker = tokio::spawn(async move {
            while !stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(p) = mesh_artifact::artifact_progress(&prog_id) {
                    update_inference_metrics(&prog_id, |obj| {
                        obj.insert("inference_status".to_string(), serde_json::json!("transferring"));
                        obj.insert("inference_transfer_total".to_string(), serde_json::json!(p.bytes_total));
                        obj.insert("inference_transfer_sent".to_string(), serde_json::json!(p.bytes_sent));
                        obj.insert("inference_transfer_rate".to_string(), serde_json::json!(p.rate_bps));
                    });
                }
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
        });
        let res = mesh_artifact::push_dir_to(&iroh, &deploy_node, &gguf_path, Some(&model_id)).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        ticker.abort();
        mesh_artifact::clear_artifact_progress(&model_id);
        match res {
            Ok(p) => p,
            Err(e) => {
                set_inference_status(&model_id, "failed", Some(&format!("transfer artefaktu: {}", e)));
                return;
            }
        }
    } else {
        set_inference_status(&model_id, "transferring", None);
        let cmd = tentaflow_protocol::mesh::MeshCommandType::MlArtifactPushTo {
            src_path: gguf_path.clone(),
            target_node_id: deploy_node.clone(),
        };
        match iroh.send_command_and_wait(&artifact_node, cmd, PEER_WAIT_BACKSTOP_SECS).await {
            Ok(resp) => match resp.payload {
                tentaflow_protocol::mesh::MeshCommandResponsePayload::MlArtifactPushResult {
                    target_path,
                    error,
                } => {
                    if let Some(err) = error {
                        set_inference_status(&model_id, "failed", Some(&format!("transfer B→C: {}", err)));
                        return;
                    }
                    target_path
                }
                _ => {
                    let msg = resp.error.unwrap_or_else(|| "transfer B→C nieudany".into());
                    set_inference_status(&model_id, "failed", Some(&msg));
                    return;
                }
            },
            Err(e) => {
                set_inference_status(&model_id, "failed", Some(&format!("zlecenie transferu B→C: {}", e)));
                return;
            }
        }
    };

    // 2) Zdalny deploy embedded — komenda ServiceDeployRemote do węzła docelowego.
    set_inference_status(&model_id, "deploying", None);
    let config_json = serde_json::json!({
        "model_repo": model_name,
        "model_file": deploy_path,
        "ctx_size": 2048,
    })
    .to_string();
    let cmd = tentaflow_protocol::mesh::MeshCommandType::ServiceDeployRemote {
        engine_id,
        deploy_method: "native".to_string(),
        config_json,
    };
    match iroh.send_command_and_wait(&deploy_node, cmd, 180).await {
        Ok(resp) if resp.ok => {
            set_inference_status(&model_id, "deployed", None);
        }
        Ok(resp) => {
            // JAWNY błąd z węzła docelowego (resp.ok=false): deploy realnie padł —
            // NIE udajemy sukcesu, bo UI pokazałoby „Zapytaj" do nieistniejącego
            // serwisu. Cofamy alias i raportujemy błąd.
            let msg = resp.error.unwrap_or_else(|| "zdalny deploy zgłosił błąd".into());
            tracing::warn!(model_id = %model_id, err = %msg, "remote deploy: węzeł docelowy zgłosił błąd");
            set_inference_status(&model_id, "failed", Some(&msg));
        }
        Err(e) => {
            // ZGUBIONY ACK (błąd transportu/strumienia): embedded przy starcie
            // przebudowuje serwis i rwie strumień odpowiedzi, więc utrata ACK ≠ porażka.
            // Optymistycznie „deployed" — czat jest realną weryfikacją gotowości i
            // ujawni faktyczną porażkę, gdyby serwis jednak nie wstał.
            tracing::warn!(model_id = %model_id, error = %e, "remote deploy: ACK zgubiony — optymistycznie deployed (czat zweryfikuje)");
            set_inference_status(&model_id, "deployed", None);
        }
    }
}

/// Zapytanie do wdrożonego modelu FT (test/„użyj"). Model lokalny → inferencja
/// wprost przez Router. Model z mesh (`node_id` w metrykach) → komenda `MlChat`
/// do węzła-właściciela, który odpala inferencję na swoim silniku i zwraca tekst.
/// Cała komunikacja A↔B idzie przez mesh — A nigdy nie woła zdalnego serwisu wprost.
#[handler(variant = "MlStudioFtChatRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_ft_chat(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::FtChatRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected MlStudioFtChatRequest")),
    };
    if payload.message.trim().is_empty() {
        return Err(ProtocolError::bad_request("puste zapytanie"));
    }
    let org = require_org(ctx)?;
    let model = repository::get_model(&payload.model_id)
        .map_err(db_err)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "model not found"))?;
    require_project_member(&org.user_id, &model.project_id)?;

    let metrics: serde_json::Value = serde_json::from_str(&model.metrics_json)
        .map_err(|e| ProtocolError::internal(format!("stored metrics corrupt: {}", e)))?;

    // Model musi być wdrożony — alias inferencji ustawia `ml_studio_ft_deploy`.
    let model_name = metrics
        .get("inference_model_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProtocolError::bad_request("najpierw wdróż model (Deploy)"))?
        .to_string();

    let max_tokens = payload.max_tokens.clamp(1, 2048);

    // Routujemy do węzła, na którym model JEST WDROŻONY (`inference_node` ustawiane
    // przy deployu, może być inne niż węzeł artefaktu po transferze B→C). Fallback
    // do `node_id` dla modeli wdrożonych przed wprowadzeniem `inference_node`.
    let local_node = ctx.state.local_node_id.to_string();
    let model_node = metrics
        .get("inference_node")
        .or_else(|| metrics.get("node_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != local_node)
        .map(str::to_string);

    let (answer, error) = match model_node {
        Some(node) => {
            let iroh = ctx.state.quic_mesh.clone().ok_or_else(|| {
                ProtocolError::internal("mesh transport not available on this node")
            })?;
            // Fail-closed: brak mesh_security = brak weryfikacji zaufania → odmowa.
            let security = ctx.state.mesh_security.as_ref().ok_or_else(|| {
                ProtocolError::internal("mesh security niedostępny — nie można zweryfikować zaufania peera")
            })?;
            if !security.is_trusted(&node) {
                return Err(ProtocolError::bad_request(format!("peer {} is not trusted", node)));
            }
            let cmd = tentaflow_protocol::mesh::MeshCommandType::MlChat {
                model_name: model_name.clone(),
                message: payload.message.clone(),
                max_tokens,
            };
            match iroh.send_command_and_wait(&node, cmd, 120).await {
                Ok(resp) => {
                    if let tentaflow_protocol::mesh::MeshCommandResponsePayload::MlChatResult {
                        answer,
                        error,
                    } = resp.payload
                    {
                        (answer, error)
                    } else if !resp.ok {
                        (String::new(), resp.error.or(Some("remote chat failed".into())))
                    } else {
                        (String::new(), Some("unexpected mesh chat response".into()))
                    }
                }
                Err(e) => (String::new(), Some(format!("mesh chat: {}", e))),
            }
        }
        None => match crate::ml_studio::infer::run_local_chat(
            &ctx.state.router,
            &model_name,
            &payload.message,
            max_tokens,
        )
        .await
        {
            Ok(answer) => (answer, None),
            Err(e) => (String::new(), Some(e.to_string())),
        },
    };

    Ok(MessageBody::MlStudioBody(MlStudioPayload::FtChatResponse(
        tentaflow_protocol::MlStudioFtChatResponse { answer, error },
    )))
}

/// Wmergowuje `export_status="running"` w istniejące metryki modelu, zerując
/// pola wyniku poprzedniego eksportu (ponowny eksport zaczyna od czysta).
fn set_export_status_running(metrics: &serde_json::Value) -> String {
    let mut root = match metrics {
        serde_json::Value::Object(map) => serde_json::Value::Object(map.clone()),
        _ => serde_json::json!({}),
    };
    let obj = root.as_object_mut().expect("root is an object");
    obj.insert("export_status".to_string(), serde_json::json!("running"));
    obj.insert("gguf_path".to_string(), serde_json::Value::Null);
    obj.insert("gguf_size_bytes".to_string(), serde_json::Value::Null);
    obj.insert("export_error".to_string(), serde_json::Value::Null);
    // Nowy eksport unieważnia poprzedni deployment: stary serwis (jeśli żyje)
    // serwuje NIEAKTUALNY artefakt. Czyścimy linkage inferencji, żeby rekoncyliacja
    // nie raportowała starego deployu jako bieżącego ani nie kierowała tam chatu.
    for key in [
        "inference_status",
        "inference_model_name",
        "inference_model_file",
        "inference_node",
    ] {
        obj.remove(key);
    }
    root.to_string()
}
