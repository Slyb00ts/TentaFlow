// ============ File: signal_proof_tests.rs — Durable signal source and receipt provenance controls ============

use super::call_tests::transition_rows;
use super::messages::test_support::start_version;
use super::repository::{self, VariableEffect};
use super::runtime::{self, test_support::*};
use serde_json::json;
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ProcessInstanceStatus, ProcessMessageDeclaration, ProcessMessageTargetSpec, ProcessNode,
    ProcessNodeKind, ProcessSubscriptionStatus,
};
use uuid::Uuid;

pub(super) fn all_transition_rows(fixture: &Fixture) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let mut rows = transition_rows(fixture);
    let conn = fixture.db.read().unwrap();
    for table in ["bpmn_signal_emissions", "bpmn_signal_receipts"] {
        rows.push(super::call_pin_tests::table_rows(&conn, table, "rowid"));
    }
    rows
}

#[test]
fn signal_race_requires_exact_fenced_receipt_winner_and_loser_closure() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture,
        &super::signal_tests::signal_timer_race_model());
    let recipient = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &recipient.instance_id).unwrap();
    let catch = snapshot.subscriptions.iter().find(|row|
        row.node_id == "Catch_1").unwrap();
    let timer = snapshot.timers.iter().find(|row|
        row.node_id == "Timer_1").unwrap();
    let emitter = publish_model(&fixture,
        &super::signal_tests::signal_throw_model());
    start_version(&fixture, &emitter);
    let at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let receipt_id = repository::due_signal_receipts(&fixture.db, at_ms).unwrap()
        .into_iter().next().unwrap();
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
        .unwrap().unwrap();
    let input = repository::AcceptedInputRef::Signal {
        signal_id: claim.signal_id.clone(),
        receipt_id: claim.receipt_id.clone(),
        expected_receipt_revision: claim.revision,
        claim_fence: claim.fence.clone(),
        target_subscription_id: claim.subscription.subscription_id.clone(),
        expected_subscription_revision: claim.subscription.revision,
    };
    let plan = runtime::plan_signal_catch(&claim.snapshot, &claim.subscription,
        &claim.payload, &claim.signal_id, &claim.source_event_id,
        at_ms, input, None).unwrap();
    assert_eq!(plan.race_updates.len(), 1);
    assert_eq!(plan.race_updates[0].winner_subscription_id.as_deref(),
        Some(catch.subscription_id.as_str()));
    assert!(plan.timer_updates.iter().any(|update| update.timer_id == timer.timer_id));
    let before = all_transition_rows(&fixture);
    for (case, forged) in [
        ("foreign race", {
            let mut plan = plan.clone();
            plan.race_updates[0].race_id = Uuid::new_v4().to_string();
            plan
        }),
        ("foreign winner subscription", {
            let mut plan = plan.clone();
            plan.race_updates[0].winner_subscription_id =
                Some(Uuid::new_v4().to_string());
            plan
        }),
        ("omitted timer loser", {
            let mut plan = plan.clone();
            plan.timer_updates.retain(|update| update.timer_id != timer.timer_id);
            plan
        }),
        ("omitted loser token", {
            let mut plan = plan.clone();
            plan.cancel_token_ids.retain(|id|
                Some(id.as_str()) != timer.token_id.as_deref());
            plan
        }),
        ("forged admitted source event", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "signal_received")
                .unwrap().data["source_event_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("forged receipt subscription", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "signal_received")
                .unwrap().data["subscription_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("extra winner continuation", {
            let mut plan = plan.clone();
            let mut extra = plan.create_tokens.iter().find(|token|
                token.node_id == "End_1").unwrap().clone();
            extra.token_id = Uuid::new_v4().to_string();
            plan.create_tokens.push(extra);
            plan
        }),
    ] {
        assert!(repository::deliver_signal_receipt(&fixture.db, &claim,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err(), "{case}");
        assert_eq!(all_transition_rows(&fixture), before, "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::deliver_signal_receipt(&reopened, &claim,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    let committed = all_transition_rows(&fixture);
    assert!(repository::deliver_signal_receipt(&reopened, &claim,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

fn mixed_message_signal_model(
    message_definition_id: &str,
    message_first: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::signal_tests::signal_throw_model();
    model.messages.push(ProcessMessageDeclaration {
        message_id: "Evidence".into(),
        name: "EvidenceReady".into(),
    });
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Send_1".into(),
            name: "Admit evidence".into(),
            kind: ProcessNodeKind::SendTask {
                message_ref: "Evidence".into(),
                target: ProcessMessageTargetSpec::Catch {
                    definition_id: message_definition_id.into(),
                    instance_id_expression: None,
                    subscription_id_expression: None,
                },
                correlation_expression: "'case-1'".into(),
                payload_expression: "{'value': 42}".into(),
                ttl_seconds: 120,
            },
            repeat: None,
        },
    );
    model.sequence_flows = if message_first {
        vec![
            edge("StartSend", "Start_1", "Send_1"),
            edge("SendThrow", "Send_1", "Throw_1"),
            edge("ThrowEnd", "Throw_1", "End_1"),
        ]
    } else {
        vec![
            edge("StartThrow", "Start_1", "Throw_1"),
            edge("ThrowSend", "Throw_1", "Send_1"),
            edge("SendEnd", "Send_1", "End_1"),
        ]
    };
    model
}

#[test]
fn same_plan_message_then_signal_commits_only_the_prior_admission_at_the_shared_limit() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let message_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &signal_target);
    let source = publish_model(
        &fixture,
        &mixed_message_signal_model(&message_target.definition_id, true),
    );
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill shared pending slots"),
            &envelope(
                catch_target(&message_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("admit message before denied Signal");
    let variables = serde_json::to_value(&source.model.variables).unwrap();
    let unbounded = runtime::plan_start(
        &source.model,
        &instance_id,
        &fixture.owner,
        &source.definition_id,
        source.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    assert_eq!(
        (
            unbounded.create_messages.len(),
            unbounded.create_signals.len()
        ),
        (1, 1)
    );
    let before = all_transition_rows(&fixture);
    let forged = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&unbounded),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "unbounded supplied admission changed durable rows: {forged:#}"
    );
    let deny = |_: &repository::RuntimePlan,
                _: &repository::PlannedSignal,
                _: Option<&repository::AcceptedInputRef>| {
        Ok(runtime::SignalAdmissionDecision::DenyPending)
    };
    let denied_plan = runtime::plan_start(
        &source.model,
        &instance_id,
        &fixture.owner,
        &source.definition_id,
        source.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        Some(&deny),
    )
    .unwrap();
    let incident_index = denied_plan
        .events
        .iter()
        .position(|event| event.kind == "incident" && event.node_id.as_deref() == Some("Throw_1"))
        .unwrap();
    let source_id = denied_plan.event_sources[&incident_index].clone();
    for mutation in 0..3 {
        let mut changed = denied_plan.clone();
        match mutation {
            0 => {
                changed
                    .event_sources
                    .insert(incident_index, Uuid::new_v4().to_string());
            }
            1 => {
                changed.consume_token_ids.retain(|id| id != &source_id);
            }
            _ => {
                changed
                    .create_tokens
                    .iter_mut()
                    .find(|token| token.node_id == "Throw_1" && token.status == "waiting")
                    .unwrap()
                    .status = "ready".into();
            }
        }
        let error = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &instance_id,
            &source.definition_id,
            source.version,
            &variables,
            repository::ProcessPlanInput::Supplied(&changed),
            at_ms,
        )
        .unwrap_err();
        assert_eq!(
            all_transition_rows(&fixture),
            before,
            "forged finite Signal denial {mutation} changed durable rows: {error:#}"
        );
    }
    let actual = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    let conn = fixture.db.read().unwrap();
    let (messages, signals): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1),
            (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1)",
            [&instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((messages, signals), (1, 0));
    drop(conn);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = all_transition_rows(&fixture);
    let replay = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(
        replay.incidents[0].incident_id,
        actual.incidents[0].incident_id
    );
    assert_eq!(all_transition_rows(&fixture), committed);
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
}

#[test]
fn same_plan_signal_then_message_rolls_back_the_tentative_signal_at_the_shared_limit() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let message_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    start_version(&fixture, &signal_target);
    let source = publish_model(
        &fixture,
        &mixed_message_signal_model(&message_target.definition_id, false),
    );
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill shared pending slots"),
            &envelope(
                catch_target(&message_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("reject directed message after tentative Signal");
    let variables = serde_json::to_value(&source.model.variables).unwrap();
    let before = all_transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("pending message capacity exceeded"));
    assert_eq!(all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let retry = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &source.definition_id,
        source.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap_err();
    assert!(format!("{retry:#}").contains("pending message capacity exceeded"));
    assert_eq!(all_transition_rows(&fixture), before);
}

#[test]
fn both_same_plan_admission_orders_commit_exact_sources_below_the_shared_limit() {
    use super::messages::test_support::receiving_model;
    for message_first in [true, false] {
        let fixture = Fixture::new();
        let message_target = publish_model(&fixture, &receiving_model(false, false));
        let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
        start_version(&fixture, &signal_target);
        let source = publish_model(
            &fixture,
            &mixed_message_signal_model(&message_target.definition_id, message_first),
        );
        let instance_id = Uuid::new_v4().to_string();
        let command = stamp("admit both source-time emissions");
        let variables = serde_json::to_value(&source.model.variables).unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_start(
            &source.model,
            &instance_id,
            &fixture.owner,
            &source.definition_id,
            source.version,
            variables.clone(),
            runtime::StartCause::Manual,
            at_ms,
            manual_input(&command),
            None,
        )
        .unwrap();
        let mut forged = plan.clone();
        assert_ne!(
            forged.create_messages[0].source_event_index,
            forged.create_signals[0].source_event_index
        );
        forged.create_signals[0].source_event_index = forged.create_messages[0].source_event_index;
        let before = all_transition_rows(&fixture);
        let error = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &instance_id,
            &source.definition_id,
            source.version,
            &variables,
            repository::ProcessPlanInput::Supplied(&forged),
            at_ms,
        )
        .unwrap_err();
        assert_eq!(
            all_transition_rows(&fixture),
            before,
            "forged admission order changed durable rows: {error:#}"
        );
        let actual = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &instance_id,
            &source.definition_id,
            source.version,
            &variables,
            repository::ProcessPlanInput::Canonical,
            at_ms,
        )
        .unwrap();
        assert_eq!(actual.status, ProcessInstanceStatus::Completed);
        let events = repository::list_events(&fixture.db, &fixture.owner, &instance_id, 0, 200)
            .unwrap()
            .0;
        let message_index = events
            .iter()
            .position(|event| event.kind == "send_task_admitted")
            .unwrap();
        let signal_index = events
            .iter()
            .position(|event| event.kind == "signal_admitted")
            .unwrap();
        assert_eq!(message_index < signal_index, message_first);
        let conn = fixture.db.read().unwrap();
        let (messages, signals, receipts): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1),
                (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),
                (SELECT COUNT(*) FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
                    ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1)",
                [&instance_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((messages, signals, receipts), (1, 1, 1));
        drop(conn);
        let committed = all_transition_rows(&fixture);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        repository::start_instance(
            &reopened,
            &fixture.owner,
            &command,
            &instance_id,
            &source.definition_id,
            source.version,
            &variables,
            repository::ProcessPlanInput::Canonical,
            at_ms,
        )
        .unwrap();
        assert_eq!(all_transition_rows(&fixture), committed);
    }
}

fn parent_arm_then_call_model(
    child: &tentaflow_protocol::processes::ProcessVersion,
) -> tentaflow_protocol::processes::ProcessModel {
    use tentaflow_protocol::processes::{ProcessNode, ProcessNodeKind};
    let mut parent = super::call_tests::caller(&child, BTreeMap::new());
    parent.target_namespace = Some("urn:orders".into());
    parent.signals = child.model.signals.clone();
    parent.nodes.extend([
        ProcessNode {
            id: "Split".into(),
            name: "Open work".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
        ProcessNode {
            id: "Join".into(),
            name: "Join work".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
        ProcessNode {
            id: "Catch_1".into(),
            name: "Wait for child signal".into(),
            kind: ProcessNodeKind::SignalCatch {
                signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
        },
    ]);
    parent.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitCall", "Split", "Call_1"),
        edge("SplitCatch", "Split", "Catch_1"),
        edge("CallJoin", "Call_1", "Join"),
        edge("CatchJoin", "Catch_1", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    parent
}

#[test]
fn canonical_call_child_signal_sees_the_parent_arm_in_the_same_transaction() {
    let fixture = Fixture::new();
    let child = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let parent = parent_arm_then_call_model(&child);
    let version = publish_model(&fixture, &parent);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("start a factual parent arm before called child emission");
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(
        &version.model,
        &instance_id,
        &fixture.owner,
        &version.definition_id,
        version.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    let mut forged = plan.clone();
    let arm = forged
        .events
        .iter_mut()
        .find(|event| event.kind == "signal_catch_opened")
        .unwrap();
    arm.data["attached_token_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&forged),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged parent arm changed durable rows: {error:#}"
    );

    let actual = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Waiting);
    assert_eq!(actual.subscriptions.len(), 1);
    assert_eq!(
        actual.subscriptions[0].status,
        ProcessSubscriptionStatus::Open
    );
    let conn = fixture.db.read().unwrap();
    let (children, receipts): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_calls WHERE parent_instance_id=?1),
            (SELECT COUNT(*) FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
                ON e.signal_id=r.signal_id WHERE e.source_instance_id IN
                (SELECT child_instance_id FROM bpmn_calls WHERE parent_instance_id=?1))",
            [&instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((children, receipts), (1, 1));
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(all_transition_rows(&fixture), committed);
    let drained = super::messages::drain_pending(&reopened, at_ms + 1);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn canonical_called_child_signal_denies_the_factual_sixty_fifth_recipient() {
    let fixture = Fixture::new();
    let catch = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    for _ in 0..64 {
        start_version(&fixture, &catch);
    }
    let child = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let parent = publish_model(&fixture, &parent_arm_then_call_model(&child));
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("deny called Signal after factual parent arm");
    let variables = serde_json::to_value(&parent.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(
        &parent.model,
        &instance_id,
        &fixture.owner,
        &parent.definition_id,
        parent.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    let mut forged = plan.clone();
    let arm = forged
        .events
        .iter_mut()
        .find(|event| event.kind == "signal_catch_opened")
        .unwrap();
    arm.data["attached_token_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &parent.definition_id,
        parent.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&forged),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged same-command parent arm changed durable rows: {error:#}"
    );

    repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &parent.definition_id,
        parent.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    let conn = fixture.db.read().unwrap();
    let child_id: String = conn
        .query_row(
            "SELECT child_instance_id FROM bpmn_calls WHERE parent_instance_id=?1",
            [&instance_id],
            |row| row.get(0),
        )
        .unwrap();
    let (emissions, receipts): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),
            (SELECT COUNT(*) FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
                ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1)",
            [&child_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((emissions, receipts), (0, 0));
    drop(conn);
    let child_state =
        repository::get_instance(&fixture.db, &fixture.owner, &child_id, None).unwrap();
    assert!(child_state
        .incidents
        .iter()
        .any(|incident| incident.code == "SIGNAL_RECIPIENT_LIMIT"));
    let events = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200)
        .unwrap()
        .0;
    assert!(!events.iter().any(|event| event.kind == "signal_admitted"));
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let replay = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &parent.definition_id,
        parent.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(replay.instance_id, instance_id);
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn forged_earlier_signal_catch_close_cannot_reduce_a_sixty_five_recipient_cohort() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    for _ in 0..64 {
        start_version(&fixture, &receiver_version);
    }
    let mut model = super::signal_tests::signal_throw_model();
    model.nodes.extend([
        ProcessNode {
            id: "Split".into(),
            name: "Open and emit".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
        ProcessNode {
            id: "Join".into(),
            name: "Join signal branches".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
        ProcessNode {
            id: "Catch_1".into(),
            name: "Local signal wait".into(),
            kind: ProcessNodeKind::SignalCatch {
                signal_ref: "Signal_1".into(),
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
        },
    ]);
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitCatch", "Split", "Catch_1"),
        edge("SplitThrow", "Split", "Throw_1"),
        edge("CatchJoin", "Catch_1", "Join"),
        edge("ThrowJoin", "Throw_1", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("reject forged earlier local Signal close");
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(
        &version.model,
        &instance_id,
        &fixture.owner,
        &version.definition_id,
        version.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    let arm_index = plan
        .events
        .iter()
        .position(|event| event.kind == "signal_catch_opened")
        .unwrap();
    let signal_index = plan
        .events
        .iter()
        .position(|event| event.kind == "signal_admitted")
        .unwrap();
    assert!(arm_index < signal_index);
    let arm = &plan.events[arm_index];
    let subscription_id = arm.data["subscription_id"].as_str().unwrap();
    let token_id = arm.data["attached_token_id"].as_str().unwrap();
    let mut forged = plan.clone();
    let mut close = arm.clone();
    close.kind = "subscription_cancelled".into();
    close.data = json!({"subscription_id":subscription_id,
        "attached_token_id":token_id,"reason":"activity_completed"});
    forged.events.insert(signal_index, close);
    forged.event_sources = forged
        .event_sources
        .into_iter()
        .map(|(index, source)| {
            (
                if index >= signal_index {
                    index + 1
                } else {
                    index
                },
                source,
            )
        })
        .collect();
    forged.event_ids = forged
        .event_ids
        .into_iter()
        .map(|(index, id)| {
            (
                if index >= signal_index {
                    index + 1
                } else {
                    index
                },
                id,
            )
        })
        .collect();
    for signal in &mut forged.create_signals {
        if signal.source_event_index >= signal_index {
            signal.source_event_index += 1;
        }
    }
    let before = all_transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&forged),
        at_ms,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("signal cohort closure lacks its exact subscription revision"),
        "forged earlier close failed at an unrelated guard: {error:#}"
    );
    assert_eq!(all_transition_rows(&fixture), before);
    let actual = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.incidents[0].code, "SIGNAL_RECIPIENT_LIMIT");
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_message_catch_frees_its_factual_pending_slot_before_signal_admission() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    start_version(&fixture, &signal_target);
    let mut model = receiving_model(false, false);
    let throw = super::signal_tests::signal_throw_model();
    model.target_namespace = throw.target_namespace.clone();
    model.signals = throw.signals;
    model.variables.extend(throw.variables);
    model.nodes.insert(
        2,
        throw
            .nodes
            .into_iter()
            .find(|node| node.id == "Throw_1")
            .unwrap(),
    );
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchThrow", "Catch_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill pending before selected message"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let selected = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("accept final pending message"),
        &envelope(
            catch_target(
                &version,
                Some(&receiver.instance_id),
                Some(&receiver.subscriptions[0].subscription_id),
            ),
            json!({"source":"selected"}),
        ),
        at_ms,
    )
    .unwrap();
    let candidate = repository::MessageCandidate {
        key: repository::MessageKey {
            org_id: fixture.owner.org_id.clone(),
            sender_user_id: fixture.owner.user_id.clone(),
            message_id: selected.message_id.clone(),
        },
        revision: selected.revision,
        next_check_at_ms: at_ms,
    };
    let prepared = match repository::message_snapshot(&fixture.db, &candidate).unwrap() {
        repository::MessageSelection::Ready(snapshot) => snapshot,
        other => panic!("selected factual message is not ready: {other:?}"),
    };
    let forged_plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert_eq!(forged_plan.create_signals.len(), 1);
    let mut wrong_source = forged_plan.clone();
    let delivered = wrong_source
        .events
        .iter_mut()
        .find(|event| event.kind == "message_delivered")
        .unwrap();
    delivered.data["message_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Supplied(&wrong_source),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged input debit changed durable rows: {error:#}"
    );
    let mut duplicated_source = forged_plan.clone();
    let delivered_index = duplicated_source
        .events
        .iter()
        .position(|event| event.kind == "message_delivered")
        .unwrap();
    let delivered = duplicated_source.events[delivered_index].clone();
    duplicated_source.events.insert(delivered_index, delivered);
    assert_eq!(duplicated_source.events.len(), forged_plan.events.len() + 1);
    let error = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Supplied(&duplicated_source),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "duplicate input debit changed durable rows: {error:#}"
    );
    let mut late_source = forged_plan.clone();
    let delivered_index = late_source
        .events
        .iter()
        .position(|event| event.kind == "message_delivered")
        .unwrap();
    let admitted_index = late_source
        .events
        .iter()
        .position(|event| event.kind == "signal_admitted")
        .unwrap();
    assert!(delivered_index < admitted_index);
    late_source.events.swap(delivered_index, admitted_index);
    late_source.create_signals[0].source_event_index = delivered_index;
    if let Some(source) = late_source.event_sources.remove(&admitted_index) {
        late_source.event_sources.insert(delivered_index, source);
    }
    if let Some(event_id) = late_source.event_ids.remove(&admitted_index) {
        late_source.event_ids.insert(delivered_index, event_id);
    }
    assert!(
        late_source
            .events
            .iter()
            .position(|event| event.kind == "signal_admitted")
            .unwrap()
            < late_source
                .events
                .iter()
                .position(|event| event.kind == "message_delivered")
                .unwrap()
    );
    let error = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Supplied(&late_source),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "late factual input debit changed durable rows: {error:#}"
    );
    let outcome = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        outcome.transition.instance.status,
        ProcessInstanceStatus::Completed
    );
    let conn = fixture.db.read().unwrap();
    let (emissions, receipts): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),
            (SELECT COUNT(*) FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
                ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1)",
            [&receiver.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((emissions, receipts), (1, 1));
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(
        &reopened,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_message_start_frees_its_pending_slot_before_signal_admission() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    start_version(&fixture, &signal_target);
    let mut model = receiving_model(true, false);
    let throw = super::signal_tests::signal_throw_model();
    model.target_namespace = throw.target_namespace.clone();
    model.signals = throw.signals;
    model.variables.extend(throw.variables);
    model
        .variables
        .insert("received".into(), serde_json::Value::Null);
    model.nodes.insert(
        1,
        throw
            .nodes
            .into_iter()
            .find(|node| node.id == "Throw_1")
            .unwrap(),
    );
    model.sequence_flows = vec![
        edge("StartThrow", "Start_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill pending before message start"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let selected = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("accept pending start message"),
        &envelope(
            tentaflow_protocol::processes::ProcessMessageTarget::Start {
                definition_id: version.definition_id.clone(),
            },
            json!({"source":"start"}),
        ),
        at_ms,
    )
    .unwrap();
    let candidate = repository::MessageCandidate {
        key: repository::MessageKey {
            org_id: fixture.owner.org_id.clone(),
            sender_user_id: fixture.owner.user_id.clone(),
            message_id: selected.message_id.clone(),
        },
        revision: selected.revision,
        next_check_at_ms: at_ms,
    };
    let prepared = match repository::message_snapshot(&fixture.db, &candidate).unwrap() {
        repository::MessageSelection::Ready(snapshot) => snapshot,
        other => panic!("selected factual start message is not ready: {other:?}"),
    };
    let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert_eq!(plan.create_signals.len(), 1);
    let mut wrong_source = plan.clone();
    let started = wrong_source
        .events
        .iter_mut()
        .find(|event| event.kind == "instance_started")
        .unwrap();
    started.data["start_message_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Supplied(&wrong_source),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged start input debit changed durable rows: {error:#}"
    );
    let outcome = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        outcome.transition.instance.status,
        ProcessInstanceStatus::Completed
    );
    let instance_id = &outcome.transition.instance.instance_id;
    let conn = fixture.db.read().unwrap();
    let emissions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(emissions, 1);
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(
        &reopened,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_last_signal_receipt_frees_only_its_factual_pending_emission() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let mut output_target_model = super::signal_tests::signal_catch_model();
    output_target_model.signals[0].signal_id = "Signal_2".into();
    if let ProcessNodeKind::SignalCatch { signal_ref, .. } = &mut output_target_model.nodes[1].kind
    {
        *signal_ref = "Signal_2".into();
    }
    let output_target = publish_model(&fixture, &output_target_model);
    start_version(&fixture, &output_target);
    let mut model = super::signal_tests::signal_catch_model();
    let throw = super::signal_tests::signal_throw_model();
    model.variables.extend(throw.variables);
    model
        .signals
        .push(tentaflow_protocol::processes::ProcessSignalDeclaration {
            signal_id: "Signal_2".into(),
            namespace_uri: "urn:orders".into(),
            name: "After original receipt".into(),
        });
    let mut outbound = throw
        .nodes
        .into_iter()
        .find(|node| node.id == "Throw_1")
        .unwrap();
    if let ProcessNodeKind::SignalThrow { signal_ref, .. } = &mut outbound.kind {
        *signal_ref = "Signal_2".into();
    }
    model.nodes.insert(2, outbound);
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchThrow", "Catch_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let receiver_version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &receiver_version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill before selected Signal receipt"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let source = start_version(&fixture, &source_version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let conn = fixture.db.read().unwrap();
    let receipt_id: String = conn
        .query_row(
            "SELECT r.receipt_id FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
         ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1",
            [&source.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms + 10)
        .unwrap()
        .unwrap();
    let input = repository::AcceptedInputRef::Signal {
        signal_id: claim.signal_id.clone(),
        receipt_id: claim.receipt_id.clone(),
        expected_receipt_revision: claim.revision,
        claim_fence: claim.fence.clone(),
        target_subscription_id: claim.subscription.subscription_id.clone(),
        expected_subscription_revision: claim.subscription.revision,
    };
    let plan = runtime::plan_signal_catch(
        &claim.snapshot,
        &claim.subscription,
        &claim.payload,
        &claim.signal_id,
        &claim.source_event_id,
        at_ms + 10,
        input,
        None,
    )
    .unwrap();
    assert_eq!(plan.create_signals.len(), 1);
    let mut wrong_source = plan.clone();
    let received = wrong_source
        .events
        .iter_mut()
        .find(|event| event.kind == "signal_received")
        .unwrap();
    received.data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Supplied(&wrong_source),
        at_ms + 10,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged fenced receipt debit changed durable rows: {error:#}"
    );
    let actual = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10,
    )
    .unwrap()
    .unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Completed);
    let conn = fixture.db.read().unwrap();
    let (emissions, receipts): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1),
            (SELECT COUNT(*) FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e
                ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1)",
            [&receiver.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((emissions, receipts), (1, 1));
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_signal_receipt(
        &reopened,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_nonlast_signal_receipt_keeps_its_emission_pending() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let mut outbound_target_model = super::signal_tests::signal_catch_model();
    outbound_target_model.signals[0].signal_id = "Signal_2".into();
    if let ProcessNodeKind::SignalCatch { signal_ref, .. } =
        &mut outbound_target_model.nodes[1].kind
    {
        *signal_ref = "Signal_2".into();
    }
    let outbound_target = publish_model(&fixture, &outbound_target_model);
    start_version(&fixture, &outbound_target);
    let mut model = super::signal_tests::signal_catch_model();
    let throw = super::signal_tests::signal_throw_model();
    model.variables.extend(throw.variables);
    model
        .signals
        .push(tentaflow_protocol::processes::ProcessSignalDeclaration {
            signal_id: "Signal_2".into(),
            namespace_uri: "urn:orders".into(),
            name: "After one receipt".into(),
        });
    let mut outbound = throw
        .nodes
        .into_iter()
        .find(|node| node.id == "Throw_1")
        .unwrap();
    if let ProcessNodeKind::SignalThrow { signal_ref, .. } = &mut outbound.kind {
        *signal_ref = "Signal_2".into();
    }
    model.nodes.insert(2, outbound);
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchThrow", "Catch_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let receiver_version = publish_model(&fixture, &model);
    let selected_receiver = start_version(&fixture, &receiver_version);
    let second_receiver = start_version(&fixture, &receiver_version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill shared budget before nonlast receipt"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let source = start_version(&fixture, &source_version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let conn = fixture.db.read().unwrap();
    let (receipt_id, signal_id, source_bytes): (String, String, i64) = conn
        .query_row(
            "SELECT r.receipt_id,r.signal_id,e.payload_bytes FROM bpmn_signal_receipts r
         JOIN bpmn_signal_emissions e ON e.signal_id=r.signal_id
         WHERE e.source_instance_id=?1 AND r.recipient_instance_id=?2",
            rusqlite::params![source.instance_id, selected_receiver.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(conn);
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms + 10)
        .unwrap()
        .unwrap();
    let before = all_transition_rows(&fixture);
    let actual = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10,
    )
    .unwrap()
    .unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.instance.incidents[0].code, "SIGNAL_PENDING_LIMIT");
    let conn = fixture.db.read().unwrap();
    let (pending_siblings, outgoing, retained_bytes, emission_status): (i64, i64, i64, String) =
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_receipts
                    WHERE signal_id=?1 AND status='pending'),
                (SELECT COUNT(*) FROM bpmn_signal_emissions
                    WHERE source_instance_id=?2),
                (SELECT payload_bytes FROM bpmn_signal_emissions WHERE signal_id=?1),
                (SELECT status FROM bpmn_signal_emissions WHERE signal_id=?1)",
            rusqlite::params![signal_id, selected_receiver.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((pending_siblings, outgoing), (1, 0));
    assert_eq!(retained_bytes, source_bytes);
    assert_eq!(emission_status, "pending");
    assert_eq!(
        repository::get_instance(
            &fixture.db,
            &fixture.owner,
            &second_receiver.instance_id,
            None
        )
        .unwrap()
        .status,
        ProcessInstanceStatus::Waiting
    );
    assert_ne!(all_transition_rows(&fixture), before);
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_signal_receipt(
        &reopened,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_nonlast_signal_receipt_frees_one_factual_receipt_slot() {
    let fixture = Fixture::new();
    let mut outbound_target_model = super::signal_tests::signal_catch_model();
    outbound_target_model.signals[0].signal_id = "Signal_2".into();
    if let ProcessNodeKind::SignalCatch { signal_ref, .. } =
        &mut outbound_target_model.nodes[1].kind
    {
        *signal_ref = "Signal_2".into();
    }
    let outbound_target = publish_model(&fixture, &outbound_target_model);
    start_version(&fixture, &outbound_target);
    let mut model = super::signal_tests::parallel_signal_catches(64);
    let throw = super::signal_tests::signal_throw_model();
    model.variables.extend(throw.variables);
    model
        .signals
        .push(tentaflow_protocol::processes::ProcessSignalDeclaration {
            signal_id: "Signal_2".into(),
            namespace_uri: "urn:orders".into(),
            name: "Receipt slot continuation".into(),
        });
    let mut outbound = throw
        .nodes
        .into_iter()
        .find(|node| node.id == "Throw_1")
        .unwrap();
    if let ProcessNodeKind::SignalThrow { signal_ref, .. } = &mut outbound.kind {
        *signal_ref = "Signal_2".into();
    }
    model.nodes.push(outbound);
    model.sequence_flows.retain(|flow| flow.id != "Join_0");
    model
        .sequence_flows
        .push(edge("CatchThrow", "Catch_0", "Throw_1"));
    model
        .sequence_flows
        .push(edge("ThrowJoin", "Throw_1", "Join"));
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let full_receiver =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &receiver.instance_id).unwrap();
    assert_eq!(full_receiver.subscriptions.len(), 64);
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    for _ in 0..16 {
        assert_eq!(
            start_version(&fixture, &source_version).status,
            ProcessInstanceStatus::Completed
        );
    }
    let conn = fixture.db.read().unwrap();
    let before_receipts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before_receipts, 1024);
    let receipt_id: String = conn
        .query_row(
            "SELECT receipt_id FROM bpmn_signal_receipts WHERE recipient_instance_id=?1
         AND recipient_subscription_id=?2 AND status='pending' ORDER BY receipt_id LIMIT 1",
            rusqlite::params![
                receiver.instance_id,
                full_receiver
                    .subscriptions
                    .iter()
                    .find(|sub| sub.node_id == "Catch_0")
                    .unwrap()
                    .subscription_id
            ],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms + 10)
        .unwrap()
        .unwrap();
    let accepted = repository::AcceptedInputRef::Signal {
        signal_id: claim.signal_id.clone(),
        receipt_id: claim.receipt_id.clone(),
        expected_receipt_revision: claim.revision,
        claim_fence: claim.fence.clone(),
        target_subscription_id: claim.subscription.subscription_id.clone(),
        expected_subscription_revision: claim.subscription.revision,
    };
    let canonical_shape = runtime::plan_signal_catch(
        &claim.snapshot,
        &claim.subscription,
        &claim.payload,
        &claim.signal_id,
        &claim.source_event_id,
        at_ms + 10,
        accepted,
        None,
    )
    .unwrap();
    assert_eq!(canonical_shape.create_signals.len(), 1);
    let mut wrong_source = canonical_shape.clone();
    wrong_source
        .events
        .iter_mut()
        .find(|event| event.kind == "signal_received")
        .unwrap()
        .data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Supplied(&wrong_source),
        at_ms + 10,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "wrong receipt source changed durable rows: {error:#}"
    );
    let mut duplicated_source = canonical_shape.clone();
    let received_index = duplicated_source
        .events
        .iter()
        .position(|event| event.kind == "signal_received")
        .unwrap();
    let received = duplicated_source.events[received_index].clone();
    duplicated_source.events.insert(received_index, received);
    assert_eq!(
        duplicated_source.events.len(),
        canonical_shape.events.len() + 1
    );
    let error = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Supplied(&duplicated_source),
        at_ms + 10,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "duplicate receipt source changed durable rows: {error:#}"
    );
    let mut late_source = canonical_shape.clone();
    let received_index = late_source
        .events
        .iter()
        .position(|event| event.kind == "signal_received")
        .unwrap();
    let admitted_index = late_source
        .events
        .iter()
        .position(|event| event.kind == "signal_admitted")
        .unwrap();
    assert!(received_index < admitted_index);
    late_source.events.swap(received_index, admitted_index);
    late_source.create_signals[0].source_event_index = received_index;
    if let Some(source) = late_source.event_sources.remove(&admitted_index) {
        late_source.event_sources.insert(received_index, source);
    }
    if let Some(event_id) = late_source.event_ids.remove(&admitted_index) {
        late_source.event_ids.insert(received_index, event_id);
    }
    let error = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Supplied(&late_source),
        at_ms + 10,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "receipt fact after outbound source changed durable rows: {error:#}"
    );
    let actual = repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10,
    )
    .unwrap()
    .unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Waiting);
    let conn = fixture.db.read().unwrap();
    let (pending_receipts, outgoing): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_signal_receipts WHERE status='pending'),
                (SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1)",
            [&receiver.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((pending_receipts, outgoing), (1024, 1));
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_signal_receipt(
        &reopened,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms + 10
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_message_catch_frees_its_slot_for_a_same_plan_directed_send() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let mut model = receiving_model(false, false);
    model.nodes.insert(
        2,
        ProcessNode {
            id: "Send_1".into(),
            name: "Forward received evidence".into(),
            kind: ProcessNodeKind::SendTask {
                message_ref: "Message_1".into(),
                target: ProcessMessageTargetSpec::Catch {
                    definition_id: filler_target.definition_id.clone(),
                    instance_id_expression: None,
                    subscription_id_expression: None,
                },
                correlation_expression: "'case-1'".into(),
                payload_expression: "{'forwarded': true}".into(),
                ttl_seconds: 120,
            },
            repeat: None,
        },
    );
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchSend", "Catch_1", "Send_1"),
        edge("SendEnd", "Send_1", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    for index in 0..1023 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill before forwarded message"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let selected = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("deliver selected final message"),
        &envelope(
            catch_target(
                &version,
                Some(&receiver.instance_id),
                Some(&receiver.subscriptions[0].subscription_id),
            ),
            json!({"source":"selected"}),
        ),
        at_ms,
    )
    .unwrap();
    let candidate = repository::MessageCandidate {
        key: repository::MessageKey {
            org_id: fixture.owner.org_id.clone(),
            sender_user_id: fixture.owner.user_id.clone(),
            message_id: selected.message_id.clone(),
        },
        revision: selected.revision,
        next_check_at_ms: at_ms,
    };
    let prepared = match repository::message_snapshot(&fixture.db, &candidate).unwrap() {
        repository::MessageSelection::Ready(snapshot) => snapshot,
        other => panic!("selected directed input is not ready: {other:?}"),
    };
    let before = all_transition_rows(&fixture);
    let actual = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        actual.transition.instance.status,
        ProcessInstanceStatus::Completed
    );
    assert_ne!(all_transition_rows(&fixture), before);
    let conn = fixture.db.read().unwrap();
    let forwarded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1 AND status='pending'",
            [&receiver.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(forwarded, 1);
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(
        &reopened,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn accepted_message_catch_frees_actual_payload_bytes_before_signal_admission() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    start_version(&fixture, &signal_target);
    let mut model = receiving_model(false, false);
    let throw = super::signal_tests::signal_throw_model();
    model.target_namespace = throw.target_namespace.clone();
    model.signals = throw.signals;
    model.variables.extend(throw.variables);
    model.nodes.insert(
        2,
        throw
            .nodes
            .into_iter()
            .find(|node| node.id == "Throw_1")
            .unwrap(),
    );
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchThrow", "Catch_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let full_payload = json!("x".repeat(256 * 1024 - 2));
    assert_eq!(serde_json::to_vec(&full_payload).unwrap().len(), 256 * 1024);
    for _ in 0..255 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill personal pending bytes"),
            &envelope(
                catch_target(&filler_target, None, None),
                full_payload.clone(),
            ),
            at_ms,
        )
        .unwrap();
    }
    let partial_payload = json!("x".repeat(256 * 1024 - 130));
    let selected_payload = json!("s".repeat(126));
    assert_eq!(
        serde_json::to_vec(&partial_payload).unwrap().len()
            + serde_json::to_vec(&selected_payload).unwrap().len(),
        256 * 1024
    );
    repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("fill final personal pending bytes"),
        &envelope(catch_target(&filler_target, None, None), partial_payload),
        at_ms,
    )
    .unwrap();
    let selected = repository::send_message(
        &fixture.db,
        &fixture.owner,
        &stamp("accept payload byte credit"),
        &envelope(
            catch_target(
                &version,
                Some(&receiver.instance_id),
                Some(&receiver.subscriptions[0].subscription_id),
            ),
            selected_payload,
        ),
        at_ms,
    )
    .unwrap();
    let conn = fixture.db.read().unwrap();
    let pending_bytes: i64 = conn
        .query_row(
            "SELECT SUM(payload_bytes) FROM bpmn_messages WHERE status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending_bytes, 64 * 1024 * 1024);
    drop(conn);
    let candidate = repository::MessageCandidate {
        key: repository::MessageKey {
            org_id: fixture.owner.org_id.clone(),
            sender_user_id: fixture.owner.user_id.clone(),
            message_id: selected.message_id.clone(),
        },
        revision: selected.revision,
        next_check_at_ms: at_ms,
    };
    let prepared = match repository::message_snapshot(&fixture.db, &candidate).unwrap() {
        repository::MessageSelection::Ready(snapshot) => snapshot,
        other => panic!("selected byte-credit message is not ready: {other:?}"),
    };
    let before = all_transition_rows(&fixture);
    let actual = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        actual.transition.instance.status,
        ProcessInstanceStatus::Completed
    );
    assert_ne!(all_transition_rows(&fixture), before);
    let conn = fixture.db.read().unwrap();
    let emission_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&receiver.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(emission_count, 1);
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(
        &reopened,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn incoming_other_sender_does_not_free_the_outgoing_senders_full_budget() {
    use super::messages::test_support::{catch_target, envelope, receiving_model};
    let fixture = Fixture::new();
    let filler_target = publish_model(&fixture, &receiving_model(false, false));
    let signal_target = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    start_version(&fixture, &signal_target);
    let mut model = receiving_model(false, false);
    let throw = super::signal_tests::signal_throw_model();
    model.target_namespace = throw.target_namespace.clone();
    model.signals = throw.signals;
    model.variables.extend(throw.variables);
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Manual".into(),
            name: "Grant assigned instance reader".into(),
            kind: ProcessNodeKind::ManualTask {
                assignee_user_id: Some(fixture.participant.user_id.clone()),
                instructions: "Acknowledge external preparation before receipt.".into(),
            },
            repeat: None,
        },
    );
    model.nodes.insert(
        3,
        throw
            .nodes
            .into_iter()
            .find(|node| node.id == "Throw_1")
            .unwrap(),
    );
    model.sequence_flows = vec![
        edge("ToManual", "Start_1", "Manual"),
        edge("ManualCatch", "Manual", "Catch_1"),
        edge("CatchThrow", "Catch_1", "Throw_1"),
        edge("ThrowEnd", "Throw_1", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let manual_wait = start_version(&fixture, &version);
    let manual = manual_wait
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Manual")
        .unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let acknowledged = repository::acknowledge_manual_task(
        &fixture.db,
        &fixture.participant,
        &stamp("allow exact assigned message sender"),
        &manual_wait.instance_id,
        &manual.user_task_id,
        manual_wait.revision,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(acknowledged.instance.status, ProcessInstanceStatus::Waiting);
    let receiver = acknowledged.instance;
    for index in 0..1024 {
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("fill owner's personal pending count"),
            &envelope(
                catch_target(&filler_target, None, None),
                json!({"index":index}),
            ),
            at_ms,
        )
        .unwrap();
    }
    let selected = repository::send_message(
        &fixture.db,
        &fixture.participant,
        &stamp("other sender to assigned readable instance"),
        &envelope(
            catch_target(
                &version,
                Some(&receiver.instance_id),
                Some(&receiver.subscriptions[0].subscription_id),
            ),
            json!({"source":"participant"}),
        ),
        at_ms,
    )
    .unwrap();
    let candidate = repository::MessageCandidate {
        key: repository::MessageKey {
            org_id: fixture.participant.org_id.clone(),
            sender_user_id: fixture.participant.user_id.clone(),
            message_id: selected.message_id.clone(),
        },
        revision: selected.revision,
        next_check_at_ms: at_ms,
    };
    let prepared = match repository::message_snapshot(&fixture.db, &candidate).unwrap() {
        repository::MessageSelection::Ready(snapshot) => snapshot,
        other => panic!("assigned sender's message is not ready: {other:?}"),
    };
    let before = all_transition_rows(&fixture);
    let actual = repository::deliver_message(
        &fixture.db,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        actual.transition.instance.status,
        ProcessInstanceStatus::Incident
    );
    assert_eq!(
        actual.transition.instance.incidents[0].code,
        "SIGNAL_PENDING_LIMIT"
    );
    let conn = fixture.db.read().unwrap();
    let outgoing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_signal_emissions WHERE source_instance_id=?1",
            [&receiver.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outgoing, 0);
    drop(conn);
    assert_ne!(all_transition_rows(&fixture), before);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(
        &reopened,
        &prepared,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn signal_admission_rejects_forged_source_payload_and_successors_before_real_commit() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &receiver_version);
    let version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("signal-admission-proof");
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(
        &version.model,
        &instance_id,
        &fixture.owner,
        &version.definition_id,
        version.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    assert_eq!(plan.create_signals.len(), 1);
    let admitted = plan
        .events
        .iter()
        .position(|event| event.kind == "signal_admitted")
        .unwrap();
    let before = all_transition_rows(&fixture);
    let mut mutants = Vec::new();

    let mut omitted = plan.clone();
    omitted.events[admitted].kind = "node_completed".into();
    mutants.push(omitted);
    let mut extra = plan.clone();
    extra.events.push(extra.events[admitted].clone());
    mutants.push(extra);
    let mut wrong_source = plan.clone();
    wrong_source
        .event_sources
        .insert(admitted, Uuid::new_v4().to_string());
    mutants.push(wrong_source);
    let mut no_source = plan.clone();
    no_source.event_sources.remove(&admitted);
    mutants.push(no_source);
    let mut wrong_event_id = plan.clone();
    wrong_event_id
        .event_ids
        .insert(admitted, "not-a-uuid".into());
    mutants.push(wrong_event_id);
    let mut no_event_id = plan.clone();
    no_event_id.event_ids.remove(&admitted);
    mutants.push(no_event_id);
    let mut wrong_identity = plan.clone();
    wrong_identity.events[admitted].data["signal_id"] = json!(Uuid::new_v4().to_string());
    mutants.push(wrong_identity);
    let mut wrong_namespace = plan.clone();
    wrong_namespace.events[admitted].data["signal_namespace_uri"] = json!("urn:foreign");
    mutants.push(wrong_namespace);
    let mut wrong_payload = plan.clone();
    wrong_payload.create_signals[0].payload = json!({"business_key":"forged"});
    mutants.push(wrong_payload);
    let mut wrong_ttl = plan.clone();
    wrong_ttl.create_signals[0].ttl_seconds += 1;
    mutants.push(wrong_ttl);
    let mut wrong_index = plan.clone();
    wrong_index.create_signals[0].source_event_index = 0;
    mutants.push(wrong_index);
    let mut duplicate_emission = plan.clone();
    duplicate_emission
        .create_signals
        .push(plan.create_signals[0].clone());
    mutants.push(duplicate_emission);
    let mut no_emission = plan.clone();
    no_emission.create_signals.clear();
    mutants.push(no_emission);
    let mut wrong_successor = plan.clone();
    let outgoing = wrong_successor
        .create_tokens
        .iter_mut()
        .find(|token| token.node_id == "End_1")
        .unwrap();
    outgoing.arrival_edge_id = Some("wrong-edge".into());
    mutants.push(wrong_successor);
    for (ordinal, mutant) in mutants.into_iter().enumerate() {
        let error = repository::start_instance(
            &fixture.db,
            &fixture.owner,
            &command,
            &instance_id,
            &version.definition_id,
            version.version,
            &variables,
            repository::ProcessPlanInput::Supplied(&mutant),
            at_ms,
        )
        .unwrap_err();
        assert_eq!(
            all_transition_rows(&fixture),
            before,
            "forged admission {ordinal} changed durable rows: {error:#}"
        );
    }
    repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    assert_ne!(all_transition_rows(&fixture), before);
    let conn = fixture.db.read().unwrap();
    let (receipts, ordinal): (u32, u32) = conn
        .query_row(
            "SELECT COUNT(*),MIN(recipient_ordinal) FROM bpmn_signal_receipts WHERE signal_id=?1",
            [&plan.create_signals[0].signal_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((receipts, ordinal), (1, 0));
    drop(conn);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = all_transition_rows(&fixture);
    repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &version.definition_id,
        version.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    assert_eq!(all_transition_rows(&fixture), committed);
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
}

#[test]
fn signal_receipt_rejects_foreign_fence_payload_mapping_and_history_before_real_delivery() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &receiver_version);
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    let conn = fixture.db.read().unwrap();
    let receipt_id: String = conn
        .query_row(
            "SELECT r.receipt_id FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e \
         ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1",
            [&sender.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
        .unwrap()
        .unwrap();
    let accepted_input = repository::AcceptedInputRef::Signal {
        signal_id: claim.signal_id.clone(),
        receipt_id: claim.receipt_id.clone(),
        expected_receipt_revision: claim.revision,
        claim_fence: claim.fence.clone(),
        target_subscription_id: claim.subscription.subscription_id.clone(),
        expected_subscription_revision: claim.subscription.revision,
    };
    let plan = runtime::plan_signal_catch(
        &claim.snapshot,
        &claim.subscription,
        &claim.payload,
        &claim.signal_id,
        &claim.source_event_id,
        at_ms,
        accepted_input,
        None,
    )
    .unwrap();
    let received = plan
        .events
        .iter()
        .position(|event| event.kind == "signal_received")
        .unwrap();
    let before = all_transition_rows(&fixture);
    let mut claim_mutants = Vec::new();
    let mut wrong_actor = claim.clone();
    wrong_actor.actor.user_id = fixture.participant.user_id.clone();
    claim_mutants.push(wrong_actor);
    let mut wrong_org = claim.clone();
    wrong_org.actor.org_id = Uuid::new_v4().to_string();
    claim_mutants.push(wrong_org);
    let mut wrong_instance = claim.clone();
    wrong_instance.subscription.instance_id = sender.instance_id.clone();
    claim_mutants.push(wrong_instance);
    let mut wrong_snapshot = claim.clone();
    wrong_snapshot.snapshot.instance.instance_id = sender.instance_id.clone();
    claim_mutants.push(wrong_snapshot);
    let mut wrong_scope = claim.clone();
    wrong_scope.subscription.scope_id = Uuid::new_v4().to_string();
    claim_mutants.push(wrong_scope);
    let mut wrong_token = claim.clone();
    wrong_token.subscription.token_id = Uuid::new_v4().to_string();
    claim_mutants.push(wrong_token);
    let mut wrong_node = claim.clone();
    wrong_node.subscription.node_id = "Throw_1".into();
    claim_mutants.push(wrong_node);
    let mut wrong_version = claim.clone();
    wrong_version.subscription.version += 1;
    claim_mutants.push(wrong_version);
    let mut wrong_subscription = claim.clone();
    wrong_subscription.subscription.subscription_id = Uuid::new_v4().to_string();
    claim_mutants.push(wrong_subscription);
    let typed_denial: anyhow::Error =
        repository::ProcessAuthorityDenied("forged caller denial cannot settle a factual receipt")
            .into();
    for (ordinal, mutant) in claim_mutants.iter().enumerate() {
        assert!(repository::deliver_signal_receipt(
            &fixture.db,
            mutant,
            repository::ProcessPlanInput::Canonical,
            at_ms
        )
        .unwrap()
        .is_none());
        assert!(repository::deliver_signal_receipt(
            &fixture.db,
            mutant,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms
        )
        .unwrap()
        .is_none());
        repository::record_signal_attempt_failed(&fixture.db, mutant, &typed_denial, at_ms)
            .unwrap();
        assert_eq!(
            all_transition_rows(&fixture),
            before,
            "forged claim provenance {ordinal} changed durable rows"
        );
    }
    let mut mutants = Vec::new();
    let mut omitted = plan.clone();
    omitted.events[received].kind = "node_completed".into();
    mutants.push(omitted);
    let mut duplicate = plan.clone();
    duplicate.events.push(duplicate.events[received].clone());
    mutants.push(duplicate);
    let mut wrong_signal = plan.clone();
    wrong_signal.events[received].data["signal_id"] = json!(Uuid::new_v4().to_string());
    mutants.push(wrong_signal);
    let mut wrong_subscription = plan.clone();
    wrong_subscription.events[received].data["subscription_id"] = json!(Uuid::new_v4().to_string());
    mutants.push(wrong_subscription);
    let mut wrong_token = plan.clone();
    wrong_token.events[received].data["attached_token_id"] = json!(Uuid::new_v4().to_string());
    mutants.push(wrong_token);
    let mut wrong_source = plan.clone();
    wrong_source.events[received].data["source_event_id"] = json!(Uuid::new_v4().to_string());
    mutants.push(wrong_source);
    let mut wrong_fence = plan.clone();
    let wrong_fence_effect = wrong_fence
        .variable_effects
        .iter_mut()
        .find_map(|effect| match effect {
            VariableEffect::Mapped {
                accepted_input: Some(repository::AcceptedInputRef::Signal { claim_fence, .. }),
                ..
            } => Some(claim_fence),
            _ => None,
        })
        .expect("received Signal must retain its mapped fenced source");
    *wrong_fence_effect = Uuid::new_v4().to_string();
    mutants.push(wrong_fence);
    let mut no_consumption = plan.clone();
    no_consumption.subscription_updates.clear();
    mutants.push(no_consumption);
    let mut wrong_mapping = plan.clone();
    wrong_mapping.variables["received"] = json!({"business_key":"forged"});
    mutants.push(wrong_mapping);
    let mut late_mapping = plan.clone();
    let effect = late_mapping.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = effect else {
        unreachable!()
    };
    assert_eq!(*event_index, received);
    assert!(received + 1 < late_mapping.events.len());
    assert_ne!(late_mapping.events[received + 1].kind, "signal_received");
    *event_index = received + 1;
    mutants.push(late_mapping);
    let mut wrong_event_source = plan.clone();
    wrong_event_source
        .event_sources
        .insert(received, Uuid::new_v4().to_string());
    mutants.push(wrong_event_source);
    let mut wrong_revision = plan.clone();
    let wrong_revision_effect = wrong_revision
        .variable_effects
        .iter_mut()
        .find_map(|effect| match effect {
            VariableEffect::Mapped {
                accepted_input:
                    Some(repository::AcceptedInputRef::Signal {
                        expected_receipt_revision,
                        ..
                    }),
                ..
            } => Some(expected_receipt_revision),
            _ => None,
        })
        .expect("received Signal must retain its mapped receipt revision");
    *wrong_revision_effect += 1;
    mutants.push(wrong_revision);
    for (ordinal, mutant) in mutants.into_iter().enumerate() {
        let error = repository::deliver_signal_receipt(
            &fixture.db,
            &claim,
            repository::ProcessPlanInput::Supplied(&mutant),
            at_ms,
        )
        .unwrap_err();
        assert_eq!(
            all_transition_rows(&fixture),
            before,
            "forged receipt {ordinal} changed durable rows: {error:#}"
        );
    }
    repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap()
    .unwrap();
    assert_ne!(all_transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let replay = repository::deliver_signal_receipt(
        &reopened,
        &claim,
        repository::ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    assert!(replay.is_none());
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Consumed
    );
}

#[test]
fn factual_signal_claim_revocation_settles_denied_without_delivery() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &receiver_version);
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let source = start_version(&fixture, &source_version);
    let receipt_id: String = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT r.receipt_id FROM bpmn_signal_receipts r
         JOIN bpmn_signal_emissions e ON e.signal_id=r.signal_id
         WHERE e.source_instance_id=?1",
            [&source.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
        .unwrap()
        .unwrap();
    assert!(crate::services::org::repo::remove_membership(
        &fixture.db,
        &fixture.owner.org_id,
        &fixture.owner.user_id
    )
    .unwrap());
    let before = all_transition_rows(&fixture);
    assert!(repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    let conn = fixture.db.read().unwrap();
    let (status, reason): (String, String) = conn
        .query_row(
            "SELECT status,terminal_reason FROM bpmn_signal_receipts WHERE receipt_id=?1",
            [&receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), reason.as_str()),
        ("denied", "recipient_access_revoked")
    );
    let received: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='signal_received'",
            [&receiver.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(received, 0);
    drop(conn);
    assert_ne!(all_transition_rows(&fixture), before);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_signal_receipt(
        &reopened,
        &claim,
        repository::ProcessPlanInput::Canonical,
        at_ms
    )
    .unwrap()
    .is_none());
    assert_eq!(all_transition_rows(&fixture), committed);
}

#[test]
fn signal_receipt_exhaustion_records_one_factual_recipient_incident_and_terminal_reason() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &receiver_version);
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    let conn = fixture.db.read().unwrap();
    let receipt_id: String = conn
        .query_row(
            "SELECT r.receipt_id FROM bpmn_signal_receipts r JOIN bpmn_signal_emissions e \
         ON e.signal_id=r.signal_id WHERE e.source_instance_id=?1",
            [&sender.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let mut at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let mut fences = std::collections::HashSet::new();
    for attempt in 1..=5 {
        let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
            .unwrap()
            .unwrap();
        assert!(fences.insert(claim.fence.clone()));
        at_ms += 31_000;
        if attempt < 5 {
            let before_recovery = all_transition_rows(&fixture);
            assert!(
                repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
                    .unwrap()
                    .is_none()
            );
            let delay = (1i64 << (attempt - 1)) * 1000;
            let conn = fixture.db.read().unwrap();
            let (status, attempts, next_check, fence): (String, u32, i64, Option<String>) = conn
                .query_row(
                    "SELECT status,attempt_count,next_check_at_ms,claim_fence
                     FROM bpmn_signal_receipts WHERE receipt_id=?1",
                    [&receipt_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (status.as_str(), attempts, next_check, fence),
                ("pending", attempt, at_ms + delay, None)
            );
            drop(conn);
            let recovered = all_transition_rows(&fixture);
            assert_ne!(recovered, before_recovery);
            assert!(
                repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms + delay - 1)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(all_transition_rows(&fixture), recovered);
            assert!(repository::deliver_signal_receipt(
                &fixture.db,
                &claim,
                repository::ProcessPlanInput::Canonical,
                at_ms + delay - 1
            )
            .unwrap()
            .is_none());
            assert_eq!(all_transition_rows(&fixture), recovered);
            at_ms += delay;
        }
    }
    let before_exhaustion = all_transition_rows(&fixture);
    assert!(
        repository::claim_signal_receipt(&fixture.db, &receipt_id, at_ms)
            .unwrap()
            .is_none()
    );
    assert_ne!(all_transition_rows(&fixture), before_exhaustion);
    let after =
        repository::get_instance(&fixture.db, &fixture.owner, &receiver.instance_id, None).unwrap();
    assert_eq!(
        after.subscriptions[0].status,
        ProcessSubscriptionStatus::Error
    );
    assert_eq!(
        after
            .incidents
            .iter()
            .filter(|incident| incident.code == "SIGNAL_DELIVERY_FAILED")
            .count(),
        1
    );
    assert!(
        repository::list_events(&fixture.db, &fixture.owner, &receiver.instance_id, 0, 200)
            .unwrap()
            .0
            .iter()
            .all(|event| event.kind != "signal_received")
    );
    let conn = fixture.db.read().unwrap();
    let (status, reason, incident_id, attempts): (String, String, String, u32) = conn.query_row(
        "SELECT status,terminal_reason,incident_id,attempt_count FROM bpmn_signal_receipts WHERE receipt_id=?1",
        [&receipt_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(
        (status.as_str(), reason.as_str()),
        ("error", "transient_retry_exhausted")
    );
    assert_eq!(attempts, 5);
    assert_eq!(
        incident_id,
        after
            .incidents
            .iter()
            .find(|incident| incident.code == "SIGNAL_DELIVERY_FAILED")
            .unwrap()
            .incident_id
    );
    drop(conn);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .incidents
            .iter()
            .filter(|incident| incident.code == "SIGNAL_DELIVERY_FAILED")
            .count(),
        1
    );
    assert!(
        repository::claim_signal_receipt(&reopened, &receipt_id, at_ms)
            .unwrap()
            .is_none()
    );
}

#[test]
fn signal_receipt_ttl_wins_during_a_factual_expired_lease_backoff() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &super::signal_tests::signal_catch_model());
    let receiver = start_version(&fixture, &receiver_version);
    let source_version = publish_model(&fixture, &super::signal_tests::signal_throw_model());
    let sender = start_version(&fixture, &source_version);
    let conn = fixture.db.read().unwrap();
    let (receipt_id, expires_at_ms): (String, i64) = conn
        .query_row(
            "SELECT r.receipt_id,e.expires_at_ms FROM bpmn_signal_receipts r
         JOIN bpmn_signal_emissions e ON e.signal_id=r.signal_id
         WHERE e.source_instance_id=?1",
            [&sender.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(conn);
    let claimed_at_ms = chrono::Utc::now().timestamp_millis() + 10;
    let claim = repository::claim_signal_receipt(&fixture.db, &receipt_id, claimed_at_ms)
        .unwrap()
        .unwrap();
    let recovery_at_ms = expires_at_ms - 500;
    assert!(recovery_at_ms >= claimed_at_ms + 30_000);
    assert!(
        repository::claim_signal_receipt(&fixture.db, &receipt_id, recovery_at_ms)
            .unwrap()
            .is_none()
    );
    let conn = fixture.db.read().unwrap();
    let (status, next_check, attempts): (String, i64, u32) = conn
        .query_row(
            "SELECT status,next_check_at_ms,attempt_count FROM bpmn_signal_receipts
         WHERE receipt_id=?1",
            [&receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), next_check, attempts),
        ("pending", recovery_at_ms + 1000, 1)
    );
    drop(conn);
    let before_expiry = all_transition_rows(&fixture);
    assert!(
        repository::claim_signal_receipt(&fixture.db, &receipt_id, expires_at_ms)
            .unwrap()
            .is_none()
    );
    assert_ne!(all_transition_rows(&fixture), before_expiry);
    assert!(repository::deliver_signal_receipt(
        &fixture.db,
        &claim,
        repository::ProcessPlanInput::Canonical,
        expires_at_ms
    )
    .unwrap()
    .is_none());
    let conn = fixture.db.read().unwrap();
    let (status, reason, attempts): (String, String, u32) = conn
        .query_row(
            "SELECT status,terminal_reason,attempt_count FROM bpmn_signal_receipts
         WHERE receipt_id=?1",
            [&receipt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), reason.as_str(), attempts),
        ("expired", "ttl_expired", 1)
    );
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(
        repository::claim_signal_receipt(&reopened, &receipt_id, expires_at_ms + 1000)
            .unwrap()
            .is_none()
    );
    assert_eq!(all_transition_rows(&fixture), committed);
    assert_eq!(
        repository::get_instance(&reopened, &fixture.owner, &receiver.instance_id, None)
            .unwrap()
            .subscriptions[0]
            .status,
        ProcessSubscriptionStatus::Open
    );
}

#[test]
fn canonical_call_start_retains_an_archived_target_incident_without_a_phantom_child() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::model::starter_model());
    let caller = publish_model(
        &fixture,
        &super::call_tests::caller(&target, BTreeMap::new()),
    );
    let target_draft =
        repository::get_definition(&fixture.db, &fixture.owner, &target.definition_id)
            .unwrap()
            .0;
    repository::archive_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("archive called target"),
        &target.definition_id,
        target_draft.draft_revision,
        true,
    )
    .unwrap();

    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("start archived called target");
    let variables = serde_json::to_value(&caller.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(
        &caller.model,
        &instance_id,
        &fixture.owner,
        &caller.definition_id,
        caller.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        manual_input(&command),
        None,
    )
    .unwrap();
    let mut forged = plan.clone();
    let requested = forged
        .events
        .iter_mut()
        .find(|event| event.kind == "call_requested")
        .unwrap();
    requested.data["call_id"] = json!(Uuid::new_v4().to_string());
    let before = all_transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &caller.definition_id,
        caller.version,
        &variables,
        repository::ProcessPlanInput::Supplied(&forged),
        at_ms,
    )
    .unwrap_err();
    assert_eq!(
        all_transition_rows(&fixture),
        before,
        "forged Call request changed durable rows: {error:#}"
    );

    let actual = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &command,
        &instance_id,
        &caller.definition_id,
        caller.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.incidents.len(), 1);
    assert_eq!(actual.incidents[0].code, "CALL_ADMISSION_ERROR");
    let conn = fixture.db.read().unwrap();
    let children: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_calls WHERE parent_instance_id=?1",
            [&instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(children, 0);
    drop(conn);
    let committed = all_transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let replay = repository::start_instance(
        &reopened,
        &fixture.owner,
        &command,
        &instance_id,
        &caller.definition_id,
        caller.version,
        &variables,
        repository::ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(
        replay.incidents[0].incident_id,
        actual.incidents[0].incident_id
    );
    assert_eq!(all_transition_rows(&fixture), committed);
}
