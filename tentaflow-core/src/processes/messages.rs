// ============ File: messages.rs — local durable message preparation, correlation, and bounded delivery ============

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessMessageTarget, ProcessMessageTargetSpec, ProcessModel, ProcessNode, ProcessNodeKind,
};
use uuid::Uuid;

use super::repository::{
    self, CancelledJobClaim, MessageDeliveryTarget, MessageSelection, MessageSnapshot,
    PreparedMessage, RuntimePlan,
};
use crate::db::DbPool;

pub fn validate_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && key.len() <= 256 && !key.chars().any(char::is_control),
        "message name/correlation must be a nonempty literal string of at most 256 UTF-8 bytes"
    );
    Ok(())
}
pub fn validate_message(message: &PreparedMessage) -> Result<()> {
    ensure!(
        Uuid::parse_str(&message.message_id).is_ok(),
        "message_id must be a UUID"
    );
    validate_key(&message.message_name)?;
    validate_key(&message.correlation_key)?;
    super::runtime::validate_output(&message.payload)?;
    ensure!(
        (1..=604800).contains(&message.ttl_seconds),
        "message TTL must be 1..604800 seconds"
    );
    let (definition_id, instance_id, subscription_id) = match &message.target {
        ProcessMessageTarget::Start { definition_id } => (definition_id, None, None),
        ProcessMessageTarget::Catch {
            definition_id,
            instance_id,
            subscription_id,
        } => (
            definition_id,
            instance_id.as_deref(),
            subscription_id.as_deref(),
        ),
    };
    ensure!(
        Uuid::parse_str(definition_id).is_ok(),
        "message definition_id must be a UUID"
    );
    ensure!(
        subscription_id.is_none() || instance_id.is_some(),
        "exact subscription needs an instance"
    );
    for id in [instance_id, subscription_id].into_iter().flatten() {
        ensure!(
            Uuid::parse_str(id).is_ok(),
            "message address must be a UUID"
        );
    }
    Ok(())
}
pub fn evaluate_key(expression: &str, variables: &Value) -> Result<String> {
    let value = super::runtime::evaluate(expression, variables, &Value::Null, &[])?;
    let key = value
        .as_str()
        .context("message correlation expression must return a string")?;
    validate_key(key)?;
    Ok(key.to_owned())
}
fn expression_uuid(expression: &str, variables: &Value) -> Result<String> {
    let value = super::runtime::evaluate(expression, variables, &Value::Null, &[])?;
    let id = value
        .as_str()
        .context("message address expression must return a UUID string")?;
    ensure!(
        Uuid::parse_str(id).is_ok(),
        "message address expression must return a UUID string"
    );
    Ok(id.to_owned())
}
pub fn prepare_throw(
    model: &ProcessModel,
    node: &ProcessNode,
    variables: &Value,
) -> Result<PreparedMessage> {
    let (message_ref, target, correlation_expression, payload_expression, ttl_seconds) =
        match &node.kind {
            ProcessNodeKind::MessageThrow { message_ref, target, correlation_expression, payload_expression, ttl_seconds }
            | ProcessNodeKind::SendTask { message_ref, target, correlation_expression, payload_expression, ttl_seconds } =>
                (message_ref, target, correlation_expression, payload_expression, ttl_seconds),
            _ => anyhow::bail!("message-producing node required"),
        };
    let message_name = model
        .messages
        .iter()
        .find(|m| &m.message_id == message_ref)
        .context("throw declaration missing")?
        .name
        .clone();
    let target = match target {
        ProcessMessageTargetSpec::Start { definition_id } => ProcessMessageTarget::Start {
            definition_id: definition_id.clone(),
        },
        ProcessMessageTargetSpec::Catch {
            definition_id,
            instance_id_expression,
            subscription_id_expression,
        } => ProcessMessageTarget::Catch {
            definition_id: definition_id.clone(),
            instance_id: instance_id_expression
                .as_ref()
                .map(|e| expression_uuid(e, variables))
                .transpose()?,
            subscription_id: subscription_id_expression
                .as_ref()
                .map(|e| expression_uuid(e, variables))
                .transpose()?,
        },
    };
    let message = PreparedMessage {
        message_id: Uuid::new_v4().to_string(),
        target,
        message_name,
        correlation_key: evaluate_key(correlation_expression, variables)?,
        payload: super::runtime::evaluate(payload_expression, variables, &Value::Null, &[])?,
        ttl_seconds: *ttl_seconds,
    };
    validate_message(&message)?;
    Ok(message)
}
pub(super) fn mapped_variables(
    mapping: &std::collections::BTreeMap<String, String>,
    local: &Value,
    effective: &Value,
    payload: &Value,
    scope_name: &str,
    metadata: Value,
) -> Result<Value> {
    let mut result = local.clone();
    let target = result
        .as_object_mut()
        .context("process variables must be an object")?;
    for (name, expression) in mapping {
        target.insert(
            name.clone(),
            super::runtime::evaluate(
                expression,
                effective,
                payload,
                &[(scope_name.to_owned(), metadata.clone())],
            )?,
        );
    }
    super::model::validate_variables(&result)?;
    Ok(result)
}
pub fn plan_message_delivery(prepared: &MessageSnapshot, at_ms: i64) -> Result<RuntimePlan> {
    let m = &prepared.message;
    let payload = m
        .payload
        .as_ref()
        .context("message payload was pruned before delivery")?;
    let metadata = json!({"message_id":m.key.message_id,"sender_user_id":m.key.sender_user_id,"message_name":m.message_name,"correlation_key":m.correlation_key,"received_at_ms":m.received_at_ms,"expires_at_ms":m.expires_at_ms,"payload_sha256":m.payload_sha256});
    match &prepared.target {
        MessageDeliveryTarget::Start {
            actor,
            version,
            instance_id,
        } => {
            let start = version
                .model
                .nodes
                .iter()
                .find(|n| matches!(n.kind, ProcessNodeKind::MessageStart { .. }))
                .context("message start missing")?;
            let ProcessNodeKind::MessageStart { output_mapping, .. } = &start.kind else {
                anyhow::bail!("message start kind mismatch")
            };
            let vars = mapped_variables(
                output_mapping,
                &serde_json::to_value(&version.model.variables)?,
                &serde_json::to_value(&version.model.variables)?,
                payload,
                "message",
                metadata.clone(),
            )?;
            let mut plan = super::runtime::plan_start(
                &version.model,
                instance_id,
                actor,
                &version.definition_id,
                version.version,
                vars,
                super::runtime::StartCause::Message {
                    message_id: m.key.message_id.clone(),
                },
                at_ms,
                repository::StartInputRef::Message {
                    org_id: m.key.org_id.clone(),
                    sender_user_id: m.key.sender_user_id.clone(),
                    message_id: m.key.message_id.clone(),
                    expected_message_revision: m.revision,
                },
            )?;
            plan.events.push(repository::PlannedEvent {
                scope_id: instance_id.clone(),
                kind: "message_delivered".into(),
                node_id: Some(start.id.clone()),
                data: json!({"message":metadata,"payload":payload,"message_id":m.key.message_id}),
            });
            Ok(plan)
        }
        MessageDeliveryTarget::Catch {
            subscription,
            snapshot,
            ..
        } => super::runtime::plan_message_catch(snapshot, subscription, payload, metadata, at_ms,
            repository::AcceptedInputRef::Message {
                org_id: m.key.org_id.clone(),
                sender_user_id: m.key.sender_user_id.clone(),
                message_id: m.key.message_id.clone(),
                expected_message_revision: m.revision,
                target_subscription_id: Some(subscription.subscription_id.clone()),
                expected_subscription_revision: Some(subscription.revision),
            }),
    }
}
#[derive(Debug)]
pub struct MessageDrainOutcome {
    pub delivered: u32,
    pub cancelled_claims: Vec<CancelledJobClaim>,
    pub completion: Result<()>,
}

pub fn drain_pending(pool: &DbPool, at_ms: i64) -> MessageDrainOutcome {
    let mut result = MessageDrainOutcome {
        delivered: 0,
        cancelled_claims: Vec::new(),
        completion: Ok(()),
    };
    result.completion = (|| -> Result<()> {
        repository::expire_messages(pool, at_ms, 32)?;
        repository::prune_message_payloads(pool, at_ms, 32)?;
        for candidate in repository::due_messages(pool, at_ms, 32)? {
            let attempt = (|| -> Result<()> {
                match repository::message_snapshot(pool, &candidate)? {
                    MessageSelection::Ready(snapshot) => {
                        let plan = plan_message_delivery(&snapshot, at_ms)?;
                        if let Some(committed) =
                            repository::deliver_message(pool, &snapshot, &plan, at_ms)?
                        {
                            result.delivered += 1;
                            result
                                .cancelled_claims
                                .extend(committed.transition.cancelled_claims);
                        }
                    }
                    MessageSelection::NoMatch => {
                        repository::record_message_waiting(pool, &candidate, at_ms)?;
                    }
                    MessageSelection::Ambiguous => {
                        repository::record_message_ambiguous(pool, &candidate, at_ms)?;
                    }
                    MessageSelection::Stale => {}
                }
                Ok(())
            })();
            if let Err(error) = attempt {
                tracing::error!(message_id=%candidate.key.message_id,error=%error,"process message delivery failed");
                if error.downcast_ref::<rusqlite::Error>().is_some() {
                    return Err(error);
                }
                let reason = format!("{error:#}");
                if error
                    .downcast_ref::<repository::ProcessAuthorityDenied>()
                    .is_some()
                {
                    repository::record_message_blocked(pool, &candidate, &reason, at_ms)?;
                } else {
                    repository::record_message_failed(pool, &candidate, &reason, at_ms)?;
                }
            }
        }
        Ok(())
    })();
    result
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::processes::runtime::{
        self,
        test_support::{edge, publish_model, stamp, Fixture},
    };
    use std::collections::BTreeMap;
    use tentaflow_protocol::processes::{
        ProcessInstance, ProcessMessageDeclaration, ProcessVersion,
    };

    pub fn receiving_model(start: bool, gate: bool) -> ProcessModel {
        let mut model = crate::processes::model::starter_model();
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(),
            name: "EvidenceReady".into(),
        });
        if start {
            model.nodes[0].kind = ProcessNodeKind::MessageStart {
                message_ref: "Message_1".into(),
                output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
            };
        } else {
            model.nodes.insert(
                1,
                ProcessNode {
                    id: "Catch_1".into(),
                    name: "Receive evidence".into(),
                    kind: ProcessNodeKind::MessageCatch {
                        message_ref: "Message_1".into(),
                        correlation_expression: "'case-1'".into(),
                        output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
                    },
                    repeat: None,
                },
            );
            model.sequence_flows = vec![
                edge("ToCatch", "Start_1", "Catch_1"),
                edge("CatchEnd", "Catch_1", "End_1"),
            ];
            if gate {
                model.nodes.insert(
                    1,
                    ProcessNode {
                        id: "Gate_1".into(),
                        name: "Allow receipt".into(),
                        kind: ProcessNodeKind::UserTask {
                            assignee_user_id: None,
                            output_mapping: BTreeMap::new(),
                        },
                        repeat: None,
                    },
                );
                model.sequence_flows = vec![
                    edge("ToGate", "Start_1", "Gate_1"),
                    edge("GateCatch", "Gate_1", "Catch_1"),
                    edge("CatchEnd", "Catch_1", "End_1"),
                ];
            }
        }
        model
    }
    pub fn start_version(f: &Fixture, version: &ProcessVersion) -> ProcessInstance {
        let id = Uuid::new_v4().to_string();
        let vars = serde_json::to_value(&version.model.variables).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let command = stamp("start message fixture");
        let plan = runtime::plan_start(
            &version.model,
            &id,
            &f.owner,
            &version.definition_id,
            version.version,
            vars.clone(),
            runtime::StartCause::Manual,
            now,
            runtime::test_support::manual_input(&command),
        )
        .unwrap();
        repository::start_instance(
            &f.db,
            &f.owner,
            &command,
            &id,
            &version.definition_id,
            version.version,
            &vars,
            &plan,
            now,
        )
        .unwrap()
    }
    pub fn complete(f: &Fixture, instance: &str, task_id: &str) -> ProcessInstance {
        let snapshot = repository::runtime_snapshot(&f.db, &f.owner, instance).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let command = stamp("complete actual work");
        let plan =
            runtime::plan_user_completion(&snapshot, task_id, &json!({}), None, now,
                runtime::test_support::human_input(&snapshot, task_id, &command)).unwrap();
        repository::complete_user_task(
            &f.db,
            &f.owner,
            &command,
            instance,
            task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            &plan,
            now,
        )
        .unwrap()
        .instance
    }
    pub fn envelope(target: ProcessMessageTarget, payload: Value) -> PreparedMessage {
        PreparedMessage {
            message_id: Uuid::new_v4().to_string(),
            target,
            message_name: "EvidenceReady".into(),
            correlation_key: "case-1".into(),
            payload,
            ttl_seconds: 120,
        }
    }
    pub fn catch_target(
        version: &ProcessVersion,
        id: Option<&str>,
        sub: Option<&str>,
    ) -> ProcessMessageTarget {
        ProcessMessageTarget::Catch {
            definition_id: version.definition_id.clone(),
            instance_id: id.map(str::to_owned),
            subscription_id: sub.map(str::to_owned),
        }
    }
    pub fn send(
        f: &Fixture,
        message: &PreparedMessage,
    ) -> tentaflow_protocol::processes::ProcessMessageSummary {
        repository::send_message(
            &f.db,
            &f.owner,
            &stamp("send envelope"),
            message,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
    }
    pub fn current(
        f: &Fixture,
        m: &PreparedMessage,
    ) -> tentaflow_protocol::processes::ProcessMessageDetail {
        repository::get_message(&f.db, &f.owner, &f.owner.user_id, &m.message_id).unwrap()
    }
    pub fn boundary_messages(
        mut model: ProcessModel,
        activity: &str,
        definitions: &[(&str, bool, &str)],
    ) -> ProcessModel {
        let end_id = model
            .nodes
            .iter()
            .find(|node| node.kind == ProcessNodeKind::End)
            .unwrap()
            .id
            .clone();
        for (id, cancel, name) in definitions {
            let declaration = format!("Declaration_{id}");
            model.messages.push(ProcessMessageDeclaration {
                message_id: declaration.clone(),
                name: (*name).into(),
            });
            model.nodes.push(ProcessNode {
                id: (*id).into(),
                name: format!("Boundary {id}"),
                kind: ProcessNodeKind::BoundaryMessage {
                    attached_to_id: activity.into(),
                    cancel_activity: *cancel,
                    message_ref: declaration,
                    correlation_expression: "'case-1'".into(),
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
            });
            model.nodes.push(ProcessNode {
                id: format!("Side_{id}"),
                name: format!("Handle {id}"),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
            });
            model.sequence_flows.extend([
                edge(&format!("BoundaryPath_{id}"), id, &format!("Side_{id}")),
                edge(&format!("BoundaryEnd_{id}"), &format!("Side_{id}"), &end_id),
            ]);
        }
        model
    }
    pub fn race_model() -> ProcessModel {
        let mut model = receiving_model(false, false);
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            id: "Race_1".into(),
            name: "First signal".into(),
            kind: ProcessNodeKind::EventBasedGateway,
            repeat: None,
        });
        model.nodes.push(ProcessNode {
            id: "Timer_1".into(),
            name: "Deadline".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 2 },
            },
            repeat: None,
        });
        model.sequence_flows = vec![
            edge("ToRace", "Start_1", "Race_1"),
            edge("RaceMessage", "Race_1", "Catch_1"),
            edge("RaceTimer", "Race_1", "Timer_1"),
            edge("MessageEnd", "Catch_1", "End_1"),
            edge("TimerEnd", "Timer_1", "End_1"),
        ];
        model
    }
    pub fn parallel_races_reaching_terminate() -> ProcessModel {
        let mut model = race_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        model.messages.push(ProcessMessageDeclaration {
            message_id: "OtherMessage".into(),
            name: "OtherEvidence".into(),
        });
        model.nodes.extend([
            ProcessNode { id: "Split".into(), name: "Independent races".into(),
                kind: ProcessNodeKind::ParallelGateway,
                repeat: None, },
            ProcessNode { id: "OtherRace".into(), name: "Other first signal".into(),
                kind: ProcessNodeKind::EventBasedGateway,
                repeat: None, },
            ProcessNode { id: "OtherCatch".into(), name: "Other message".into(),
                kind: ProcessNodeKind::MessageCatch {
                    message_ref: "OtherMessage".into(),
                    correlation_expression: "'case-1'".into(),
                    output_mapping: BTreeMap::new(),
                },
                repeat: None, },
            ProcessNode { id: "OtherTimer".into(), name: "Other deadline".into(),
                kind: ProcessNodeKind::TimerCatch {
                    timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 120 },
                },
                repeat: None, },
        ]);
        model.sequence_flows = vec![
            edge("StartSplit", "Start_1", "Split"),
            edge("SplitFirst", "Split", "Race_1"),
            edge("SplitOther", "Split", "OtherRace"),
            edge("RaceMessage", "Race_1", "Catch_1"),
            edge("RaceTimer", "Race_1", "Timer_1"),
            edge("OtherMessageEdge", "OtherRace", "OtherCatch"),
            edge("OtherTimerEdge", "OtherRace", "OtherTimer"),
            edge("MessageEnd", "Catch_1", "End_1"),
            edge("TimerEnd", "Timer_1", "End_1"),
            edge("OtherCatchEnd", "OtherCatch", "End_1"),
            edge("OtherTimerEnd", "OtherTimer", "End_1"),
        ];
        model
    }
    pub fn published(f: &Fixture, model: &ProcessModel) -> ProcessVersion {
        publish_model(f, model)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::collections::BTreeMap;
    use crate::processes::runtime::{
        self,
        test_support::{publish_model, stamp, user_model, Fixture},
    };
    use tentaflow_protocol::processes::{
        ProcessEventRaceStatus as R, ProcessInstanceStatus as I, ProcessMessageStatus as M,
        ProcessSubscriptionStatus as S, ProcessTimerStatus as T,
    };

    #[test]
    fn unmatched_retries_preserve_public_receipt_and_cancel_fences_stale_internal_candidates() {
        let f = Fixture::new();
        assert!(f.directory.path().join("processes.db").is_file());
        let version = published(&f, &receiving_model(false, true));
        let instance = start_version(&f, &version);
        let message = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            Value::Null,
        );
        let admitted = send(&f, &message);
        let initial_candidate = repository::due_messages(&f.db, admitted.received_at_ms, 32)
            .unwrap()
            .remove(0);
        let facts = || {
            let conn = f.db.read().unwrap();
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM bpmn_events),(SELECT COUNT(*) FROM audit_log),(SELECT COUNT(*) FROM bpmn_commands)",
                [],
                |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?)),
            ).unwrap()
        };
        let unchanged = facts();
        for step in 0..4 {
            let at = admitted.received_at_ms + step * 1000;
            let drain = drain_pending(&f.db, at);
            drain.completion.unwrap();
            assert_eq!(drain.delivered, 0);
            assert_eq!(current(&f, &message).message, admitted);
            assert_eq!(facts(), unchanged);
            let next: i64 =
                f.db.read()
                    .unwrap()
                    .query_row(
                        "SELECT next_check_at_ms FROM bpmn_messages WHERE message_id=?1",
                        [&message.message_id],
                        |row| row.get(0),
                    )
                    .unwrap();
            assert_eq!(next, at + 1000);
            assert!(matches!(
                repository::message_snapshot(&f.db, &initial_candidate).unwrap(),
                MessageSelection::Stale
            ));
            assert!(!repository::record_message_waiting(&f.db, &initial_candidate, at).unwrap());
            assert_eq!(facts(), unchanged);
        }
        complete(
            &f,
            &instance.instance_id,
            &instance.user_tasks[0].user_task_id,
        );
        assert!(matches!(
            repository::message_snapshot(&f.db, &initial_candidate).unwrap(),
            MessageSelection::Stale
        ));
        let at = admitted.received_at_ms + 4000;
        let ready_candidate = repository::due_messages(&f.db, at, 32).unwrap().remove(0);
        let MessageSelection::Ready(prepared) =
            repository::message_snapshot(&f.db, &ready_candidate).unwrap()
        else {
            panic!("the real gate completion must arm the matching catch")
        };
        let delivery = plan_message_delivery(&prepared, at).unwrap();
        let cancel = stamp("cancel after internal unmatched retries");
        let before_denied = facts();
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&f.owner.user_id],
            )
            .unwrap();
        assert!(repository::cancel_message(
            &f.db,
            &f.owner,
            &cancel,
            &message.message_id,
            admitted.revision,
            at,
        )
        .is_err());
        assert_eq!(facts(), before_denied);
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                [&f.owner.user_id],
            )
            .unwrap();
        assert_eq!(current(&f, &message).message, admitted);
        let cancelled = repository::cancel_message(
            &f.db,
            &f.owner,
            &cancel,
            &message.message_id,
            admitted.revision,
            at,
        )
        .unwrap();
        assert_eq!(cancelled.status, M::Cancelled);
        assert_eq!(cancelled.revision, admitted.revision + 1);
        assert_eq!(cancelled.last_reason.as_deref(), Some("sender_cancelled"));
        assert!(!cancelled.can_cancel);
        assert!(repository::deliver_message(&f.db, &prepared, &delivery, at)
            .unwrap()
            .is_none());
        let committed = facts();
        assert_eq!(
            repository::cancel_message(
                &f.db,
                &f.owner,
                &cancel,
                &message.message_id,
                admitted.revision,
                at,
            )
            .unwrap(),
            cancelled
        );
        assert_eq!(facts(), committed);
        let waiting =
            repository::get_instance(&f.db, &f.owner, &instance.instance_id, None).unwrap();
        assert_eq!(waiting.status, I::Waiting);
        assert_eq!(waiting.subscriptions[0].status, S::Open);
        assert!(waiting.variables.get("received").is_none());
    }

    #[test]
    fn early_catch_survives_reopen_and_identity_tombstone_preserves_available_null() {
        let f = Fixture::new();
        let mut model = receiving_model(false, true);
        if let ProcessNodeKind::MessageCatch { output_mapping, .. } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Catch_1")
            .unwrap()
            .kind
        {
            output_mapping.extend([
                ("received_metadata".into(), "message".into()),
                ("received_key".into(), "message.correlation_key".into()),
            ]);
        }
        let version = published(&f, &model);
        let instance = start_version(&f, &version);
        let message = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            Value::Null,
        );
        let admitted = send(&f, &message);
        assert_eq!(admitted.status, M::Pending);
        assert!(drain_pending(&f.db, chrono::Utc::now().timestamp_millis())
            .completion
            .is_ok());
        assert!(current(&f, &message).message.payload_available);
        assert_eq!(current(&f, &message).payload, Some(Value::Null));
        complete(
            &f,
            &instance.instance_id,
            &instance.user_tasks[0].user_task_id,
        );
        let path = f.directory.path().join("processes.db");
        let owner = f.owner.clone();
        let Fixture {
            directory,
            db,
            router,
            ..
        } = f;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).unwrap();
        let at = chrono::Utc::now().timestamp_millis() + 1000;
        let drain = drain_pending(&reopened, at);
        drain.completion.unwrap();
        assert_eq!(drain.delivered, 1);
        let detail =
            repository::get_message(&reopened, &owner, &owner.user_id, &message.message_id)
                .unwrap();
        assert_eq!(detail.message.status, M::Delivered);
        assert_eq!(detail.payload, Some(Value::Null));
        let instance =
            repository::get_instance(&reopened, &owner, &instance.instance_id, None).unwrap();
        assert_eq!(instance.status, I::Completed);
        assert_eq!(instance.variables["received"], Value::Null);
        assert_eq!(instance.variables["received_key"], admitted.correlation_key);
        assert_eq!(
            instance.variables["received_metadata"],
            json!({
                "message_id": admitted.message_id,
                "sender_user_id": admitted.sender_user_id,
                "message_name": admitted.message_name,
                "correlation_key": admitted.correlation_key,
                "received_at_ms": admitted.received_at_ms,
                "expires_at_ms": admitted.expires_at_ms,
                "payload_sha256": admitted.payload_sha256,
            })
        );
        assert_eq!(
            repository::send_message(
                &reopened,
                &owner,
                &stamp("retry same envelope"),
                &message,
                at
            )
            .unwrap()
            .status,
            M::Delivered
        );
        assert_eq!(
            repository::prune_message_payloads(&reopened, at + 7 * 86400 * 1000, 32).unwrap(),
            1
        );
        let pruned =
            repository::get_message(&reopened, &owner, &owner.user_id, &message.message_id)
                .unwrap();
        assert!(!pruned.message.payload_available);
        assert_eq!(pruned.payload, None);
        assert_eq!(
            repository::send_message(
                &reopened,
                &owner,
                &stamp("retry pruned identity"),
                &message,
                at
            )
            .unwrap()
            .status,
            M::Delivered
        );
        let mut changed = message.clone();
        changed.payload = json!({"customer_ID":17});
        assert!(repository::send_message(
            &reopened,
            &owner,
            &stamp("conflicting identity"),
            &changed,
            at
        )
        .is_err());
        drop(reopened);
        drop(directory);
    }

    #[test]
    fn ambiguous_requires_sender_resolution_and_closed_chosen_activation_never_redirects() {
        let f = Fixture::new();
        let version = published(&f, &receiving_model(false, false));
        let first = start_version(&f, &version);
        let second = start_version(&f, &version);
        let message = envelope(catch_target(&version, None, None), json!({"customer_ID":7}));
        send(&f, &message);
        let at = chrono::Utc::now().timestamp_millis();
        drain_pending(&f.db, at).completion.unwrap();
        let ambiguous = current(&f, &message).message;
        assert_eq!(ambiguous.status, M::Ambiguous);
        assert!(repository::resolve_message(
            &f.db,
            &f.participant,
            &stamp("unauthorized resolution"),
            &message.message_id,
            ambiguous.revision,
            &first.instance_id,
            &first.subscriptions[0].subscription_id,
            at
        )
        .is_err());
        repository::resolve_message(
            &f.db,
            &f.owner,
            &stamp("choose exact activation"),
            &message.message_id,
            ambiguous.revision,
            &first.instance_id,
            &first.subscriptions[0].subscription_id,
            at,
        )
        .unwrap();
        repository::cancel_instance(
            &f.db,
            &f.owner,
            &stamp("close chosen target"),
            &first.instance_id,
            first.revision,
        )
        .unwrap()
        .instance;
        drain_pending(&f.db, at + 1).completion.unwrap();
        assert_eq!(current(&f, &message).message.status, M::Cancelled);
        assert_eq!(
            current(&f, &message).message.last_reason.as_deref(),
            Some("activation_closed")
        );
        assert_eq!(
            repository::get_instance(&f.db, &f.owner, &second.instance_id, None)
                .unwrap()
                .subscriptions[0]
                .status,
            S::Open
        );
        let next = envelope(
            catch_target(
                &version,
                Some(&second.instance_id),
                Some(&second.subscriptions[0].subscription_id),
            ),
            json!({"attached_to_id":"actual business key"}),
        );
        let admitted = send(&f, &next);
        let result = drain_pending(&f.db, admitted.received_at_ms);
        result.completion.unwrap();
        assert_eq!(result.delivered, 1);
        assert_eq!(current(&f, &next).message.status, M::Delivered);
        assert_eq!(
            current(&f, &next).message.matched_instance_id,
            Some(second.instance_id)
        );
    }

    #[test]
    fn explicit_instance_resolution_cannot_escape_its_immutable_address_or_mutate_receipts() {
        let f = Fixture::new();
        let mut model = boundary_messages(
            user_model(None),
            "Work",
            &[
                ("Receive_A", false, "EvidenceReady"),
                ("Receive_B", false, "OtherName"),
            ],
        );
        if let ProcessNodeKind::BoundaryMessage { message_ref, .. } = &mut model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Receive_B")
            .unwrap()
            .kind
        {
            *message_ref = "Declaration_Receive_A".into();
        }
        model
            .messages
            .retain(|declaration| declaration.name == "EvidenceReady");
        let version = published(&f, &model);
        let addressed = start_version(&f, &version);
        let other = start_version(&f, &version);
        let message = envelope(
            catch_target(&version, Some(&addressed.instance_id), None),
            json!({"customer_ID": 19}),
        );
        send(&f, &message);
        let at = chrono::Utc::now().timestamp_millis();
        drain_pending(&f.db, at).completion.unwrap();
        let before = current(&f, &message);
        assert_eq!(before.message.status, M::Ambiguous);
        let stored = || {
            let conn = f.db.read().unwrap();
            let row = conn.query_row(
                "SELECT revision,status,resolved_instance_id,resolved_subscription_id,resolved_token_id FROM bpmn_messages WHERE org_id=?1 AND sender_user_id=?2 AND message_id=?3",
                rusqlite::params![f.owner.org_id,f.owner.user_id,message.message_id],
                |row| Ok((row.get::<_, i64>(0)?,row.get::<_, String>(1)?,row.get::<_, Option<String>>(2)?,row.get::<_, Option<String>>(3)?,row.get::<_, Option<String>>(4)?)),
            ).unwrap();
            let counts = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM bpmn_events),(SELECT COUNT(*) FROM bpmn_commands),(SELECT COUNT(*) FROM audit_log)",
                [],
                |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?)),
            ).unwrap();
            (row, counts)
        };
        let unchanged = stored();
        for target_instance in [&other.instance_id, &addressed.instance_id] {
            assert!(repository::resolve_message(
                &f.db,
                &f.owner,
                &stamp("reject resolution outside immutable address"),
                &message.message_id,
                before.message.revision,
                target_instance,
                &other.subscriptions[0].subscription_id,
                at,
            )
            .is_err());
            assert_eq!(current(&f, &message), before);
            assert_eq!(stored(), unchanged);
        }
        let chosen = &addressed.subscriptions[0].subscription_id;
        repository::resolve_message(
            &f.db,
            &f.owner,
            &stamp("resolve within immutable instance"),
            &message.message_id,
            before.message.revision,
            &addressed.instance_id,
            chosen,
            at,
        )
        .unwrap();
        let result = drain_pending(&f.db, at + 1);
        result.completion.unwrap();
        assert_eq!(result.delivered, 1);
        let delivered = current(&f, &message).message;
        assert_eq!(delivered.status, M::Delivered);
        assert_eq!(delivered.target, message.target);
        assert_eq!(
            delivered.matched_instance_id.as_deref(),
            Some(addressed.instance_id.as_str())
        );
        assert_eq!(
            delivered.matched_subscription_id.as_deref(),
            Some(chosen.as_str())
        );
        assert!(
            repository::get_instance(&f.db, &f.owner, &other.instance_id, None)
                .unwrap()
                .subscriptions
                .iter()
                .all(|subscription| subscription.status == S::Open)
        );
    }

    #[tokio::test]
    async fn process_throw_rechecks_pinned_source_acl_at_enqueue_and_delivery_without_effect_replay(
    ) {
        use crate::processes::runtime::test_support::{edge, flow, graph, service_model};
        use tentaflow_protocol::processes::{
            ActivityVerification, ProcessMessageDeclaration, ProcessMessageTargetSpec,
        };

        let f = Fixture::new();
        let receiving_version = published(&f, &receiving_model(false, true));
        let receiver = start_version(&f, &receiving_version);
        let flow_id = flow(&f.db, &f.owner, &graph("factual source effect", None));
        let mut source_model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        source_model.messages.push(ProcessMessageDeclaration {
            message_id: "ThrowDecl".into(),
            name: "EvidenceReady".into(),
        });
        source_model.nodes.extend([
            ProcessNode {
                id: "Gate_1".into(),
                name: "Confirm source evidence".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: std::collections::BTreeMap::new(),
                },
                repeat: None,
            },
            ProcessNode {
                id: "Throw_1".into(),
                name: "Submit actual source evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "ThrowDecl".into(),
                    target: ProcessMessageTargetSpec::Catch {
                        definition_id: receiving_version.definition_id.clone(),
                        instance_id_expression: Some(
                            serde_json::to_string(&receiver.instance_id).unwrap(),
                        ),
                        subscription_id_expression: None,
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'customer_ID': 17}".into(),
                    ttl_seconds: 120,
                },
                repeat: None,
            },
        ]);
        source_model.sequence_flows = vec![
            edge("ToService", "Start_1", "Service"),
            edge("ServiceGate", "Service", "Gate_1"),
            edge("GateThrow", "Gate_1", "Throw_1"),
            edge("ThrowEnd", "Throw_1", "End_1"),
        ];
        let source_version = published(&f, &source_model);
        let source = start_version(&f, &source_version);
        let worker = "factual-message-source";
        let claim = repository::claim_job(&f.db, worker, chrono::Utc::now().timestamp_millis())
            .unwrap()
            .unwrap();
        crate::processes::jobs::execute_claimed(
            &f.db,
            f.dispatcher(),
            worker,
            claim,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let snapshot = repository::runtime_snapshot(&f.db, &f.owner, &source.instance_id).unwrap();
        assert_eq!(
            snapshot.instance.variables["answer"],
            "factual source effect"
        );
        let gate = snapshot
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Gate_1")
            .unwrap();
        let at = chrono::Utc::now().timestamp_millis();
        let denied_command = stamp("source revoked before enqueue");
        let plan =
            runtime::plan_user_completion(&snapshot, &gate.user_task_id, &json!({}), None, at,
                runtime::test_support::human_input(&snapshot, &gate.user_task_id, &denied_command))
                .unwrap();
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "deny",
        )
        .unwrap();
        assert!(repository::complete_user_task(
            &f.db,
            &f.owner,
            &denied_command,
            &source.instance_id,
            &gate.user_task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            &plan,
            at,
        )
        .unwrap_err()
        .downcast_ref::<repository::ProcessAuthorityDenied>()
        .is_some());
        let unchanged =
            repository::get_instance(&f.db, &f.owner, &source.instance_id, None).unwrap();
        assert_eq!(unchanged.revision, snapshot.instance.revision);
        assert_eq!(
            unchanged
                .user_tasks
                .iter()
                .find(|task| task.user_task_id == gate.user_task_id)
                .unwrap()
                .status,
            tentaflow_protocol::processes::ProcessUserTaskStatus::Open
        );
        assert!(unchanged.outgoing_messages.is_empty());
        assert!(
            !repository::list_events(&f.db, &f.owner, &source.instance_id, 0, 100)
                .unwrap()
                .0
                .iter()
                .any(|event| event.kind == "message_queued")
        );
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "allow",
        )
        .unwrap();
        let completed = repository::complete_user_task(
            &f.db,
            &f.owner,
            &denied_command,
            &source.instance_id,
            &gate.user_task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            &plan,
            at,
        )
        .unwrap()
        .instance;
        assert_eq!(completed.status, I::Completed);
        let message_id = completed.outgoing_messages[0].message_id.clone();
        assert_eq!(
            completed.outgoing_messages[0].origin,
            tentaflow_protocol::processes::ProcessMessageOrigin::Process
        );
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "deny",
        )
        .unwrap();
        let receiver_ready = complete(
            &f,
            &receiver.instance_id,
            &receiver.user_tasks[0].user_task_id,
        );
        let blocked = drain_pending(&f.db, at + 1);
        blocked.completion.unwrap();
        assert_eq!(blocked.delivered, 0);
        let receipt =
            repository::get_message(&f.db, &f.owner, &f.owner.user_id, &message_id).unwrap();
        assert_eq!(receipt.message.status, M::Blocked);
        assert_eq!(receipt.payload, Some(json!({"customer_ID": 17})));
        let source_events = repository::list_events(&f.db, &f.owner, &source.instance_id, 0, 100)
            .unwrap()
            .0;
        assert_eq!(
            source_events
                .iter()
                .filter(|event| event.kind == "message_blocked")
                .count(),
            1
        );
        drain_pending(&f.db, at + 59_000).completion.unwrap();
        assert_eq!(
            repository::get_message(&f.db, &f.owner, &f.owner.user_id, &message_id)
                .unwrap()
                .message
                .revision,
            receipt.message.revision
        );
        let waiting =
            repository::get_instance(&f.db, &f.owner, &receiver.instance_id, None).unwrap();
        assert_eq!(waiting.revision, receiver_ready.revision);
        assert_eq!(waiting.subscriptions[0].status, S::Open);
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
                .unwrap()
                .len(),
            1
        );
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "allow",
        )
        .unwrap();
        let candidate = repository::due_messages(&f.db, at + 60_002, 32)
            .unwrap()
            .remove(0);
        let MessageSelection::Ready(prepared) =
            repository::message_snapshot(&f.db, &candidate).unwrap()
        else {
            panic!("actual restored source message is ready");
        };
        let delivery_plan = plan_message_delivery(&prepared, at + 60_002).unwrap();
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "deny",
        )
        .unwrap();
        assert!(
            repository::deliver_message(&f.db, &prepared, &delivery_plan, at + 60_002)
                .unwrap_err()
                .downcast_ref::<repository::ProcessAuthorityDenied>()
                .is_some()
        );
        assert_eq!(
            repository::get_instance(&f.db, &f.owner, &receiver.instance_id, None)
                .unwrap()
                .revision,
            receiver_ready.revision
        );
        assert_eq!(
            repository::get_message(&f.db, &f.owner, &f.owner.user_id, &message_id)
                .unwrap()
                .message
                .revision,
            receipt.message.revision
        );
        crate::db::repository::resource_permissions::set(
            &f.db,
            "flow",
            &flow_id,
            "user",
            &f.owner.user_id,
            "allow",
        )
        .unwrap();
        let delivered = drain_pending(&f.db, at + 60_002);
        delivered.completion.unwrap();
        assert_eq!(delivered.delivered, 1);
        assert_eq!(
            repository::get_message(&f.db, &f.owner, &f.owner.user_id, &message_id)
                .unwrap()
                .message
                .status,
            M::Delivered
        );
        let received =
            repository::get_instance(&f.db, &f.owner, &receiver.instance_id, None).unwrap();
        assert_eq!(received.status, I::Completed);
        assert_eq!(received.variables["received"], json!({"customer_ID": 17}));
        assert_eq!(
            repository::get_instance(&f.db, &f.owner, &source.instance_id, None)
                .unwrap()
                .revision,
            completed.revision
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn admission_and_delivery_recheck_actual_owner_account_assignee_and_ttl() {
        let f = Fixture::new();
        let mut model = receiving_model(true, false);
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Work".into(),
                name: "Handle evidence".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(f.participant.user_id.clone()),
                    output_mapping: std::collections::BTreeMap::new(),
                },
                repeat: None,
            },
        );
        model.sequence_flows = vec![
            runtime::test_support::edge("ToWork", "Start_1", "Work"),
            runtime::test_support::edge("WorkEnd", "Work", "End_1"),
        ];
        let version = published(&f, &model);
        let mut message = envelope(
            ProcessMessageTarget::Start {
                definition_id: version.definition_id.clone(),
            },
            json!({"literal":true}),
        );
        message.ttl_seconds = 300;
        assert!(repository::send_message(
            &f.db,
            &f.participant,
            &stamp("nonowner start"),
            &message,
            chrono::Utc::now().timestamp_millis()
        )
        .is_err());
        let admitted = send(&f, &message);
        let at = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&f.db, at, 32).unwrap().remove(0);
        let MessageSelection::Ready(snapshot) =
            repository::message_snapshot(&f.db, &candidate).unwrap()
        else {
            panic!("actual start candidate")
        };
        let plan = plan_message_delivery(&snapshot, at).unwrap();
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&f.participant.user_id],
            )
            .unwrap();
        assert!(repository::deliver_message(&f.db, &snapshot, &plan, at)
            .unwrap_err()
            .downcast_ref::<repository::ProcessAuthorityDenied>()
            .is_some());
        drain_pending(&f.db, at).completion.unwrap();
        let blocked = current(&f, &message).message;
        assert_eq!(blocked.status, M::Blocked);
        assert_eq!(blocked.revision, admitted.revision + 1);
        assert!(repository::cancel_message(
            &f.db,
            &f.owner,
            &stamp("stale pending cancellation after authority change"),
            &message.message_id,
            admitted.revision,
            at,
        )
        .is_err());
        assert_eq!(current(&f, &message).message, blocked);
        assert_eq!(
            repository::list_instances(&f.db, &f.owner, None, 0, 20)
                .unwrap()
                .1,
            0
        );
        let history =
            f.db.read()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
        let retry = repository::due_messages(&f.db, at + 60_000, 32)
            .unwrap()
            .remove(0);
        let repeated = drain_pending(&f.db, at + 60_000);
        repeated.completion.unwrap();
        assert_eq!(repeated.delivered, 0);
        assert_eq!(current(&f, &message).message, blocked);
        assert_eq!(
            f.db.read()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            history
        );
        assert!(!repository::record_message_blocked(
            &f.db,
            &retry,
            blocked.last_reason.as_deref().unwrap(),
            at + 60_000,
        )
        .unwrap());
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                [&f.participant.user_id],
            )
            .unwrap();
        assert!(repository::due_messages(&f.db, at + 119_999, 32)
            .unwrap()
            .is_empty());
        let recovered = drain_pending(&f.db, at + 120_000);
        recovered.completion.unwrap();
        assert_eq!(recovered.delivered, 1);
        let delivered = current(&f, &message).message;
        assert_eq!(delivered.status, M::Delivered);
        assert_eq!(delivered.revision, blocked.revision + 1);
        for revision in [blocked.revision, delivered.revision] {
            assert!(repository::cancel_message(
                &f.db,
                &f.owner,
                &stamp("cancellation cannot replace actual delivery"),
                &message.message_id,
                revision,
                at + 120_000,
            )
            .is_err());
            assert_eq!(current(&f, &message).message, delivered);
        }
        let mut expires = envelope(
            ProcessMessageTarget::Start {
                definition_id: version.definition_id,
            },
            Value::Null,
        );
        expires.ttl_seconds = 1;
        let sent = send(&f, &expires);
        assert_eq!(
            repository::expire_messages(&f.db, sent.expires_at_ms, 32).unwrap(),
            1
        );
        let expired = current(&f, &expires).message;
        assert_eq!(expired.status, M::Expired);
        assert_eq!(expired.revision, sent.revision + 1);
        for revision in [sent.revision, expired.revision] {
            assert!(repository::cancel_message(
                &f.db,
                &f.owner,
                &stamp("cancellation cannot replace actual expiry"),
                &expires.message_id,
                revision,
                sent.expires_at_ms,
            )
            .is_err());
            assert_eq!(current(&f, &expires).message, expired);
        }
    }

    #[test]
    fn mapping_error_retains_subscription_and_new_message_resolves_only_its_linked_incident() {
        let f = Fixture::new();
        let mut model = receiving_model(false, false);
        if let ProcessNodeKind::MessageCatch { output_mapping, .. } = &mut model.nodes[1].kind {
            *output_mapping =
                std::collections::BTreeMap::from([("received".into(), "outputs.required".into())]);
        }
        let version = published(&f, &model);
        let instance = start_version(&f, &version);
        let bad = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            json!({}),
        );
        send(&f, &bad);
        let at = chrono::Utc::now().timestamp_millis();
        drain_pending(&f.db, at).completion.unwrap();
        assert_eq!(current(&f, &bad).message.status, M::Error);
        let snapshot =
            repository::runtime_snapshot(&f.db, &f.owner, &instance.instance_id).unwrap();
        assert_eq!(snapshot.subscriptions[0].status, S::Open);
        assert_eq!(snapshot.incidents.len(), 1);
        let mut unrelated = RuntimePlan::initial(snapshot.instance.variables.clone());
        unrelated.status = I::Incident;
        let id = Uuid::new_v4().to_string();
        unrelated
            .add_incidents
            .push(tentaflow_protocol::processes::ProcessIncident {
                scope_id: snapshot.instance.instance_id.clone(),
                incident_id: id.clone(),
                node_id: Some("Catch_1".into()),
                node_name: None,
                job_id: None,
                code: "UNRELATED".into(),
                message: "Independent retained evidence".into(),
                at_ms: at,
                can_retry: false,
            });
        repository::apply_transition(
            &f.db,
            &f.owner,
            &instance.instance_id,
            snapshot.instance.revision,
            &unrelated,
            at,
        )
        .unwrap()
        .instance;
        let good = envelope(
            catch_target(
                &version,
                Some(&instance.instance_id),
                Some(&snapshot.subscriptions[0].subscription_id),
            ),
            json!({"required":42}),
        );
        let admitted = send(&f, &good);
        let delivered = drain_pending(&f.db, admitted.received_at_ms);
        delivered.completion.unwrap();
        assert_eq!(delivered.delivered, 1);
        assert_eq!(current(&f, &good).message.status, M::Delivered);
        let final_state =
            repository::runtime_snapshot(&f.db, &f.owner, &instance.instance_id).unwrap();
        assert_eq!(final_state.instance.variables["received"], 42);
        assert_eq!(final_state.incidents.len(), 1);
        assert_eq!(final_state.incidents[0].incident_id, id);
        assert_eq!(final_state.instance.status, I::Incident);
        assert_eq!(final_state.subscriptions[0].status, S::Consumed);
    }

    #[test]
    fn message_and_timer_race_each_commit_order_has_one_winner_and_rejects_foreign_plan() {
        for message_first in [true, false] {
            let f = Fixture::new();
            let version = published(&f, &race_model());
            let instance = start_version(&f, &version);
            let other = start_version(&f, &version);
            let m = envelope(
                catch_target(&version, Some(&instance.instance_id), None),
                json!(7),
            );
            send(&f, &m);
            let at = chrono::Utc::now().timestamp_millis();
            let candidate = repository::due_messages(&f.db, at, 32).unwrap().remove(0);
            let MessageSelection::Ready(snapshot) =
                repository::message_snapshot(&f.db, &candidate).unwrap()
            else {
                panic!("ready race message")
            };
            let plan = plan_message_delivery(&snapshot, at).unwrap();
            let mut forged_facts = plan.clone();
            forged_facts
                .events
                .iter_mut()
                .find(|event| event.kind == "message_delivered")
                .unwrap()
                .data["payload"] = json!("forged envelope");
            assert!(repository::deliver_message(&f.db, &snapshot, &forged_facts, at).is_err());
            assert_eq!(current(&f, &m).message.status, M::Pending);
            let mut forged = plan.clone();
            forged.race_updates[0].race_id = other.event_races[0].race_id.clone();
            assert!(repository::deliver_message(&f.db, &snapshot, &forged, at).is_err());
            assert_eq!(
                repository::get_instance(&f.db, &f.owner, &instance.instance_id, None)
                    .unwrap()
                    .revision,
                instance.revision
            );
            if message_first {
                assert_eq!(drain_pending(&f.db, at).delivered, 1);
                crate::processes::timers::drain_due(&f.db, at + 3000)
                    .completion
                    .unwrap();
            } else {
                crate::processes::timers::drain_due(&f.db, at + 3000)
                    .completion
                    .unwrap();
                drain_pending(&f.db, at + 3000).completion.unwrap();
            }
            let state =
                repository::runtime_snapshot(&f.db, &f.owner, &instance.instance_id).unwrap();
            assert_eq!(state.event_races[0].status, R::Won);
            assert_eq!(state.instance.status, I::Completed);
            assert_eq!(
                state.subscriptions[0].status,
                if message_first {
                    S::Consumed
                } else {
                    S::Cancelled
                }
            );
            assert_eq!(
                state.timers[0].status,
                if message_first {
                    T::Cancelled
                } else {
                    T::Fired
                }
            );
            assert_eq!(
                state
                    .tokens
                    .iter()
                    .filter(|t| t.status == "waiting")
                    .count(),
                0
            );
        }
    }

    #[test]
    fn message_race_winner_reaching_terminate_settles_once_and_rejects_forged_closure() {
        let f = Fixture::new();
        let mut model = race_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        let version = published(&f, &model);
        let started = start_version(&f, &version);
        let other = start_version(&f, &version);
        let message = envelope(
            catch_target(&version, Some(&started.instance_id), None),
            json!({"winner": "message"}),
        );
        send(&f, &message);
        let at = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&f.db, at, 32).unwrap().into_iter()
            .find(|candidate| candidate.key.message_id == message.message_id).unwrap();
        let MessageSelection::Ready(snapshot) = repository::message_snapshot(&f.db, &candidate).unwrap() else {
            panic!("addressed message must have an open catch");
        };
        let plan = plan_message_delivery(&snapshot, at).unwrap();
        assert_eq!(plan.race_updates.len(), 1);
        assert_eq!(plan.race_updates[0].status, R::Won);
        assert_eq!(plan.timer_updates.iter().filter(|update| update.status == T::Cancelled).count(), 1);
        assert_eq!(plan.events.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(plan.events.iter().filter(|event| event.kind == "event_race_cancelled").count(), 0);
        let before = super::super::call_tests::transition_rows(&f);
        let mut duplicate = plan.clone();
        let mut extra = duplicate.race_updates[0].clone();
        extra.status = R::Cancelled;
        extra.winner_node_id = None;
        extra.winner_subscription_id = None;
        duplicate.race_updates.push(extra);
        assert!(repository::deliver_message(&f.db, &snapshot, &duplicate, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let mut wrong_winner = plan.clone();
        wrong_winner.race_updates[0].winner_node_id = Some("Timer_1".into());
        assert!(repository::deliver_message(&f.db, &snapshot, &wrong_winner, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let mut foreign = plan.clone();
        let other_tokens = repository::runtime_snapshot(&f.db, &f.owner, &other.instance_id).unwrap().tokens;
        foreign.cancel_token_ids.push(other_tokens[0].token_id.clone());
        assert!(repository::deliver_message(&f.db, &snapshot, &foreign, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let drained = drain_pending(&f.db, at);
        drained.completion.unwrap();
        assert_eq!(drained.delivered, 1);
        assert_eq!(current(&f, &message).message.status, M::Delivered);
        let state = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap();
        assert_eq!(state.instance.status, I::Completed);
        assert_eq!(state.event_races[0].status, R::Won);
        assert_eq!(state.timers[0].status, T::Cancelled);
        assert_eq!(state.subscriptions[0].status, S::Consumed);
        let events = repository::list_events(&f.db, &f.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "event_race_cancelled").count(), 0);
        assert_eq!(events.iter().filter(|event| event.kind == "timer_cancelled").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
        let reopened = crate::db::init(&f.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &f.owner, &started.instance_id).unwrap().event_races[0].status, R::Won);
        let replay = drain_pending(&reopened, at + 1);
        replay.completion.unwrap();
        assert_eq!(replay.delivered, 0);
    }

    #[test]
    fn message_race_termination_closes_unrelated_waiting_user_task() {
        let f = Fixture::new();
        let mut model = race_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        model.nodes.extend([
            ProcessNode { id: "Split".into(), name: "Concurrent paths".into(),
                kind: ProcessNodeKind::ParallelGateway,
                repeat: None, },
            ProcessNode { id: "SideWork".into(), name: "Independent open work".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None,
                    output_mapping: BTreeMap::new() },
                repeat: None, },
        ]);
        model.sequence_flows = vec![
            runtime::test_support::edge("StartSplit", "Start_1", "Split"),
            runtime::test_support::edge("SplitRace", "Split", "Race_1"),
            runtime::test_support::edge("SplitSide", "Split", "SideWork"),
            runtime::test_support::edge("RaceMessage", "Race_1", "Catch_1"),
            runtime::test_support::edge("RaceTimer", "Race_1", "Timer_1"),
            runtime::test_support::edge("MessageEnd", "Catch_1", "End_1"),
            runtime::test_support::edge("TimerEnd", "Timer_1", "End_1"),
            runtime::test_support::edge("SideEnd", "SideWork", "End_1"),
        ];
        let version = published(&f, &model);
        let started = start_version(&f, &version);
        assert_eq!(started.user_tasks.len(), 1);
        assert_eq!(started.user_tasks[0].status, tentaflow_protocol::processes::ProcessUserTaskStatus::Open);
        let message = envelope(catch_target(&version, Some(&started.instance_id), None), Value::Null);
        let sent = send(&f, &message);
        let candidate = repository::due_messages(&f.db, sent.received_at_ms, 32).unwrap().into_iter()
            .find(|candidate| candidate.key.message_id == message.message_id).unwrap();
        let MessageSelection::Ready(snapshot) = repository::message_snapshot(&f.db, &candidate).unwrap() else {
            panic!("the companion race message has a factual addressed catch");
        };
        let plan = plan_message_delivery(&snapshot, sent.received_at_ms).unwrap();
        assert!(repository::deliver_message(&f.db, &snapshot, &plan, sent.received_at_ms).unwrap().is_some());
        let after = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, I::Completed);
        assert_eq!(after.event_races[0].status, R::Won);
        assert_eq!(after.user_tasks[0].status, tentaflow_protocol::processes::ProcessUserTaskStatus::Cancelled);
        let reopened = crate::db::init(&f.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &f.owner, &started.instance_id).unwrap().user_tasks[0].status,
            tentaflow_protocol::processes::ProcessUserTaskStatus::Cancelled);
        let replay = drain_pending(&reopened, sent.received_at_ms + 1);
        replay.completion.unwrap();
        assert_eq!(replay.delivered, 0);
    }

    #[test]
    fn message_race_termination_closes_a_distinct_open_race_with_source_proof() {
        let f = Fixture::new();
        let version = published(&f, &parallel_races_reaching_terminate());
        let started = start_version(&f, &version);
        assert_eq!(started.event_races.len(), 2);
        let message = envelope(catch_target(&version, Some(&started.instance_id),
            started.subscriptions.iter().find(|sub| sub.node_id == "Catch_1")
                .map(|sub| sub.subscription_id.as_str())), Value::Null);
        send(&f, &message);
        let at = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&f.db, at, 32).unwrap().into_iter()
            .find(|candidate| candidate.key.message_id == message.message_id).unwrap();
        let MessageSelection::Ready(snapshot) = repository::message_snapshot(&f.db, &candidate).unwrap() else {
            panic!("first race message has a factual addressed catch");
        };
        let plan = plan_message_delivery(&snapshot, at).unwrap();
        assert_eq!(plan.race_updates.len(), 2);
        assert_eq!(plan.race_updates.iter().filter(|update| update.status == R::Won).count(), 1);
        assert_eq!(plan.race_updates.iter().filter(|update| update.status == R::Cancelled).count(), 1);
        let first = started.event_races.iter().find(|race| race.gateway_node_id == "Race_1").unwrap();
        let other = started.event_races.iter().find(|race| race.gateway_node_id == "OtherRace").unwrap();
        assert!(plan.race_updates.iter().any(|update| update.race_id == first.race_id && update.status == R::Won));
        assert!(plan.race_updates.iter().any(|update| update.race_id == other.race_id && update.status == R::Cancelled));
        let before = super::super::call_tests::transition_rows(&f);
        let loser = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap()
            .timers.into_iter().find(|timer| timer.node_id == "Timer_1").unwrap().token_id.unwrap();
        let mut missing_loser = plan.clone();
        missing_loser.cancel_token_ids.retain(|id| id != &loser);
        assert!(repository::deliver_message(&f.db, &snapshot, &missing_loser, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let mut duplicate_loser = plan.clone();
        duplicate_loser.cancel_token_ids.push(loser.clone());
        assert!(repository::deliver_message(&f.db, &snapshot, &duplicate_loser, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let mut wrong_source = plan.clone();
        wrong_source.events.iter_mut().find(|event| event.kind == "event_race_cancelled"
            && event.data["race_id"].as_str() == Some(other.race_id.as_str())).unwrap().data["source_event_id"] =
            json!(Uuid::new_v4().to_string());
        assert!(repository::deliver_message(&f.db, &snapshot, &wrong_source, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        let mut extra_winner = plan.clone();
        let other_update = extra_winner.race_updates.iter_mut().find(|update| update.race_id == other.race_id).unwrap();
        other_update.status = R::Won;
        other_update.winner_node_id = Some("OtherCatch".into());
        other_update.winner_subscription_id = started.subscriptions.iter().find(|sub| sub.node_id == "OtherCatch")
            .map(|sub| sub.subscription_id.clone());
        assert!(repository::deliver_message(&f.db, &snapshot, &extra_winner, at).is_err());
        assert_eq!(super::super::call_tests::transition_rows(&f), before);
        assert!(repository::deliver_message(&f.db, &snapshot, &plan, at).unwrap().is_some());
        let after = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, I::Completed);
        assert_eq!(after.event_races.iter().find(|race| race.race_id == first.race_id).unwrap().status, R::Won);
        assert_eq!(after.event_races.iter().find(|race| race.race_id == other.race_id).unwrap().status, R::Cancelled);
        let reopened = crate::db::init(&f.directory.path().join("processes.db")).unwrap();
        let history = repository::list_events(&reopened, &f.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(history.iter().filter(|event| event.kind == "event_race_won").count(), 1);
        assert_eq!(history.iter().filter(|event| event.kind == "event_race_cancelled").count(), 1);
        let replay = drain_pending(&reopened, at + 1);
        replay.completion.unwrap();
        assert_eq!(replay.delivered, 0);
    }

    #[test]
    fn boundary_message_siblings_preserve_prior_side_work_and_disarm_on_real_completion() {
        let f = Fixture::new();
        let mut base = user_model(None);
        if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut base.nodes[1].kind {
            output_mapping.clear();
        }
        let model = boundary_messages(
            base,
            "Work",
            &[
                ("Notify", false, "Notice"),
                ("Interrupt", true, "StopWork"),
                ("Later", true, "LaterNotice"),
            ],
        );
        let version = published(&f, &model);
        let instance = start_version(&f, &version);
        let mut first = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            Value::Null,
        );
        first.message_name = "Notice".into();
        send(&f, &first);
        let at = chrono::Utc::now().timestamp_millis();
        drain_pending(&f.db, at).completion.unwrap();
        let state = repository::get_instance(&f.db, &f.owner, &instance.instance_id, None).unwrap();
        let independent = state
            .user_tasks
            .iter()
            .find(|t| t.node_id == "Side_Notify")
            .unwrap()
            .user_task_id
            .clone();
        assert!(state
            .user_tasks
            .iter()
            .any(|t| t.node_id == "Work" && t.can_complete));
        let mut stop = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            Value::Null,
        );
        stop.message_name = "StopWork".into();
        let admitted = send(&f, &stop);
        let delivered = drain_pending(&f.db, admitted.received_at_ms);
        delivered.completion.unwrap();
        assert_eq!(delivered.delivered, 1);
        assert_eq!(current(&f, &stop).message.status, M::Delivered);
        let state = repository::get_instance(&f.db, &f.owner, &instance.instance_id, None).unwrap();
        assert!(state
            .user_tasks
            .iter()
            .any(|t| t.user_task_id == independent && t.can_complete));
        assert!(state.user_tasks.iter().any(|t| t.node_id == "Work"
            && t.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Cancelled));
        assert_eq!(
            state
                .subscriptions
                .iter()
                .find(|s| s.node_id == "Later")
                .unwrap()
                .last_reason
                .as_deref(),
            Some("sibling_interrupted")
        );
        let completed_version = published(&f, &model);
        let completed = start_version(&f, &completed_version);
        complete(
            &f,
            &completed.instance_id,
            &completed.user_tasks[0].user_task_id,
        );
        let after =
            repository::get_instance(&f.db, &f.owner, &completed.instance_id, None).unwrap();
        assert!(after
            .subscriptions
            .iter()
            .all(|s| s.status == S::Cancelled
                && s.last_reason.as_deref() == Some("activity_completed")));
    }

    #[test]
    fn throw_commits_once_and_completed_source_receipt_and_history_keep_current_target_visibility()
    {
        let f = Fixture::new();
        let receiver = published(&f, &receiving_model(true, false));
        let mut model = crate::processes::model::starter_model();
        model
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "ThrowDecl".into(),
                name: "EvidenceReady".into(),
            });
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Throw_1".into(),
                name: "Submit evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "ThrowDecl".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: receiver.definition_id.clone(),
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'customer_ID': 23}".into(),
                    ttl_seconds: 120,
                },
                repeat: None,
            },
        );
        model.sequence_flows = vec![
            runtime::test_support::edge("ToThrow", "Start_1", "Throw_1"),
            runtime::test_support::edge("ThrowEnd", "Throw_1", "End_1"),
        ];
        let source_version = publish_model(&f, &model);
        let source = start_version(&f, &source_version);
        assert_eq!(source.status, I::Completed);
        assert_eq!(source.outgoing_messages.len(), 1);
        assert_eq!(source.outgoing_messages[0].status, M::Pending);
        let revision = source.revision;
        let at = chrono::Utc::now().timestamp_millis();
        let drain = drain_pending(&f.db, at);
        drain.completion.unwrap();
        assert_eq!(drain.delivered, 1);
        let state = repository::get_instance(&f.db, &f.owner, &source.instance_id, None).unwrap();
        assert_eq!(state.revision, revision);
        assert_eq!(state.status, I::Completed);
        assert_eq!(state.outgoing_messages[0].status, M::Delivered);
        assert!(state.outgoing_messages[0].revision > source.outgoing_messages[0].revision);
        let events = repository::list_events(&f.db, &f.owner, &source.instance_id, 0, 100)
            .unwrap()
            .0;
        assert_eq!(
            events.iter().filter(|e| e.kind == "message_queued").count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "message_delivered")
                .count(),
            1
        );
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE bpmn_definitions SET owner_user_id=?1 WHERE definition_id=?2",
                rusqlite::params![f.participant.user_id, receiver.definition_id],
            )
            .unwrap();
        let hidden = repository::get_instance(&f.db, &f.owner, &source.instance_id, None).unwrap();
        assert!(hidden.outgoing_messages.is_empty());
        assert_eq!(hidden.pages.unwrap().outgoing_messages.total, 0);
    }
    #[test]
    fn latest_start_publication_archive_and_old_catch_version_have_distinct_delivery_semantics() {
        let f = Fixture::new();
        let version = published(&f, &receiving_model(true, false));
        let m = envelope(
            ProcessMessageTarget::Start {
                definition_id: version.definition_id.clone(),
            },
            Value::Null,
        );
        send(&f, &m);
        let at = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&f.db, at, 32).unwrap().remove(0);
        let MessageSelection::Ready(prepared) =
            repository::message_snapshot(&f.db, &candidate).unwrap()
        else {
            panic!("ready start")
        };
        let plan = plan_message_delivery(&prepared, at).unwrap();
        let (draft, _, _) =
            repository::get_definition(&f.db, &f.owner, &version.definition_id).unwrap();
        let mut changed = draft.model.clone();
        changed.messages[0].name = "NewEvidence".into();
        let saved = repository::save_definition(
            &f.db,
            &f.owner,
            &stamp("change start declaration"),
            Some(&draft.definition_id),
            draft.draft_revision,
            &draft.name,
            &draft.description,
            &changed,
        )
        .unwrap();
        repository::publish_definition(
            &f.db,
            &f.owner,
            &stamp("publish changed start"),
            &saved.definition_id,
            saved.draft_revision,
            &[],
            None,
        )
        .unwrap();
        assert!(repository::deliver_message(&f.db, &prepared, &plan, at).is_err());
        drain_pending(&f.db, at).completion.unwrap();
        assert_eq!(current(&f, &m).message.status, M::Error);
        assert_eq!(
            repository::list_instances(&f.db, &f.owner, None, 0, 20)
                .unwrap()
                .1,
            0
        );
        let mut pending = envelope(
            ProcessMessageTarget::Start {
                definition_id: version.definition_id.clone(),
            },
            Value::Null,
        );
        pending.message_name = "NewEvidence".into();
        send(&f, &pending);
        repository::archive_definition(
            &f.db,
            &f.owner,
            &stamp("archive message start"),
            &version.definition_id,
            repository::get_definition(&f.db, &f.owner, &version.definition_id)
                .unwrap()
                .0
                .draft_revision,
            true,
        )
        .unwrap();
        assert_eq!(current(&f, &pending).message.status, M::Cancelled);
        repository::archive_definition(
            &f.db,
            &f.owner,
            &stamp("restore message start"),
            &version.definition_id,
            repository::get_definition(&f.db, &f.owner, &version.definition_id)
                .unwrap()
                .0
                .draft_revision,
            false,
        )
        .unwrap();
        let restored = drain_pending(&f.db, chrono::Utc::now().timestamp_millis());
        restored.completion.unwrap();
        assert_eq!(restored.delivered, 0);
        let old = published(&f, &receiving_model(false, false));
        let waiting = start_version(&f, &old);
        repository::archive_definition(
            &f.db,
            &f.owner,
            &stamp("archive waiting catch"),
            &old.definition_id,
            repository::get_definition(&f.db, &f.owner, &old.definition_id)
                .unwrap()
                .0
                .draft_revision,
            true,
        )
        .unwrap();
        let catch = envelope(
            catch_target(&old, Some(&waiting.instance_id), None),
            json!("pinned V1"),
        );
        let admitted = send(&f, &catch);
        let outcome = drain_pending(&f.db, admitted.received_at_ms);
        outcome.completion.unwrap();
        assert_eq!(outcome.delivered, 1);
        assert_eq!(current(&f, &catch).message.status, M::Delivered);
        assert_eq!(
            repository::get_instance(&f.db, &f.owner, &waiting.instance_id, None)
                .unwrap()
                .version,
            1
        );
    }

    #[test]
    fn exact_participant_ingress_and_full_payload_do_not_grant_private_definition_or_foreign_instance(
    ) {
        let f = Fixture::new();
        let mut model = receiving_model(false, true);
        if let ProcessNodeKind::UserTask {
            assignee_user_id, ..
        } = &mut model.nodes[1].kind
        {
            *assignee_user_id = Some(f.participant.user_id.clone());
        }
        let version = published(&f, &model);
        let instance = start_version(&f, &version);
        assert!(repository::get_definition(&f.db, &f.participant, &version.definition_id).is_err());
        let message = envelope(
            catch_target(&version, Some(&instance.instance_id), None),
            json!({"customer_ID":23,"attached_to_id":"opaque"}),
        );
        let at = chrono::Utc::now().timestamp_millis();
        let sent = repository::send_message(
            &f.db,
            &f.participant,
            &stamp("participant exact send"),
            &message,
            at,
        )
        .unwrap();
        assert_eq!(sent.sender_user_id, f.participant.user_id);
        let mut wide = message.clone();
        wide.message_id = Uuid::new_v4().to_string();
        wide.target = catch_target(&version, None, None);
        assert!(repository::send_message(
            &f.db,
            &f.participant,
            &stamp("participant wide denied"),
            &wide,
            at
        )
        .is_err());
        let foreign = published(&f, &receiving_model(false, false));
        let foreign_instance = start_version(&f, &foreign);
        let mut guessed = message.clone();
        guessed.message_id = Uuid::new_v4().to_string();
        guessed.target = catch_target(&foreign, Some(&foreign_instance.instance_id), None);
        assert!(repository::send_message(
            &f.db,
            &f.participant,
            &stamp("foreign instance denied"),
            &guessed,
            at
        )
        .is_err());
        assert!(repository::get_message(
            &f.db,
            &f.participant,
            &f.owner.user_id,
            &Uuid::new_v4().to_string()
        )
        .is_err());
        let snapshot =
            repository::runtime_snapshot(&f.db, &f.participant, &instance.instance_id).unwrap();
        let command = stamp("participant completes real gate");
        let plan = runtime::plan_user_completion(
            &snapshot,
            &instance.user_tasks[0].user_task_id,
            &json!({}),
            None,
            at,
            runtime::test_support::human_input(&snapshot, &instance.user_tasks[0].user_task_id, &command),
        )
        .unwrap();
        repository::complete_user_task(
            &f.db,
            &f.participant,
            &command,
            &instance.instance_id,
            &instance.user_tasks[0].user_task_id,
            snapshot.instance.revision,
            &json!({}),
            None,
            &plan,
            at,
        )
        .unwrap()
        .instance;
        drain_pending(&f.db, at + 1).completion.unwrap();
        let payload = repository::get_message(
            &f.db,
            &f.participant,
            &f.participant.user_id,
            &message.message_id,
        )
        .unwrap()
        .payload
        .unwrap();
        assert_eq!(payload["customer_ID"], 23);
        assert_eq!(payload["attached_to_id"], "opaque");
        f.db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&f.participant.user_id],
            )
            .unwrap();
        assert!(repository::get_message(
            &f.db,
            &f.participant,
            &f.participant.user_id,
            &message.message_id
        )
        .is_err());
        assert!(repository::list_messages(&f.db, &f.participant, None, None, 0, 20).is_err());
    }

    #[test]
    fn catch_race_continuation_preserves_independent_gateway_receipts() {
        for inclusive in [false, true] {
        let f = Fixture::new();
        let mut model = race_model();
        model.nodes.extend([
            ProcessNode {
                id: "Split_1".into(),
                name: "Selected start".into(),
                kind: if inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway },
                repeat: None,
            },
            ProcessNode {
                id: "Merge_1".into(),
                name: "Exclusive merge".into(),
                kind: ProcessNodeKind::ExclusiveGateway {
                    default_flow_id: None,
                },
                repeat: None,
            },
            ProcessNode {
                id: "Other_1".into(),
                name: "Independent work".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: std::collections::BTreeMap::new(),
                },
                repeat: None,
            },
            ProcessNode {
                id: "Join_1".into(),
                name: "Join work".into(),
                kind: if inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway },
                repeat: None,
            },
        ]);
        model.sequence_flows = vec![
            runtime::test_support::edge("ToSplit", "Start_1", "Split_1"),
            runtime::test_support::edge("SplitRace", "Split_1", "Race_1"),
            runtime::test_support::edge("SplitOther", "Split_1", "Other_1"),
            runtime::test_support::edge("RaceMessage", "Race_1", "Catch_1"),
            runtime::test_support::edge("RaceTimer", "Race_1", "Timer_1"),
            runtime::test_support::edge("MessageMerge", "Catch_1", "Merge_1"),
            runtime::test_support::edge("TimerMerge", "Timer_1", "Merge_1"),
            runtime::test_support::edge("MergeJoin", "Merge_1", "Join_1"),
            runtime::test_support::edge("OtherJoin", "Other_1", "Join_1"),
            runtime::test_support::edge("JoinEnd", "Join_1", "End_1"),
        ];
        if inclusive {
            model.sequence_flows[1].condition = Some("true".into());
            model.sequence_flows[2].condition = Some("true".into());
        }
        let version = published(&f, &model);
        let started = start_version(&f, &version);
        let other = started
            .user_tasks
            .iter()
            .find(|t| t.node_id == "Other_1")
            .unwrap();
        complete(&f, &started.instance_id, &other.user_task_id);
        let before = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap();
        assert_eq!(before.receipts.len(), 1);
        let m = envelope(
            catch_target(&version, Some(&started.instance_id), None),
            Value::Null,
        );
        send(&f, &m);
        let drained = drain_pending(&f.db, chrono::Utc::now().timestamp_millis());
        drained.completion.unwrap();
        assert_eq!(drained.delivered, 1);
        let after = repository::runtime_snapshot(&f.db, &f.owner, &started.instance_id).unwrap();
        assert_eq!(after.instance.status, I::Completed);
        assert!(after.receipts.is_empty());
        assert_eq!(after.event_races[0].status, R::Won);
        let events = repository::list_events(&f.db, &f.owner, &started.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == if inclusive { "inclusive_joined" } else { "parallel_joined" }).count(), 1);
        }
    }

    #[test]
    fn capacity_rejection_rolls_back_throw_and_forged_race_subscription_is_not_published() {
        let f = Fixture::new();
        let receiver = published(&f, &receiving_model(false, false));
        let at = chrono::Utc::now().timestamp_millis();
        for index in 0..1024 {
            let m = envelope(catch_target(&receiver, None, None), json!(index));
            repository::send_message(&f.db, &f.owner, &stamp("fill admitted queue"), &m, at)
                .unwrap();
        }
        let over = envelope(catch_target(&receiver, None, None), Value::Null);
        assert!(
            repository::send_message(&f.db, &f.owner, &stamp("over capacity"), &over, at).is_err()
        );
        let mut throw = crate::processes::model::starter_model();
        throw
            .messages
            .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
                message_id: "ThrowDecl".into(),
                name: "EvidenceReady".into(),
            });
        throw.nodes.insert(
            1,
            ProcessNode {
                id: "Throw".into(),
                name: "Commit outbox".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "ThrowDecl".into(),
                    target: ProcessMessageTargetSpec::Catch {
                        definition_id: receiver.definition_id.clone(),
                        instance_id_expression: None,
                        subscription_id_expression: None,
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "null".into(),
                    ttl_seconds: 120,
                },
                repeat: None,
            },
        );
        throw.sequence_flows = vec![
            runtime::test_support::edge("ToThrow", "Start_1", "Throw"),
            runtime::test_support::edge("ThrowEnd", "Throw", "End_1"),
        ];
        let version = published(&f, &throw);
        let id = Uuid::new_v4().to_string();
        let command = stamp("atomic source capacity fail");
        let plan = runtime::plan_start(
            &version.model,
            &id,
            &f.owner,
            &version.definition_id,
            1,
            json!({}),
            runtime::StartCause::Manual,
            at,
            runtime::test_support::manual_input(&command),
        )
        .unwrap();
        assert!(repository::start_instance(
            &f.db,
            &f.owner,
            &command,
            &id,
            &version.definition_id,
            1,
            &json!({}),
            &plan,
            at
        )
        .is_err());
        assert!(repository::get_instance(&f.db, &f.owner, &id, None).is_err());
        let race = published(&f, &race_model());
        let id = Uuid::new_v4().to_string();
        let command = stamp("missing race branch denied");
        let mut forged = runtime::plan_start(
            &race.model,
            &id,
            &f.owner,
            &race.definition_id,
            1,
            json!({}),
            runtime::StartCause::Manual,
            at,
            runtime::test_support::manual_input(&command),
        )
        .unwrap();
        forged.create_subscriptions.clear();
        assert!(repository::start_instance(
            &f.db,
            &f.owner,
            &command,
            &id,
            &race.definition_id,
            1,
            &json!({}),
            &forged,
            at
        )
        .is_err());
        assert!(repository::get_instance(&f.db, &f.owner, &id, None).is_err());
    }
}
