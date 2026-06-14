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
use crate::ml_studio::models::{ProjectMember, ProjectRole, ProjectSummary, ProjectType};
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
#[policy(UserSession)]
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
#[policy(UserSession)]
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
#[policy(UserSession)]
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
#[policy(UserSession)]
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

#[handler(variant = "MlStudioProjectMembersListRequest", since = (1, 0))]
#[policy(UserSession)]
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

#[handler(variant = "MlStudioProjectInviteRequest", since = (1, 0))]
#[policy(UserSession)]
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
#[policy(UserSession)]
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
#[policy(UserSession)]
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
