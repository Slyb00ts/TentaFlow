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
    Dataset, ProjectMember, ProjectRole, ProjectSummary, ProjectType, ResourceGrant,
};
use crate::ml_studio::profile::{self, TableProfile};
use crate::ml_studio::repository;
use crate::services::rbac::OrgContext;

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
        role,
        is_owner,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

fn to_member(member: ProjectMember) -> MlStudioProjectMember {
    MlStudioProjectMember {
        user_id: member.user_id,
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
    let members = repository::list_members(&payload.project_id)
        .map_err(db_err)?
        .into_iter()
        .map(to_member)
        .collect();
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
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectInviteResponse(
        MlStudioProjectInviteResponse {
            member: to_member(member),
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
    Ok(MessageBody::MlStudioBody(MlStudioPayload::ProjectMemberRoleSetResponse(
        MlStudioProjectMemberRoleSetResponse {
            member: to_member(member),
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

    let table = profile::profile_table(&payload.bytes, &payload.filename)
        .map_err(|e| ProtocolError::bad_request(format!("profiling failed: {}", e)))?;
    let kind = table.format.clone();
    let row_count = table.row_count;
    let column_count = table.column_count;
    let profile_json =
        serde_json::to_string(&table).map_err(|e| ProtocolError::internal(e.to_string()))?;

    let name = if payload.name.trim().is_empty() {
        payload.filename.as_str()
    } else {
        payload.name.as_str()
    };
    let dataset = repository::create_dataset(
        &org.user_id,
        &payload.project_id,
        name,
        &kind,
        row_count,
        column_count,
        &profile_json,
    )
    .map_err(|e| ProtocolError::bad_request(format!("create dataset failed: {}", e)))?;

    Ok(MessageBody::MlStudioBody(MlStudioPayload::DatasetUploadResponse(
        tentaflow_protocol::MlStudioDatasetUploadResponse {
            dataset: to_dataset_summary(&dataset),
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
