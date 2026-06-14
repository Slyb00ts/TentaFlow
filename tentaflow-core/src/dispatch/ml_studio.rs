// ===== File: dispatch/ml_studio.rs — binary protocol handlers for ML Studio =====
//
// Projects slice: list/create/detail plus the fixed project-type catalogue.
// Identity (owner/org) comes from the request `HandlerContext` (UserSession +
// org context); ML Studio data lives in its own `ml_studio.db`.

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, MlStudioPayload, MlStudioProjectDetail, MlStudioProjectSummary,
    MlStudioProjectTypeInfo, MlStudioProjectTypesListResponse, MlStudioProjectsListResponse,
    ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::ml_studio::models::{ProjectSummary, ProjectType};
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
    let p = summary.project;
    MlStudioProjectDetail {
        project_id: p.project_id,
        name: p.name,
        description: p.description,
        project_type: p.project_type,
        status: p.status,
        owner_user_id: p.owner_user_id,
        org_id: p.org_id,
        model_count: summary.model_count,
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}

fn to_summary(summary: ProjectSummary) -> MlStudioProjectSummary {
    let model_count = summary.model_count;
    let dataset_count = summary.dataset_count;
    let p = summary.project;
    MlStudioProjectSummary {
        project_id: p.project_id,
        name: p.name,
        description: p.description,
        project_type: p.project_type,
        status: p.status,
        dataset_count,
        model_count,
        created_at: p.created_at,
        updated_at: p.updated_at,
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
    let projects = repository::list_projects(&org.org_id)
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
    let summary = repository::get_project(&org.org_id, &payload.project_id)
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
