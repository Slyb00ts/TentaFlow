// ============ File: processes.rs — private BPMN authoring and current-participant process commands ============

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::processes::{
    PinnedFlowInfo, ProcessNodeKind, ProcessOptionFlow, ProcessOptionUser, ProcessPayload as P,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

use super::HandlerContext;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::processes::{bpmn, repository, runtime};
use repository::{CommandStamp, PinnedServiceSnapshot, ProcessActor};

fn error(error: anyhow::Error) -> ProtocolError {
    let message = error.to_string();
    if message.contains("not found") || message.contains("Query returned no rows") {
        return ProtocolError::not_found("process resource not found");
    }
    if message.contains("not active")
        || message.contains("not the initiator")
        || message.contains("only process")
        || message.contains("not currently completable")
        || message.contains("access denied")
    {
        return ProtocolError::new(ProtocolErrorCode::PolicyDenied, message);
    }
    if error.downcast_ref::<rusqlite::Error>().is_some()
        || error.downcast_ref::<crate::db::DbError>().is_some()
    {
        tracing::error!(error = %error, "process database operation failed");
        return ProtocolError::internal("process database operation failed");
    }
    ProtocolError::bad_request(message)
}

fn actor(ctx: &HandlerContext) -> Result<ProcessActor, ProtocolError> {
    let SessionAuth::UserSession { user_id, .. } = &ctx.session else {
        return Err(ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "a user session is required",
        ));
    };
    let user_id = uuid::Uuid::from_bytes(*user_id).to_string();
    let scope = ctx
        .org_context
        .as_ref()
        .filter(|scope| scope.user_id == user_id)
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::AuthRequired,
                "a current organization is required",
            )
        })?;
    let current = crate::db::repository::get_user_account_by_id(&ctx.state.db, &user_id)
        .map_err(error)?
        .filter(|user| user.is_active)
        .ok_or_else(|| {
            ProtocolError::new(ProtocolErrorCode::PolicyDenied, "the account is not active")
        })?;
    crate::services::rbac::resolve_org_context(&ctx.state.db, &current.id, Some(&scope.org_id))
        .map_err(|_| {
            ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "the account is not a current organization member",
            )
        })?;
    let org = crate::services::org::repo::get_organization(&ctx.state.db, &scope.org_id)
        .map_err(|error| ProtocolError::internal(error.to_string()))?
        .filter(|org| org.status == "active")
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "the organization is not active",
            )
        })?;
    Ok(ProcessActor {
        org_id: org.org_id,
        user_id,
    })
}

fn dispatcher(ctx: &HandlerContext) -> Result<&std::sync::Arc<FlowDispatcher>, ProtocolError> {
    ctx.state
        .router
        .flow_dispatcher()
        .ok_or_else(|| ProtocolError::internal("the flow executor is unavailable"))
}

fn stamp(payload: &P, command_id: &str) -> Result<CommandStamp, ProtocolError> {
    uuid::Uuid::parse_str(command_id)
        .map_err(|_| ProtocolError::bad_request("command_id must be a UUID"))?;
    Ok(CommandStamp {
        command_id: command_id.to_owned(),
        request_hash: repository::request_hash(payload).map_err(error)?,
    })
}

fn options(ctx: &HandlerContext, actor: &ProcessActor) -> Result<P, ProtocolError> {
    let members =
        crate::services::org::repo::list_memberships_for_org(&ctx.state.db, &actor.org_id)
            .map_err(|error| ProtocolError::internal(error.to_string()))?;
    if members.len() > 1000 {
        return Err(ProtocolError::bad_request(
            "the process assignee catalogue exceeds 1000 entries",
        ));
    }
    let mut assignees = Vec::new();
    for (id, _) in members {
        if let Some(user) = crate::db::repository::get_user_account_by_id(&ctx.state.db, &id)
            .map_err(error)?
            .filter(|user| user.is_active)
        {
            assignees.push(ProcessOptionUser {
                user_id: user.id,
                display_name: user.display_name,
            });
        }
    }
    let executor = dispatcher(ctx)?;
    let mut service_flows = Vec::new();
    for flow in crate::db::repository::list_flows(&ctx.state.db, 0, 1001).map_err(error)? {
        if flow.status != "active" {
            continue;
        }
        let Ok(meta) = executor.authorize_process_flow(&flow.id, &actor.user_id, &actor.org_id)
        else {
            continue;
        };
        if executor.snapshot_flow(&flow.id, &meta).is_ok() {
            service_flows.push(ProcessOptionFlow {
                flow_id: flow.id,
                name: flow.name,
            });
        }
    }
    if service_flows.len() > 1000 {
        return Err(ProtocolError::bad_request(
            "the process service catalogue exceeds 1000 entries",
        ));
    }
    Ok(P::OptionsResponse {
        assignees,
        service_flows,
    })
}

#[handler(variant = "ProcessBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn process_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let MessageBody::ProcessBody(payload) = req else {
        return Err(ProtocolError::bad_request("expected a process request"));
    };
    if tentaflow_protocol::cbor::encode(req)
        .map_err(|error| ProtocolError::bad_request(error.to_string()))?
        .len()
        > 960 * 1024
    {
        return Err(ProtocolError::bad_request(
            "the process request exceeds its binary frame budget",
        ));
    }
    let actor = actor(ctx)?;
    let pool = &ctx.state.db;
    let response = match payload {
        P::OptionsRequest {} => options(ctx, &actor)?,
        P::DefinitionListRequest { offset, limit } => {
            let (definitions, total, has_more) =
                repository::list_definitions(pool, &actor, *offset, *limit).map_err(error)?;
            P::DefinitionListResponse {
                definitions,
                total,
                has_more,
            }
        }
        P::DefinitionGetRequest { definition_id } => {
            let (definition, timer_start, message_start) =
                repository::get_definition(pool, &actor, definition_id).map_err(error)?;
            P::DefinitionGetResponse {
                definition,
                timer_start,
                message_start,
            }
        }
        P::DefinitionSaveRequest {
            command_id,
            definition_id,
            expected_revision,
            name,
            description,
            model,
        } => P::DefinitionSaveResponse {
            definition: repository::save_definition(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                definition_id.as_deref(),
                *expected_revision,
                name,
                description,
                model,
            )
            .map_err(error)?,
        },
        P::DefinitionPublishRequest {
            command_id,
            definition_id,
            expected_revision,
            repin_calendar,
        } => {
            let stamp = stamp(payload, command_id)?;
            if let Some((definition, version)) =
                repository::replay_definition_publication(pool, &actor, &stamp, definition_id)
                    .map_err(error)?
            {
                return Ok(MessageBody::ProcessBody(P::DefinitionPublishResponse {
                    definition: (&definition).into(),
                    version,
                }));
            }
            let (definition, _, _) =
                repository::get_definition(pool, &actor, definition_id).map_err(error)?;
            crate::processes::model::validate_model(&definition.model).map_err(error)?;
            let mut snapshots = Vec::new();
            for node in &definition.model.nodes {
                if let ProcessNodeKind::ServiceTask { flow_id, .. } = &node.kind {
                    let executor = dispatcher(ctx)?;
                    let meta = executor
                        .authorize_process_flow(flow_id, &actor.user_id, &actor.org_id)
                        .map_err(|error| {
                            ProtocolError::new(ProtocolErrorCode::PolicyDenied, error.to_string())
                        })?;
                    let pinned = executor
                        .snapshot_flow(flow_id, &meta)
                        .map_err(|error| ProtocolError::bad_request(error.to_string()))?;
                    snapshots.push(PinnedServiceSnapshot {
                        info: PinnedFlowInfo {
                            node_id: node.id.clone(),
                            flow_id: pinned.flow_id,
                            source_version: pinned.source_version,
                            graph_sha256: pinned.graph_sha256,
                        },
                        graph_json: pinned.graph_json,
                    });
                }
            }
            let (definition, version) = repository::publish_definition(
                pool,
                &actor,
                &stamp,
                definition_id,
                *expected_revision,
                &snapshots,
                *repin_calendar,
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::DefinitionPublishResponse {
                definition: (&definition).into(),
                version,
            }
        }
        P::DefinitionArchiveRequest {
            command_id,
            definition_id,
            expected_revision,
            archived,
        } => {
            let definition = repository::archive_definition(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                definition_id,
                *expected_revision,
                *archived,
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::DefinitionArchiveResponse { definition }
        }
        P::VersionListRequest {
            definition_id,
            offset,
            limit,
        } => {
            let (versions, total, has_more) =
                repository::list_versions(pool, &actor, definition_id, *offset, *limit)
                    .map_err(error)?;
            P::VersionListResponse {
                versions,
                total,
                has_more,
            }
        }
        P::VersionGetRequest {
            definition_id,
            version,
        } => P::VersionGetResponse {
            version: repository::get_version(pool, &actor, definition_id, *version)
                .map_err(error)?,
        },
        P::XmlImportRequest { xml } => {
            let (model, diagnostics) = bpmn::import_xml(xml);
            P::XmlImportResponse { model, diagnostics }
        }
        P::XmlExportRequest {
            definition_id,
            version,
        } => {
            let model = match version {
                Some(version) => {
                    repository::get_version(pool, &actor, definition_id, *version)
                        .map_err(error)?
                        .model
                }
                None => {
                    repository::get_definition(pool, &actor, definition_id)
                        .map_err(error)?
                        .0
                        .model
                }
            };
            P::XmlExportResponse {
                xml: bpmn::export_xml(&model).map_err(error)?,
            }
        }
        P::InstanceStartRequest {
            command_id,
            definition_id,
            version,
            variables,
        } => {
            let stamp = stamp(payload, command_id)?;
            let instance = if let Some(prior) =
                repository::replay_instance_command(pool, &actor, &stamp, None).map_err(error)?
            {
                prior
            } else {
                let published = repository::get_version(pool, &actor, definition_id, *version)
                    .map_err(error)?;
                crate::processes::model::validate_variables(variables).map_err(error)?;
                let mut merged = serde_json::to_value(&published.model.variables)
                    .map_err(|error| ProtocolError::internal(error.to_string()))?;
                let object = merged.as_object_mut().ok_or_else(|| {
                    ProtocolError::bad_request("process variables must be an object")
                })?;
                for (key, value) in variables.as_object().ok_or_else(|| {
                    ProtocolError::bad_request("process variables must be an object")
                })? {
                    object.insert(key.clone(), value.clone());
                }
                let instance_id = uuid::Uuid::new_v4().to_string();
                let at_ms = chrono::Utc::now().timestamp_millis();
                let plan = runtime::plan_start(
                    &published.model,
                    &instance_id,
                    &actor,
                    definition_id,
                    *version,
                    merged,
                    runtime::StartCause::Manual,
                    at_ms,
                )
                .map_err(error)?;
                repository::start_instance(
                    pool,
                    &actor,
                    &stamp,
                    &instance_id,
                    definition_id,
                    *version,
                    variables,
                    &plan,
                    at_ms,
                )
                .map_err(error)?
            };
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::InstanceStartResponse { instance }
        }
        P::InstanceListRequest {
            definition_id,
            offset,
            limit,
        } => {
            let (instances, total, has_more) =
                repository::list_instances(pool, &actor, definition_id.as_deref(), *offset, *limit)
                    .map_err(error)?;
            P::InstanceListResponse {
                instances,
                total,
                has_more,
            }
        }
        P::InstanceGetRequest { instance_id, pages } => P::InstanceGetResponse {
            instance: repository::get_instance(pool, &actor, instance_id, pages.as_ref())
                .map_err(error)?,
        },
        P::UserTaskGetRequest {
            instance_id,
            user_task_id,
        } => P::UserTaskGetResponse {
            task: repository::get_user_task(pool, &actor, instance_id, user_task_id)
                .map_err(error)?,
        },
        P::UserTaskCompleteRequest {
            command_id,
            instance_id,
            user_task_id,
            expected_revision,
            outputs,
            approved,
        } => {
            let stamp = stamp(payload, command_id)?;
            let instance = if let Some(prior) =
                repository::replay_instance_command(pool, &actor, &stamp, Some(instance_id))
                    .map_err(error)?
            {
                prior
            } else {
                let snapshot =
                    repository::runtime_snapshot(pool, &actor, instance_id).map_err(error)?;
                let at_ms = chrono::Utc::now().timestamp_millis();
                let plan = runtime::plan_user_completion(
                    &snapshot,
                    user_task_id,
                    outputs,
                    *approved,
                    at_ms,
                )
                .map_err(error)?;
                repository::complete_user_task(
                    pool,
                    &actor,
                    &stamp,
                    instance_id,
                    user_task_id,
                    *expected_revision,
                    outputs,
                    *approved,
                    &plan,
                    at_ms,
                )
                .map_err(error)?
            };
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::UserTaskCompleteResponse { instance }
        }
        P::InstanceCancelRequest {
            command_id,
            instance_id,
            expected_revision,
        } => {
            let instance = repository::cancel_instance(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                instance_id,
                *expected_revision,
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::cancel_instance(executor, instance_id);
            }
            P::InstanceCancelResponse { instance }
        }
        P::JobRetryRequest {
            command_id,
            instance_id,
            job_id,
            expected_revision,
        } => {
            let instance = repository::retry_job(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                instance_id,
                job_id,
                *expected_revision,
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::JobRetryResponse { instance }
        }
        P::HistoryRequest {
            instance_id,
            after_seq,
            limit,
        } => {
            let (events, next_seq, has_more) =
                repository::list_events(pool, &actor, instance_id, *after_seq, *limit)
                    .map_err(error)?;
            P::HistoryResponse {
                events,
                next_seq,
                has_more,
            }
        }
        P::MessageSendRequest {
            command_id,
            message_id,
            target,
            message_name,
            correlation_key,
            payload: business_payload,
            ttl_seconds,
        } => {
            let prepared = repository::PreparedMessage {
                message_id: message_id.clone(),
                target: target.clone(),
                message_name: message_name.clone(),
                correlation_key: correlation_key.clone(),
                payload: business_payload.clone(),
                ttl_seconds: *ttl_seconds,
            };
            let message = repository::send_message(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                &prepared,
                chrono::Utc::now().timestamp_millis(),
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::MessageSendResponse { message }
        }
        P::MessageGetRequest {
            sender_user_id,
            message_id,
        } => P::MessageGetResponse {
            message: repository::get_message(pool, &actor, sender_user_id, message_id)
                .map_err(error)?,
        },
        P::MessageListRequest {
            definition_id,
            instance_id,
            offset,
            limit,
        } => {
            let (messages, total, has_more) = repository::list_messages(
                pool,
                &actor,
                definition_id.as_deref(),
                instance_id.as_deref(),
                *offset,
                *limit,
            )
            .map_err(error)?;
            P::MessageListResponse {
                messages,
                total,
                has_more,
            }
        }
        P::MessageResolveRequest {
            command_id,
            message_id,
            expected_revision,
            instance_id,
            subscription_id,
        } => {
            let message = repository::resolve_message(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                message_id,
                *expected_revision,
                instance_id,
                subscription_id,
                chrono::Utc::now().timestamp_millis(),
            )
            .map_err(error)?;
            if let Some(executor) = ctx.state.router.flow_dispatcher() {
                runtime::wake(executor);
            }
            P::MessageResolveResponse { message }
        }
        P::MessageCancelRequest {
            command_id,
            message_id,
            expected_revision,
        } => P::MessageCancelResponse {
            message: repository::cancel_message(
                pool,
                &actor,
                &stamp(payload, command_id)?,
                message_id,
                *expected_revision,
                chrono::Utc::now().timestamp_millis(),
            )
            .map_err(error)?,
        },
        P::MessageSendResponse { .. }
        | P::MessageGetResponse { .. }
        | P::MessageListResponse { .. }
        | P::MessageResolveResponse { .. }
        | P::MessageCancelResponse { .. }
        | P::OptionsResponse { .. }
        | P::DefinitionListResponse { .. }
        | P::DefinitionGetResponse { .. }
        | P::DefinitionSaveResponse { .. }
        | P::DefinitionPublishResponse { .. }
        | P::DefinitionArchiveResponse { .. }
        | P::VersionListResponse { .. }
        | P::VersionGetResponse { .. }
        | P::XmlImportResponse { .. }
        | P::XmlExportResponse { .. }
        | P::InstanceStartResponse { .. }
        | P::InstanceListResponse { .. }
        | P::InstanceGetResponse { .. }
        | P::UserTaskGetResponse { .. }
        | P::UserTaskCompleteResponse { .. }
        | P::InstanceCancelResponse { .. }
        | P::JobRetryResponse { .. }
        | P::HistoryResponse { .. } => {
            return Err(ProtocolError::bad_request(
                "process responses are not accepted as requests",
            ))
        }
    };
    Ok(MessageBody::ProcessBody(response))
}

macro_rules! register_request {
    ($variant:literal) => {
        inventory::submit! {
            super::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: __tentaflow_policy_process_dispatch,
                metric_name: concat!("tentaflow_ws_handler_", $variant),
                dispatch_fn: __tentaflow_dispatch_process_dispatch,
            }
        }
    };
}

register_request!("ProcessOptionsRequest");
register_request!("ProcessDefinitionListRequest");
register_request!("ProcessDefinitionGetRequest");
register_request!("ProcessDefinitionSaveRequest");
register_request!("ProcessDefinitionPublishRequest");
register_request!("ProcessDefinitionArchiveRequest");
register_request!("ProcessVersionListRequest");
register_request!("ProcessVersionGetRequest");
register_request!("ProcessXmlImportRequest");
register_request!("ProcessXmlExportRequest");
register_request!("ProcessInstanceStartRequest");
register_request!("ProcessInstanceListRequest");
register_request!("ProcessInstanceGetRequest");
register_request!("ProcessUserTaskGetRequest");
register_request!("ProcessUserTaskCompleteRequest");
register_request!("ProcessInstanceCancelRequest");
register_request!("ProcessJobRetryRequest");
register_request!("ProcessHistoryRequest");

register_request!("ProcessMessageSendRequest");
register_request!("ProcessMessageGetRequest");
register_request!("ProcessMessageListRequest");
register_request!("ProcessMessageResolveRequest");
register_request!("ProcessMessageCancelRequest");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{AppState, RequestOrigin};
    use crate::processes::runtime::test_support;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tentaflow_protocol::processes::{
        ActivityVerification, ProcessDefinition, ProcessInstanceStatus, ProcessNode,
        ProcessTimerKind, ProcessTimerSpec, ProcessTimerStatus,
    };

    fn context(state: &Arc<AppState>, actor: &ProcessActor) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::parse_str(&actor.user_id).unwrap().as_bytes(),
                role: Some("user".into()),
            },
            correlation_id: 1,
            connection_id: 1,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(
                crate::services::rbac::resolve_org_context(
                    &state.db,
                    &actor.user_id,
                    Some(&actor.org_id),
                )
                .unwrap(),
            ),
            origin: RequestOrigin::Local,
        }
    }

    async fn request(ctx: &HandlerContext, payload: P) -> P {
        let response = super::super::dispatch(&MessageBody::ProcessBody(payload), ctx).await;
        assert!(
            !response.1,
            "actual process dispatch failed: {:?}",
            response.0
        );
        let MessageBody::ProcessBody(payload) = response.0 else {
            panic!("typed process response expected")
        };
        payload
    }

    async fn refused(ctx: &HandlerContext, payload: P) -> ProtocolError {
        let response = super::super::dispatch(&MessageBody::ProcessBody(payload), ctx).await;
        assert!(response.1, "actual process dispatch unexpectedly succeeded");
        let MessageBody::Error(error) = response.0 else {
            panic!("protocol error expected")
        };
        error
    }

    async fn save(
        ctx: &HandlerContext,
        model: tentaflow_protocol::processes::ProcessModel,
    ) -> ProcessDefinition {
        let P::DefinitionSaveResponse { definition } = request(
            ctx,
            P::DefinitionSaveRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: None,
                expected_revision: 0,
                name: "My evidence process".into(),
                description: "".into(),
                model,
            },
        )
        .await
        else {
            panic!("saved definition expected")
        };
        definition
    }

    #[tokio::test]
    async fn ordinary_authors_have_private_definitions_and_current_assignees_complete_their_tasks()
    {
        let state = AppState::for_test();
        let first = test_support::actor(&state.db, "first-author");
        let second = test_support::actor(&state.db, "second-author");
        let outsider = test_support::actor(&state.db, "unassigned-reader");
        let first_ctx = context(&state, &first);
        let second_ctx = context(&state, &second);
        let outsider_ctx = context(&state, &outsider);
        let definition = save(&first_ctx, test_support::user_model(Some(&second.user_id))).await;
        let other = save(&second_ctx, crate::processes::model::starter_model()).await;
        let P::DefinitionListResponse {
            definitions,
            total,
            has_more,
        } = request(
            &first_ctx,
            P::DefinitionListRequest {
                offset: 0,
                limit: 10,
            },
        )
        .await
        else {
            panic!("list expected")
        };
        assert_eq!(total, 1);
        assert!(!has_more);
        assert_eq!(definitions[0].definition_id, definition.definition_id);
        assert_ne!(other.definition_id, definition.definition_id);
        assert_eq!(
            refused(
                &second_ctx,
                P::DefinitionGetRequest {
                    definition_id: definition.definition_id.clone()
                }
            )
            .await
            .code,
            ProtocolErrorCode::NotFound
        );
        request(
            &first_ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                expected_revision: definition.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        let P::InstanceStartResponse { instance } = request(
            &first_ctx,
            P::InstanceStartRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                version: 1,
                variables: json!({}),
            },
        )
        .await
        else {
            panic!("started instance expected")
        };
        assert_eq!(instance.status, ProcessInstanceStatus::Waiting);
        assert_eq!(
            refused(
                &outsider_ctx,
                P::InstanceGetRequest {
                    instance_id: instance.instance_id.clone(),
                    pages: None
                }
            )
            .await
            .code,
            ProtocolErrorCode::NotFound
        );
        let task_id = instance.user_tasks[0].user_task_id.clone();
        assert_eq!(
            refused(
                &outsider_ctx,
                P::UserTaskGetRequest {
                    instance_id: instance.instance_id.clone(),
                    user_task_id: task_id.clone()
                }
            )
            .await
            .code,
            ProtocolErrorCode::NotFound
        );
        let P::InstanceGetResponse {
            instance: participant,
        } = request(
            &second_ctx,
            P::InstanceGetRequest {
                instance_id: instance.instance_id.clone(),
                pages: None,
            },
        )
        .await
        else {
            panic!("participant detail expected")
        };
        assert!(participant.user_tasks[0].can_complete);
        assert!(!participant.can_cancel);
        let completion = P::UserTaskCompleteRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            instance_id: instance.instance_id.clone(),
            user_task_id: task_id.clone(),
            expected_revision: instance.revision,
            outputs: json!({"answer":"verified"}),
            approved: None,
        };
        let P::UserTaskCompleteResponse {
            instance: completed,
        } = request(&second_ctx, completion.clone()).await
        else {
            panic!("completed instance expected")
        };
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.variables["answer"], "verified");
        let replay = request(&second_ctx, completion).await;
        let P::UserTaskCompleteResponse { instance: replayed } = replay else {
            panic!("completion replay expected")
        };
        assert_eq!(replayed.revision, completed.revision);
        let P::HistoryResponse {
            events, has_more, ..
        } = request(
            &second_ctx,
            P::HistoryRequest {
                instance_id: instance.instance_id.clone(),
                after_seq: 0,
                limit: 200,
            },
        )
        .await
        else {
            panic!("history expected")
        };
        assert!(!has_more);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "user_task_completed")
                .count(),
            1
        );
        crate::db::repository::update_user_account(
            &state.db,
            &second.user_id,
            "second-author",
            "second@example.test",
            false,
        )
        .unwrap();
        assert!(
            super::super::dispatch(
                &MessageBody::ProcessBody(P::InstanceGetRequest {
                    instance_id: instance.instance_id.clone(),
                    pages: None
                }),
                &second_ctx
            )
            .await
            .1
        );
        crate::db::repository::update_user_account(
            &state.db,
            &second.user_id,
            "second-author",
            "second@example.test",
            true,
        )
        .unwrap();
        crate::services::org::repo::remove_membership(&state.db, &second.org_id, &second.user_id)
            .unwrap();
        assert_eq!(
            refused(
                &second_ctx,
                P::InstanceGetRequest {
                    instance_id: instance.instance_id,
                    pages: None
                }
            )
            .await
            .code,
            ProtocolErrorCode::PolicyDenied
        );
    }

    #[tokio::test]
    async fn publication_replay_preserves_graph_and_mutable_subflow_publication_is_rejected() {
        let state = AppState::for_test();
        let actor = test_support::actor(&state.db, "pinned-author");
        let ctx = context(&state, &actor);
        let flow_id = test_support::flow(&state.db, &actor, &test_support::graph("before", None));
        let definition = save(
            &ctx,
            test_support::service_model(&flow_id, ActivityVerification::Human),
        )
        .await;
        let publication = P::DefinitionPublishRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            definition_id: definition.definition_id.clone(),
            expected_revision: definition.draft_revision,
            repin_calendar: None,
        };
        let P::DefinitionPublishResponse { version, .. } = request(&ctx, publication.clone()).await
        else {
            panic!("published version expected")
        };
        test_support::update_flow(
            &state.db,
            &actor,
            &flow_id,
            &test_support::graph("after", None),
        );
        let P::DefinitionPublishResponse {
            version: replayed, ..
        } = request(&ctx, publication).await
        else {
            panic!("publication replay expected")
        };
        assert_eq!(replayed, version);
        let P::VersionListResponse { total, .. } = request(
            &ctx,
            P::VersionListRequest {
                definition_id: definition.definition_id,
                offset: 0,
                limit: 10,
            },
        )
        .await
        else {
            panic!("version list expected")
        };
        assert_eq!(total, 1);
        let nested=json!({"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"sub","type":"subflow","config":{"flow_id":flow_id}},{"id":"o","type":"output","config":{}}],"edges":[{"from":"t","to":"sub","from_port":"text","to_port":"in"},{"from":"sub","to":"o","from_port":"full","to_port":"text"}]}).to_string();
        let nested_id = test_support::flow(&state.db, &actor, &nested);
        let definition = save(
            &ctx,
            test_support::service_model(&nested_id, ActivityVerification::Human),
        )
        .await;
        let denied = refused(
            &ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                expected_revision: definition.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::BadRequest);
        assert!(denied.message.contains("cannot pin"));
        let P::VersionListResponse { total, .. } = request(
            &ctx,
            P::VersionListRequest {
                definition_id: definition.definition_id,
                offset: 0,
                limit: 10,
            },
        )
        .await
        else {
            panic!("version list expected")
        };
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn typed_options_use_current_accounts_and_flow_acl_and_reject_response_direction() {
        let state = AppState::for_test();
        let actor = test_support::actor(&state.db, "option-author");
        let other = test_support::actor(&state.db, "inactive-option");
        let ctx = context(&state, &actor);
        crate::db::repository::update_user_account(
            &state.db,
            &other.user_id,
            "inactive-option",
            "inactive@example.test",
            false,
        )
        .unwrap();
        let allowed = test_support::flow(&state.db, &actor, &test_support::graph("allowed", None));
        let denied = test_support::flow(&state.db, &actor, &test_support::graph("denied", None));
        crate::db::repository::resource_permissions::set(
            &state.db,
            "flow",
            &denied,
            "user",
            &actor.user_id,
            "deny",
        )
        .unwrap();
        let P::OptionsResponse {
            assignees,
            service_flows,
        } = request(&ctx, P::OptionsRequest {}).await
        else {
            panic!("typed options expected")
        };
        assert!(assignees.iter().any(|user| user.user_id == actor.user_id));
        assert!(assignees.iter().all(|user| user.user_id != other.user_id));
        assert!(service_flows.iter().any(|flow| flow.flow_id == allowed));
        assert!(service_flows.iter().all(|flow| flow.flow_id != denied));
        let mut anonymous = ctx.clone();
        anonymous.session = SessionAuth::Anonymous;
        assert!(
            super::super::dispatch(&MessageBody::ProcessBody(P::OptionsRequest {}), &anonymous)
                .await
                .1
        );
        assert!(
            super::super::dispatch(
                &MessageBody::ProcessBody(P::OptionsResponse {
                    assignees: vec![],
                    service_flows: vec![]
                }),
                &ctx
            )
            .await
            .1
        );
        for name in [
            "ProcessDefinitionSaveRequest",
            "ProcessUserTaskGetRequest",
            "ProcessUserTaskCompleteRequest",
            "ProcessHistoryRequest",
            "ProcessJobRetryRequest",
        ] {
            assert!(
                super::super::find(name).is_some(),
                "nested typed handler {name} must be registered"
            );
        }
    }

    #[tokio::test]
    async fn oversized_binary_request_is_rejected_before_any_definition_mutation() {
        let state = AppState::for_test();
        let actor = test_support::actor(&state.db, "bounded-author");
        let ctx = context(&state, &actor);
        let error = refused(
            &ctx,
            P::DefinitionSaveRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: None,
                expected_revision: 0,
                name: "Bounded process".into(),
                description: "x".repeat(1024 * 1024),
                model: crate::processes::model::starter_model(),
            },
        )
        .await;
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        assert!(error.message.contains("binary frame budget"));
        let P::DefinitionListResponse {
            definitions, total, ..
        } = request(
            &ctx,
            P::DefinitionListRequest {
                offset: 0,
                limit: 10,
            },
        )
        .await
        else {
            panic!("list expected")
        };
        assert_eq!(total, 0);
        assert!(definitions.is_empty());
    }

    #[tokio::test]
    async fn published_timer_summary_is_private_and_manual_start_or_replay_cannot_rearm_it() {
        let state = AppState::for_test();
        let owner = test_support::actor(&state.db, "timed-author");
        let outsider = test_support::actor(&state.db, "timed-outsider");
        let ctx = context(&state, &owner);
        let outside = context(&state, &outsider);
        let mut model = crate::processes::model::starter_model();
        model.timer_timezone = Some("Europe/Warsaw".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Duration { seconds: 60 },
        };
        let definition = save(&ctx, model).await;
        let publish = P::DefinitionPublishRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            definition_id: definition.definition_id.clone(),
            expected_revision: definition.draft_revision,
            repin_calendar: None,
        };
        let P::DefinitionPublishResponse { version, .. } = request(&ctx, publish.clone()).await
        else {
            panic!("actual publication expected")
        };
        let P::DefinitionGetResponse {
            definition,
            timer_start: Some(summary),
            message_start: _,
        } = request(
            &ctx,
            P::DefinitionGetRequest {
                definition_id: definition.definition_id.clone(),
            },
        )
        .await
        else {
            panic!("actual persisted schedule expected")
        };
        assert_eq!(summary.kind, ProcessTimerKind::Start);
        assert_eq!(summary.status, ProcessTimerStatus::Pending);
        assert_eq!(summary.timezone, "Europe/Warsaw");
        assert_eq!(summary.due_at_ms, Some(version.published_at_ms + 60_000));
        assert_eq!(
            refused(
                &outside,
                P::DefinitionGetRequest {
                    definition_id: definition.definition_id.clone()
                }
            )
            .await
            .code,
            ProtocolErrorCode::NotFound
        );
        let denial = refused(
            &ctx,
            P::InstanceStartRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                version: version.version,
                variables: json!({}),
            },
        )
        .await;
        assert_eq!(denial.code, ProtocolErrorCode::BadRequest);
        assert!(denial.message.contains("cannot be started manually"));
        request(&ctx, publish).await;
        let P::DefinitionGetResponse {
            timer_start: Some(replayed),
            ..
        } = request(
            &ctx,
            P::DefinitionGetRequest {
                definition_id: definition.definition_id.clone(),
            },
        )
        .await
        else {
            panic!("replayed schedule expected")
        };
        assert_eq!(replayed, summary);
        let P::InstanceListResponse {
            total, instances, ..
        } = request(
            &ctx,
            P::InstanceListRequest {
                definition_id: Some(definition.definition_id),
                offset: 0,
                limit: 10,
            },
        )
        .await
        else {
            panic!("actual empty instance list expected")
        };
        assert_eq!(total, 0);
        assert!(instances.is_empty());
    }

    #[tokio::test]
    async fn participant_completion_arms_one_catch_with_commit_time_and_cancelled_summary() {
        let state = AppState::for_test();
        let owner = test_support::actor(&state.db, "catch-author");
        let participant = test_support::actor(&state.db, "catch-reviewer");
        let ctx = context(&state, &owner);
        let reviewer = context(&state, &participant);
        let mut model = test_support::user_model(Some(&participant.user_id));
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            id: "Wait".into(),
            name: "Wait after human work".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            },
        });
        model.sequence_flows = vec![
            test_support::edge("ToWork", "Start_1", "Work"),
            test_support::edge("ToWait", "Work", "Wait"),
            test_support::edge("ToEnd", "Wait", "End_1"),
        ];
        let definition = save(&ctx, model).await;
        request(
            &ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                expected_revision: definition.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        let P::InstanceStartResponse { instance } = request(
            &ctx,
            P::InstanceStartRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                version: 1,
                variables: json!({}),
            },
        )
        .await
        else {
            panic!("human wait expected")
        };
        let complete = P::UserTaskCompleteRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            instance_id: instance.instance_id.clone(),
            user_task_id: instance.user_tasks[0].user_task_id.clone(),
            expected_revision: instance.revision,
            outputs: json!({"answer":"checked"}),
            approved: None,
        };
        let P::UserTaskCompleteResponse { instance: waiting } =
            request(&reviewer, complete.clone()).await
        else {
            panic!("timer wait expected")
        };
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(waiting.timers.len(), 1);
        assert_eq!(
            waiting.timers[0].due_at_ms,
            Some(waiting.updated_at_ms + 60_000)
        );
        let snapshot =
            repository::runtime_snapshot(&state.db, &owner, &instance.instance_id).unwrap();
        assert_eq!(snapshot.timers[0].anchor_at_ms, waiting.updated_at_ms);
        assert_eq!(snapshot.timers[0].org_id, owner.org_id);
        assert_eq!(snapshot.timers[0].definition_id, definition.definition_id);
        assert_eq!(snapshot.timers[0].version, 1);
        let P::HistoryResponse { events: before, .. } = request(
            &reviewer,
            P::HistoryRequest {
                instance_id: instance.instance_id.clone(),
                after_seq: 0,
                limit: 200,
            },
        )
        .await
        else {
            panic!("participant history expected")
        };
        request(&reviewer, complete).await;
        let P::HistoryResponse { events: after, .. } = request(
            &reviewer,
            P::HistoryRequest {
                instance_id: instance.instance_id.clone(),
                after_seq: 0,
                limit: 200,
            },
        )
        .await
        else {
            panic!("idempotent participant history expected")
        };
        assert_eq!(before, after);
        assert_eq!(
            after
                .iter()
                .filter(|event| event.kind == "timer_armed")
                .count(),
            1
        );
        let P::InstanceCancelResponse {
            instance: cancelled,
        } = request(
            &ctx,
            P::InstanceCancelRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                instance_id: instance.instance_id.clone(),
                expected_revision: waiting.revision,
            },
        )
        .await
        else {
            panic!("actual cancelled timer expected")
        };
        assert_eq!(cancelled.timers[0].status, ProcessTimerStatus::Cancelled);
        assert_eq!(
            {
                let drained = crate::processes::timers::drain_due(
                    &state.db,
                    waiting.timers[0].due_at_ms.unwrap(),
                );
                drained.completion.unwrap();
                assert!(drained.cancelled_claims.is_empty());
                drained.fired
            },
            0
        );
        crate::services::org::repo::remove_membership(
            &state.db,
            &participant.org_id,
            &participant.user_id,
        )
        .unwrap();
        assert_eq!(
            refused(
                &reviewer,
                P::InstanceGetRequest {
                    instance_id: instance.instance_id,
                    pages: None
                }
            )
            .await
            .code,
            ProtocolErrorCode::PolicyDenied
        );
    }

    #[tokio::test]
    async fn private_calendar_publication_returns_current_pin_revision_and_rejects_stale_or_revoked_replay(
    ) {
        use tentaflow_protocol::processes::{
            HolidayPolicy, ProcessCalendarPinState, ProcessWorkCalendar, WorkWindow,
        };
        let state = AppState::for_test();
        let owner = test_support::actor(&state.db, "calendar-author");
        let other = test_support::actor(&state.db, "other-calendar-author");
        let ctx = context(&state, &owner);
        let outsider = context(&state, &other);
        let mut model = crate::processes::model::starter_model();
        model.timer_timezone = Some("America/Winnipeg".into());
        model.variables.insert(
            "project_id".into(),
            json!({"source_id":"opaque_business_fact"}),
        );
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Private business hours".into(),
            weekly_windows: (1..=7)
                .map(|weekday| WorkWindow {
                    weekday,
                    start_minute: 0,
                    end_minute: 1440,
                })
                .collect(),
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
        });
        let draft = save(&ctx, model).await;
        assert_eq!(
            draft.calendar_pin_state,
            Some(ProcessCalendarPinState::Unpinned)
        );
        let publication = P::DefinitionPublishRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            definition_id: draft.definition_id.clone(),
            expected_revision: draft.draft_revision,
            repin_calendar: None,
        };
        let reply = request(&ctx, publication.clone()).await;
        let P::DefinitionPublishResponse {
            definition,
            version,
        } = &reply
        else {
            panic!("actual calendar publication response");
        };
        assert_eq!(
            definition.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert_eq!(definition.draft_revision, draft.draft_revision + 1);
        assert!(version.model.calendar_pin.is_some());
        assert_eq!(
            version.model.variables["project_id"]["source_id"],
            "opaque_business_fact"
        );
        assert_eq!(request(&ctx, publication.clone()).await, reply);
        assert_eq!(
            refused(&outsider, publication.clone()).await.code,
            ProtocolErrorCode::NotFound
        );
        let mut edited = version.model.clone();
        edited.nodes[0].name = "Ordinary saved name after mint".into();
        let P::DefinitionSaveResponse {
            definition: ordinary,
        } = request(
            &ctx,
            P::DefinitionSaveRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: Some(definition.definition_id.clone()),
                expected_revision: definition.draft_revision,
                name: "Saved returned pin".into(),
                description: "".into(),
                model: edited,
            },
        )
        .await
        else {
            panic!("actual saved current pin");
        };
        assert_eq!(
            ordinary.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert_eq!(ordinary.model.calendar_pin, version.model.calendar_pin);
        let mut changed = ordinary.model.clone();
        changed.work_calendar.as_mut().unwrap().holiday_policy = HolidayPolicy::PolandStatutory;
        let P::DefinitionSaveResponse { definition: stale } = request(
            &ctx,
            P::DefinitionSaveRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: Some(ordinary.definition_id.clone()),
                expected_revision: ordinary.draft_revision,
                name: ordinary.name.clone(),
                description: ordinary.description.clone(),
                model: changed,
            },
        )
        .await
        else {
            panic!("actual stale draft");
        };
        assert_eq!(
            stale.calendar_pin_state,
            Some(ProcessCalendarPinState::Stale)
        );
        refused(
            &ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: stale.definition_id.clone(),
                expected_revision: stale.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        let refresh = P::DefinitionPublishRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            definition_id: stale.definition_id.clone(),
            expected_revision: stale.draft_revision,
            repin_calendar: Some(true),
        };
        let refreshed = request(&ctx, refresh.clone()).await;
        let P::DefinitionPublishResponse {
            definition: current,
            version: v2,
        } = &refreshed
        else {
            panic!("explicit refreshed publication");
        };
        assert_eq!(
            current.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert_eq!(v2.version, 2);
        assert_ne!(v2.model.calendar_pin, version.model.calendar_pin);
        let bytes = tentaflow_protocol::cbor::encode(&MessageBody::ProcessBody(refreshed)).unwrap();
        assert!(bytes.len() < 900 * 1024);
        state
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&owner.user_id],
            )
            .unwrap();
        refused(&ctx, refresh).await;
        assert_eq!(
            state
                .db
                .read()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM bpmn_versions", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            2
        );
    }
    #[tokio::test]
    async fn typed_message_ingress_current_participant_payload_replay_and_selected_page_remain_authorized(
    ) {
        use tentaflow_protocol::processes::{
            ProcessInstancePageRequest, ProcessMessageStatus, ProcessMessageTarget, ProcessPageSpec,
        };
        let state = AppState::for_test();
        let owner = test_support::actor(&state.db, "message-author");
        let participant = test_support::actor(&state.db, "message-participant");
        let outsider = test_support::actor(&state.db, "message-outsider");
        let owner_ctx = context(&state, &owner);
        let participant_ctx = context(&state, &participant);
        let outsider_ctx = context(&state, &outsider);
        let mut model = crate::processes::messages::test_support::receiving_model(false, true);
        if let ProcessNodeKind::UserTask {
            assignee_user_id, ..
        } = &mut model.nodes[1].kind
        {
            *assignee_user_id = Some(participant.user_id.clone());
        }
        let definition = save(&owner_ctx, model).await;
        request(
            &owner_ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                expected_revision: definition.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        let P::InstanceStartResponse { instance } = request(
            &owner_ctx,
            P::InstanceStartRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: definition.definition_id.clone(),
                version: 1,
                variables: json!({}),
            },
        )
        .await
        else {
            panic!("instance expected")
        };
        let message_id = uuid::Uuid::new_v4().to_string();
        let send = P::MessageSendRequest {
            command_id: uuid::Uuid::new_v4().to_string(),
            message_id: message_id.clone(),
            target: ProcessMessageTarget::Catch {
                definition_id: definition.definition_id.clone(),
                instance_id: Some(instance.instance_id.clone()),
                subscription_id: None,
            },
            message_name: "EvidenceReady".into(),
            correlation_key: "case-1".into(),
            payload: Value::Null,
            ttl_seconds: 120,
        };
        let P::MessageSendResponse { message } = request(&participant_ctx, send.clone()).await
        else {
            panic!("message expected")
        };
        assert_eq!(message.status, ProcessMessageStatus::Pending);
        assert_eq!(message.sender_user_id, participant.user_id);
        let P::MessageSendResponse { message: replayed } =
            request(&participant_ctx, send.clone()).await
        else {
            panic!("replayed message expected")
        };
        assert_eq!(replayed.message_id, message_id);
        let mut conflict = send.clone();
        if let P::MessageSendRequest {
            command_id,
            payload,
            ..
        } = &mut conflict
        {
            *command_id = uuid::Uuid::new_v4().to_string();
            *payload = json!({"customer_ID": 7});
        }
        refused(&participant_ctx, conflict).await;
        refused(&outsider_ctx, send.clone()).await;
        refused(
            &participant_ctx,
            P::DefinitionGetRequest {
                definition_id: definition.definition_id.clone(),
            },
        )
        .await;
        let mut wide = send;
        if let P::MessageSendRequest {
            command_id,
            message_id,
            target,
            ..
        } = &mut wide
        {
            *command_id = uuid::Uuid::new_v4().to_string();
            *message_id = uuid::Uuid::new_v4().to_string();
            *target = ProcessMessageTarget::Catch {
                definition_id: definition.definition_id.clone(),
                instance_id: None,
                subscription_id: None,
            };
        }
        refused(&participant_ctx, wide).await;
        let P::MessageGetResponse { message: detail } = request(
            &participant_ctx,
            P::MessageGetRequest {
                sender_user_id: participant.user_id.clone(),
                message_id: message_id.clone(),
            },
        )
        .await
        else {
            panic!("full message expected")
        };
        assert!(detail.message.payload_available);
        assert_eq!(detail.payload, Some(Value::Null));
        let frame =
            tentaflow_protocol::cbor::encode(&MessageBody::ProcessBody(P::MessageGetResponse {
                message: detail,
            }))
            .unwrap();
        let MessageBody::ProcessBody(P::MessageGetResponse { message: decoded }) =
            tentaflow_protocol::cbor::decode(&frame).unwrap()
        else {
            panic!("typed CBOR detail expected")
        };
        assert!(decoded.message.payload_available);
        assert_eq!(decoded.payload, Some(Value::Null));
        let maximum_payload = json!({"customer_ID": vec![0.1_f64; 65_526], "attached_to_id": null});
        crate::processes::runtime::validate_output(&maximum_payload).unwrap();
        let maximum_id = uuid::Uuid::new_v4().to_string();
        request(
            &participant_ctx,
            P::MessageSendRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                message_id: maximum_id.clone(),
                target: tentaflow_protocol::processes::ProcessMessageTarget::Catch {
                    definition_id: definition.definition_id.clone(),
                    instance_id: Some(instance.instance_id.clone()),
                    subscription_id: None,
                },
                message_name: "界".repeat(85) + "x",
                correlation_key: "界".repeat(85) + "x",
                payload: maximum_payload.clone(),
                ttl_seconds: 120,
            },
        )
        .await;
        let maximum = request(
            &participant_ctx,
            P::MessageGetRequest {
                sender_user_id: participant.user_id.clone(),
                message_id: maximum_id,
            },
        )
        .await;
        let frame = tentaflow_protocol::cbor::encode(&MessageBody::ProcessBody(maximum)).unwrap();
        assert!(frame.len() > 580_000 && frame.len() < 900 * 1024);
        let MessageBody::ProcessBody(P::MessageGetResponse { message: maximum }) =
            tentaflow_protocol::cbor::decode(&frame).unwrap()
        else {
            panic!("full maximum payload expected")
        };
        assert_eq!(maximum.payload, Some(maximum_payload));
        let task_id = instance.user_tasks[0].user_task_id.clone();
        let pages = ProcessInstancePageRequest {
            user_tasks: Some(ProcessPageSpec {
                offset: 1,
                limit: 20,
            }),
            incidents: None,
            timers: None,
            subscriptions: None,
            event_races: None,
            outgoing_messages: None,
            selected_user_task_id: Some(task_id.clone()),
            selected_incident_id: None,
        };
        let P::InstanceGetResponse { instance: selected } = request(
            &participant_ctx,
            P::InstanceGetRequest {
                instance_id: instance.instance_id.clone(),
                pages: Some(pages),
            },
        )
        .await
        else {
            panic!("selected detail expected")
        };
        assert!(selected.user_tasks.is_empty());
        assert_eq!(selected.pages.unwrap().user_tasks.total, 1);
        assert_eq!(selected.selected_user_task.unwrap().user_task_id, task_id);
        request(
            &participant_ctx,
            P::UserTaskCompleteRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                instance_id: instance.instance_id.clone(),
                user_task_id: task_id,
                expected_revision: instance.revision,
                outputs: json!({}),
                approved: None,
            },
        )
        .await;
        let drain = crate::processes::messages::drain_pending(
            &state.db,
            chrono::Utc::now().timestamp_millis(),
        );
        drain.completion.unwrap();
        assert_eq!(drain.delivered, 1);
        let P::InstanceGetResponse { instance: finished } = request(
            &participant_ctx,
            P::InstanceGetRequest {
                instance_id: instance.instance_id.clone(),
                pages: None,
            },
        )
        .await
        else {
            panic!("completed participant detail expected")
        };
        assert_eq!(finished.status, ProcessInstanceStatus::Completed);
        assert_eq!(finished.can_send_message, Some(false));
        let P::MessageGetResponse { message: receipt } = request(
            &participant_ctx,
            P::MessageGetRequest {
                sender_user_id: participant.user_id.clone(),
                message_id: message_id.clone(),
            },
        )
        .await
        else {
            panic!("receipt expected")
        };
        assert_eq!(receipt.message.status, ProcessMessageStatus::Delivered);
        crate::db::repository::update_user_account(
            &state.db,
            &participant.user_id,
            "message-participant",
            "participant@example.test",
            false,
        )
        .unwrap();
        refused(
            &participant_ctx,
            P::MessageGetRequest {
                sender_user_id: participant.user_id.clone(),
                message_id,
            },
        )
        .await;
        let start_definition = save(
            &owner_ctx,
            crate::processes::messages::test_support::receiving_model(true, false),
        )
        .await;
        request(
            &owner_ctx,
            P::DefinitionPublishRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: start_definition.definition_id.clone(),
                expected_revision: start_definition.draft_revision,
                repin_calendar: None,
            },
        )
        .await;
        let P::DefinitionGetResponse {
            message_start: Some(start),
            ..
        } = request(
            &owner_ctx,
            P::DefinitionGetRequest {
                definition_id: start_definition.definition_id.clone(),
            },
        )
        .await
        else {
            panic!("message start summary expected")
        };
        assert!(start.can_send);
        assert_eq!(start.message_name, "EvidenceReady");
        refused(
            &owner_ctx,
            P::InstanceStartRequest {
                command_id: uuid::Uuid::new_v4().to_string(),
                definition_id: start_definition.definition_id,
                version: 1,
                variables: json!({}),
            },
        )
        .await;
    }
}
