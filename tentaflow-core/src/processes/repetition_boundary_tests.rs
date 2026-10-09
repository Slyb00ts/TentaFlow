// ============ File: repetition_boundary_tests.rs — File-backed outer repetition boundary proofs ============

use super::messages::test_support::start_version;
use super::repository::{self, ProcessPlanInput};
use super::runtime;
use super::runtime::test_support::{edge, publish_model, Fixture};
use serde_json::json;
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessCallStatus, ProcessErrorDeclaration, ProcessEscalationDeclaration,
    ProcessInstanceStatus, ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNode,
    ProcessNodeKind, ProcessRepeatSpec, ProcessRepetitionGroupStatus,
    ProcessRepetitionOccurrenceStatus, ProcessTimerSpec, ProcessTimerStatus, ProcessUserTaskStatus,
};
use uuid::Uuid;

fn repeated_manual_timer_model(interrupt: bool) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::manual_tests::manual_model(None, false);
    model.timer_timezone = Some("UTC".into());
    model
        .variables
        .insert("results".into(), serde_json::json!([]));
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Manual")
        .unwrap()
        .repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    model.nodes.extend([
        ProcessNode {
            id: "OuterTimer".into(),
            name: "Outer deadline".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Manual".into(),
                cancel_activity: interrupt,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            },
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "TimeoutEnd".into(),
            name: "Timed out".into(),
            kind: ProcessNodeKind::End,
            repeat: None,
            activity_io: None,
        },
    ]);
    model
        .sequence_flows
        .push(edge("OuterTimeoutFlow", "OuterTimer", "TimeoutEnd"));
    model
}

fn attach_outer_timer(
    model: &mut tentaflow_protocol::processes::ProcessModel,
    attached_to_id: &str,
    boundary_id: &str,
) {
    model.timer_timezone = Some("UTC".into());
    model.nodes.push(ProcessNode {
        id: boundary_id.into(),
        name: "Cancel repeated activity".into(),
        kind: ProcessNodeKind::BoundaryTimer {
            attached_to_id: attached_to_id.into(),
            cancel_activity: true,
            timer: ProcessTimerSpec::Duration { seconds: 1 },
        },
        repeat: None,
        activity_io: None,
    });
    model.nodes.push(ProcessNode {
        id: "BoundaryEnd".into(),
        name: "Boundary end".into(),
        kind: ProcessNodeKind::End,
        repeat: None,
        activity_io: None,
    });
    model
        .sequence_flows
        .push(edge("BoundaryToEnd", boundary_id, "BoundaryEnd"));
}

#[test]
fn interrupting_outer_timer_closes_both_manual_ordinals_and_one_coordinator() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &repeated_manual_timer_model(true));
    let started = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_occurrences.len(), 2);
    assert_eq!(
        snapshot
            .user_tasks
            .iter()
            .filter(|task| task.node_id == "Manual" && task.status == ProcessUserTaskStatus::Open)
            .count(),
        2
    );
    let group = &snapshot.repetition_groups[0];
    let outer = snapshot
        .timers
        .iter()
        .filter(|timer| {
            timer.node_id == "OuterTimer" && timer.status == ProcessTimerStatus::Pending
        })
        .collect::<Vec<_>>();
    assert_eq!(outer.len(), 1);
    assert_eq!(
        outer[0].token_id.as_deref(),
        Some(group.parent_token_id.as_str())
    );
    let due = outer[0].due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, due, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.timer_id == outer[0].timer_id)
        .unwrap();
    let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&timer_snapshot, due, None, None).unwrap();
    assert_eq!(plan.cancel_user_task_ids.len(), 2);
    assert_eq!(
        plan.repetition_groups
            .iter()
            .filter(|row| row.group_id == group.group_id
                && row.status == ProcessRepetitionGroupStatus::Cancelled)
            .count(),
        1
    );
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("missing ordinal closure", {
            let mut forged = plan.clone();
            forged.cancel_user_task_ids.pop();
            forged
        }),
        ("foreign ordinal cancellation", {
            let mut forged = plan.clone();
            forged.cancel_user_task_ids[0] = Uuid::new_v4().to_string();
            forged
        }),
        ("coordinator left open", {
            let mut forged = plan.clone();
            forged
                .repetition_groups
                .iter_mut()
                .find(|row| row.group_id == group.group_id)
                .unwrap()
                .status = ProcessRepetitionGroupStatus::Open;
            forged
        }),
        ("one ordinal left active", {
            let mut forged = plan.clone();
            forged
                .repetition_occurrences
                .iter_mut()
                .find(|row| row.status == ProcessRepetitionOccurrenceStatus::Cancelled)
                .unwrap()
                .status = ProcessRepetitionOccurrenceStatus::Active;
            forged
        }),
    ] {
        let error = repository::fire_timer(
            &fixture.db,
            &candidate,
            &fixture.owner,
            Some(snapshot.instance.revision),
            ProcessPlanInput::Supplied(&forged),
            due,
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::fire_timer(
        &reopened,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&plan),
        due,
    )
    .unwrap()
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Completed);
    let after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert!(after
        .repetition_occurrences
        .iter()
        .all(|row| row.status == ProcessRepetitionOccurrenceStatus::Cancelled));
    assert!(after
        .user_tasks
        .iter()
        .all(|task| task.status != ProcessUserTaskStatus::Open));
    let committed_rows = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::fire_timer(
        &reopened,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&plan),
        due
    )
    .unwrap()
    .is_none());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        committed_rows
    );
}

#[test]
fn one_completed_ordinal_does_not_disarm_the_outer_timer() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &repeated_manual_timer_model(false));
    let started = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let first = snapshot
        .repetition_occurrences
        .iter()
        .find(|row| row.ordinal == 0)
        .unwrap();
    let task_id = first.user_task_id.as_ref().unwrap();
    let command = runtime::test_support::stamp("first repeated acknowledgment");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_manual_acknowledgment(
        &snapshot,
        task_id,
        &fixture.owner.user_id,
        at_ms,
        super::manual_tests::manual_entry(&snapshot, task_id, &command),
        None,
    )
    .unwrap();
    repository::acknowledge_manual_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        task_id,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&plan),
        at_ms,
    )
    .unwrap();
    let remaining =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(remaining.repetition_groups[0].completed_count, 1);
    assert_eq!(
        remaining
            .timers
            .iter()
            .filter(|timer| timer.node_id == "OuterTimer"
                && timer.status == ProcessTimerStatus::Pending)
            .count(),
        1
    );
    let timer = remaining
        .timers
        .iter()
        .find(|timer| timer.node_id == "OuterTimer")
        .unwrap();
    let due = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, due, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id)
        .unwrap();
    let committed = repository::fire_timer(
        &fixture.db,
        &candidate,
        &fixture.owner,
        Some(remaining.instance.revision),
        ProcessPlanInput::Canonical,
        due,
    )
    .unwrap()
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Waiting);
    let after =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(after.repetition_groups[0].completed_count, 1);
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Open
    );
    assert_eq!(
        after
            .user_tasks
            .iter()
            .filter(|task| task.node_id == "Manual" && task.status == ProcessUserTaskStatus::Open)
            .count(),
        1
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::fire_timer(
        &reopened,
        &candidate,
        &fixture.owner,
        Some(remaining.instance.revision),
        ProcessPlanInput::Canonical,
        due
    )
    .unwrap()
    .is_none());
}

#[test]
fn outer_timer_disarms_only_after_the_whole_manual_group_completes() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &repeated_manual_timer_model(true));
    let started = start_version(&fixture, &version);
    for ordinal in 0..2 {
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let occurrence = snapshot
            .repetition_occurrences
            .iter()
            .find(|row| row.ordinal == ordinal)
            .unwrap();
        let task_id = occurrence.user_task_id.as_deref().unwrap();
        let command = runtime::test_support::stamp(&format!("complete ordinal {ordinal}"));
        let at = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(
            &snapshot,
            task_id,
            &fixture.owner.user_id,
            at,
            super::manual_tests::manual_entry(&snapshot, task_id, &command),
            None,
        )
        .unwrap();
        if ordinal == 0 {
            assert!(plan
                .timer_updates
                .iter()
                .all(|update| update.status != ProcessTimerStatus::Cancelled));
        } else {
            assert_eq!(
                plan.timer_updates
                    .iter()
                    .filter(|update| update.status == ProcessTimerStatus::Cancelled)
                    .count(),
                1
            );
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            let mut forged = plan.clone();
            forged.timer_updates.clear();
            assert!(repository::acknowledge_manual_task(
                &fixture.db,
                &fixture.owner,
                &command,
                &started.instance_id,
                task_id,
                snapshot.instance.revision,
                ProcessPlanInput::Supplied(&forged),
                at
            )
            .is_err());
            assert_eq!(
                super::signal_proof_tests::all_transition_rows(&fixture),
                before
            );
        }
        repository::acknowledge_manual_task(
            &fixture.db,
            &fixture.owner,
            &command,
            &started.instance_id,
            task_id,
            snapshot.instance.revision,
            ProcessPlanInput::Supplied(&plan),
            at,
        )
        .unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let after =
            repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.repetition_groups[0].completed_count, ordinal + 1);
        assert_eq!(
            after
                .timers
                .iter()
                .filter(|timer| timer.node_id == "OuterTimer"
                    && timer.status == ProcessTimerStatus::Pending)
                .count(),
            usize::from(ordinal == 0)
        );
    }
    let completed =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
}

#[test]
fn outer_message_interrupts_the_remaining_ordinals_after_one_completion() {
    use super::messages::test_support::{catch_target, envelope, send};
    use super::repository::MessageSelection;
    use tentaflow_protocol::processes::ProcessSubscriptionStatus;

    for complete_first in [false, true] {
        let fixture = Fixture::new();
        let model = super::messages::test_support::boundary_messages(
            repeated_manual_timer_model(false),
            "Manual",
            &[("OuterMessage", true, "OuterDeadline")],
        );
        let version = publish_model(&fixture, &model);
        let started = start_version(&fixture, &version);
        if complete_first {
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let first = snapshot
                .repetition_occurrences
                .iter()
                .find(|row| row.ordinal == 0)
                .unwrap();
            let task_id = first.user_task_id.as_deref().unwrap();
            let command = runtime::test_support::stamp("one ordinal completed before message");
            let at = chrono::Utc::now().timestamp_millis();
            let plan = runtime::plan_manual_acknowledgment(
                &snapshot,
                task_id,
                &fixture.owner.user_id,
                at,
                super::manual_tests::manual_entry(&snapshot, task_id, &command),
                None,
            )
            .unwrap();
            repository::acknowledge_manual_task(
                &fixture.db,
                &fixture.owner,
                &command,
                &started.instance_id,
                task_id,
                snapshot.instance.revision,
                ProcessPlanInput::Supplied(&plan),
                at,
            )
            .unwrap();
        }
        let pending =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let group = &pending.repetition_groups[0];
        let boundary = pending
            .subscriptions
            .iter()
            .find(|sub| {
                sub.node_id == "OuterMessage" && sub.status == ProcessSubscriptionStatus::Open
            })
            .unwrap();
        assert_eq!(boundary.token_id, group.parent_token_id);
        assert_eq!(
            pending
                .subscriptions
                .iter()
                .filter(|sub| sub.node_id == "OuterMessage")
                .count(),
            1
        );
        let mut message = envelope(
            catch_target(
                &version,
                Some(&started.instance_id),
                Some(&boundary.subscription_id),
            ),
            json!({"factual":"deadline"}),
        );
        message.message_name = "OuterDeadline".into();
        let sent = send(&fixture, &message);
        let at = sent.received_at_ms + 1;
        let candidate = repository::due_messages(&fixture.db, at, 32)
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.key.message_id == sent.message_id)
            .unwrap();
        let MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else {
            panic!("factual outer message candidate was not ready")
        };
        let plan = super::messages::plan_message_delivery(&prepared, at, None).unwrap();
        assert_eq!(
            plan.cancel_user_task_ids.len(),
            if complete_first { 1 } else { 2 }
        );
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged.cancel_user_task_ids.clear();
        assert!(repository::deliver_message(
            &fixture.db,
            &prepared,
            ProcessPlanInput::Supplied(&forged),
            at
        )
        .is_err());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before
        );
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        repository::deliver_message(&reopened, &prepared, ProcessPlanInput::Supplied(&plan), at)
            .unwrap()
            .unwrap();
        let after =
            repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(
            after.repetition_groups[0].status,
            ProcessRepetitionGroupStatus::Cancelled
        );
        assert_eq!(
            after
                .repetition_occurrences
                .iter()
                .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Completed)
                .count(),
            usize::from(complete_first)
        );
        assert!(after
            .user_tasks
            .iter()
            .any(|task| task.node_id == "Side_OuterMessage"
                && task.status == ProcessUserTaskStatus::Open));
        assert!(after
            .user_tasks
            .iter()
            .filter(|task| task.node_id == "Manual")
            .all(|task| task.status != ProcessUserTaskStatus::Open));
        let committed = super::signal_proof_tests::all_transition_rows(&fixture);
        assert!(repository::deliver_message(
            &reopened,
            &prepared,
            ProcessPlanInput::Supplied(&plan),
            at
        )
        .unwrap()
        .is_none());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            committed
        );
    }
}

#[test]
fn receive_first_consumes_only_its_ordinal_then_outer_timer_cancels_the_other() {
    use super::messages::test_support::{catch_target, envelope, send};
    use super::repository::MessageSelection;
    use tentaflow_protocol::processes::ProcessSubscriptionStatus;

    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("results".into(), json!([]));
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Catch_1")
        .unwrap()
        .repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    attach_outer_timer(&mut model, "Catch_1", "ReceiveDeadline");
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let subscriptions = snapshot
        .subscriptions
        .iter()
        .filter(|sub| sub.node_id == "Catch_1" && sub.status == ProcessSubscriptionStatus::Open)
        .collect::<Vec<_>>();
    assert_eq!(subscriptions.len(), 2);
    assert_eq!(
        snapshot
            .timers
            .iter()
            .filter(|timer| timer.node_id == "ReceiveDeadline"
                && timer.status == ProcessTimerStatus::Pending)
            .count(),
        1
    );
    let message = envelope(
        catch_target(
            &version,
            Some(&started.instance_id),
            Some(&subscriptions[0].subscription_id),
        ),
        json!({"first":true}),
    );
    let sent = send(&fixture, &message);
    let at = sent.received_at_ms + 1;
    let candidate = repository::due_messages(&fixture.db, at, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.key.message_id == sent.message_id)
        .unwrap();
    let MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
    else {
        panic!("first Receive ordinal lost its factual message")
    };
    repository::deliver_message(&fixture.db, &prepared, ProcessPlanInput::Canonical, at)
        .unwrap()
        .unwrap();
    let after_first =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(after_first.repetition_groups[0].completed_count, 1);
    assert_eq!(
        after_first
            .subscriptions
            .iter()
            .filter(
                |sub| sub.node_id == "Catch_1" && sub.status == ProcessSubscriptionStatus::Consumed
            )
            .count(),
        1
    );
    assert_eq!(
        after_first
            .subscriptions
            .iter()
            .filter(|sub| sub.node_id == "Catch_1" && sub.status == ProcessSubscriptionStatus::Open)
            .count(),
        1
    );
    let timer = after_first
        .timers
        .iter()
        .find(|timer| timer.node_id == "ReceiveDeadline")
        .unwrap();
    let due = timer.due_at_ms.unwrap() + 1;
    let timer_candidate = repository::due_timers(&fixture.db, due, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id)
        .unwrap();
    let plan = super::timers::plan_timer_fire(
        &repository::timer_snapshot(&fixture.db, &timer_candidate).unwrap(),
        due,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        plan.subscription_updates
            .iter()
            .filter(|update| update.status == ProcessSubscriptionStatus::Cancelled)
            .count(),
        1
    );
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged
        .subscription_updates
        .retain(|update| update.status != ProcessSubscriptionStatus::Cancelled);
    assert!(repository::fire_timer(
        &fixture.db,
        &timer_candidate,
        &fixture.owner,
        Some(after_first.instance.revision),
        ProcessPlanInput::Supplied(&forged),
        due
    )
    .is_err());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(
        &reopened,
        &timer_candidate,
        &fixture.owner,
        Some(after_first.instance.revision),
        ProcessPlanInput::Supplied(&plan),
        due,
    )
    .unwrap()
    .unwrap();
    let after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert_eq!(
        after
            .repetition_occurrences
            .iter()
            .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Completed)
            .count(),
        1
    );
    assert_eq!(
        after
            .subscriptions
            .iter()
            .filter(|sub| sub.node_id == "Catch_1"
                && sub.status == ProcessSubscriptionStatus::Cancelled)
            .count(),
        1
    );
}

#[test]
fn outer_timer_closes_two_embedded_child_ordinals_without_cloning_the_arm() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, false);
    let mut model = runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "RepeatedScope")
        .unwrap()
        .repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    attach_outer_timer(&mut model, "RepeatedScope", "ScopeDeadline");
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let foreign = start_version(&fixture, &version);
    let foreign_snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &foreign.instance_id).unwrap();
    let foreign_group = &foreign_snapshot.repetition_groups[0];
    let foreign_scope = foreign_snapshot
        .scopes
        .iter()
        .find(|scope| {
            scope.parent_scope_id.as_deref() == Some(foreign.instance_id.as_str())
                && matches!(scope.status, ProcessInstanceStatus::Running | ProcessInstanceStatus::Waiting)
        })
        .unwrap();
    assert!(foreign_snapshot.repetition_occurrences.iter().any(|row|
        row.group_id == foreign_group.group_id
            && foreign_scope.parent_token_id.as_deref() == Some(row.token_id.as_str())
            && row.status == ProcessRepetitionOccurrenceStatus::Active));
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert_ne!(snapshot.repetition_groups[0].group_id, foreign_group.group_id);
    assert_eq!(
        snapshot
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.as_deref() == Some(started.instance_id.as_str()))
            .count(),
        2
    );
    assert_eq!(
        snapshot
            .timers
            .iter()
            .filter(|timer| timer.node_id == "ScopeDeadline"
                && timer.status == ProcessTimerStatus::Pending)
            .count(),
        1
    );
    let timer = snapshot
        .timers
        .iter()
        .find(|timer| timer.node_id == "ScopeDeadline")
        .unwrap();
    let at = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, at, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id)
        .unwrap();
    let plan = super::timers::plan_timer_fire(
        &repository::timer_snapshot(&fixture.db, &candidate).unwrap(),
        at,
        None,
        None,
    )
    .unwrap();
    assert_eq!(plan.cancel_scope_roots.len(), 2);
    assert!(plan.cancel_scope_roots.iter().all(|root| {
        snapshot.scopes.iter().any(|scope|
            &scope.scope_id == root && scope.parent_token_id.as_deref().is_some_and(|parent|
                snapshot.repetition_occurrences.iter().any(|row|
                    row.group_id == snapshot.repetition_groups[0].group_id
                        && row.token_id.as_str() == parent)))
    }));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.cancel_scope_roots.pop();
    assert!(repository::fire_timer(
        &fixture.db,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&forged),
        at
    )
    .is_err());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let mut foreign_child = plan.clone();
    foreign_child.cancel_scope_roots[0] = foreign_scope.scope_id.clone();
    assert!(repository::fire_timer(
        &fixture.db,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&foreign_child),
        at
    )
    .is_err());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(
        &reopened,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap()
    .unwrap();
    let after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert!(after
        .scopes
        .iter()
        .filter(|scope| scope.parent_scope_id.as_deref() == Some(started.instance_id.as_str()))
        .all(|scope| scope.status == ProcessInstanceStatus::Cancelled));
    assert!(after
        .user_tasks
        .iter()
        .all(|task| task.status != ProcessUserTaskStatus::Open));
    let foreign_after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &foreign.instance_id).unwrap();
    assert_eq!(foreign_after.repetition_groups[0].group_id, foreign_group.group_id);
    assert!(foreign_after.scopes.iter().any(|scope| {
        scope.scope_id == foreign_scope.scope_id
            && matches!(scope.status, ProcessInstanceStatus::Running | ProcessInstanceStatus::Waiting)
    }));
}

#[test]
fn outer_timer_closes_two_pinned_called_ordinals_and_their_real_children() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::manual_tests::manual_model(None, false));
    let mut model = super::call_tests::caller(&target, Default::default());
    model.variables.insert("results".into(), json!([]));
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Call_1")
        .unwrap()
        .repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    attach_outer_timer(&mut model, "Call_1", "CallDeadline");
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let calls = snapshot
        .calls
        .iter()
        .filter(|call| call.status == ProcessCallStatus::Waiting)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|call| call.called_definition_id == target.definition_id
            && call.called_version == target.version));
    let timer = snapshot
        .timers
        .iter()
        .find(|timer| {
            timer.node_id == "CallDeadline" && timer.status == ProcessTimerStatus::Pending
        })
        .unwrap();
    let at = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, at, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.timer_id == timer.timer_id)
        .unwrap();
    let plan = super::timers::plan_timer_fire(
        &repository::timer_snapshot(&fixture.db, &candidate).unwrap(),
        at,
        None,
        None,
    )
    .unwrap();
    assert_eq!(plan.cancel_token_ids.len(), 3);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged
        .cancel_token_ids
        .retain(|id| id != &calls[0].parent_token_id);
    assert!(repository::fire_timer(
        &fixture.db,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&forged),
        at
    )
    .is_err());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(
        &reopened,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap()
    .unwrap();
    let after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert!(after
        .calls
        .iter()
        .all(|call| call.status == ProcessCallStatus::Cancelled));
    for call in calls {
        let child =
            repository::runtime_snapshot(&reopened, &fixture.owner, &call.child_instance_id)
                .unwrap();
        assert_eq!(child.instance.status, ProcessInstanceStatus::Cancelled);
        assert!(child
            .user_tasks
            .iter()
            .all(|task| task.status != ProcessUserTaskStatus::Open));
    }
}

#[test]
fn terminate_inside_one_repeated_embedded_child_completes_only_its_ordinal() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, true);
    let mut model = runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "RepeatedScope")
        .unwrap()
        .repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    attach_outer_timer(&mut model, "RepeatedScope", "ScopeDeadline");
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    for ordinal in 0..2 {
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Manual" && task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let command = runtime::test_support::stamp(&format!("terminate child ordinal {ordinal}"));
        let at = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(
            &snapshot,
            &task.user_task_id,
            &fixture.owner.user_id,
            at,
            super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command),
            None,
        )
        .unwrap();
        let terminal = plan
            .events
            .iter()
            .position(|event| {
                event.kind == "terminate_end_reached" && event.scope_id == task.scope_id
            })
            .unwrap();
        let child = plan
            .events
            .iter()
            .position(|event| event.kind == "scope_completed" && event.scope_id == task.scope_id)
            .unwrap();
        let occurrence = plan
            .events
            .iter()
            .position(|event| event.kind == "repetition_occurrence_completed")
            .unwrap();
        assert!(terminal < child && child < occurrence);
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged
            .repetition_occurrences
            .iter_mut()
            .find(|row| row.status == ProcessRepetitionOccurrenceStatus::Completed)
            .unwrap()
            .accepted_source_event_id = Some(Uuid::new_v4().to_string());
        assert!(repository::acknowledge_manual_task(
            &fixture.db,
            &fixture.owner,
            &command,
            &started.instance_id,
            &task.user_task_id,
            snapshot.instance.revision,
            ProcessPlanInput::Supplied(&forged),
            at
        )
        .is_err());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before
        );
        if ordinal == 1 {
            let sibling = snapshot
                .scopes
                .iter()
                .find(|scope| {
                    scope.scope_id != task.scope_id
                        && scope.parent_scope_id.as_deref() == Some(started.instance_id.as_str())
                        && scope.status == ProcessInstanceStatus::Completed
                })
                .unwrap();
            let sibling_completion = repository::list_events(
                &fixture.db, &fixture.owner, &started.instance_id, 0, 200,
            )
            .unwrap()
            .0
            .into_iter()
            .find(|event| event.kind == "scope_completed" && event.scope_id == sibling.scope_id)
            .unwrap();
            let mut wrong_source = plan.clone();
            wrong_source.events[child].data["source_event_id"] =
                sibling_completion.data["source_event_id"].clone();
            let mut wrong_parent = plan.clone();
            wrong_parent.events[child].data["parent_token_id"] =
                json!(sibling.parent_token_id.as_deref().unwrap());
            let mut wrong_locals = plan.clone();
            wrong_locals.scope_updates.iter_mut().find(|row|
                row.scope_id == task.scope_id && row.status == ProcessInstanceStatus::Completed)
                .unwrap().variables = Some(json!({"forged": true}));
            let mut wrong_aggregate = plan.clone();
            wrong_aggregate.repetition_occurrences.iter_mut().find(|row|
                row.status == ProcessRepetitionOccurrenceStatus::Completed)
                .unwrap().aggregate_item = Some(json!({"forged": true}));
            for (case, forged) in [
                ("sibling termination source", wrong_source),
                ("sibling child parent token", wrong_parent),
                ("forged child locals", wrong_locals),
                ("forged ordinal aggregate", wrong_aggregate),
            ] {
                let error = repository::acknowledge_manual_task(
                    &fixture.db,
                    &fixture.owner,
                    &command,
                    &started.instance_id,
                    &task.user_task_id,
                    snapshot.instance.revision,
                    ProcessPlanInput::Supplied(&forged),
                    at,
                )
                .unwrap_err();
                assert!(!format!("{error:#}").is_empty(), "{case} lacked a rejection reason");
                assert_eq!(
                    super::signal_proof_tests::all_transition_rows(&fixture),
                    before,
                    "{case} changed durable process rows"
                );
            }
        }
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        repository::acknowledge_manual_task(
            &reopened,
            &fixture.owner,
            &command,
            &started.instance_id,
            &task.user_task_id,
            snapshot.instance.revision,
            ProcessPlanInput::Supplied(&plan),
            at,
        )
        .unwrap();
        let after =
            repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(after.repetition_groups[0].completed_count, ordinal + 1);
        assert_eq!(
            after.instance.status,
            if ordinal == 0 {
                ProcessInstanceStatus::Waiting
            } else {
                ProcessInstanceStatus::Completed
            }
        );
        assert_eq!(
            after
                .timers
                .iter()
                .filter(|timer| timer.node_id == "ScopeDeadline"
                    && timer.status == ProcessTimerStatus::Pending)
                .count(),
            usize::from(ordinal == 0)
        );
    }
}

#[test]
fn repeated_pending_send_uses_each_frozen_item_in_its_real_worker_admission() {
    use tentaflow_protocol::processes::ProcessMessageTargetSpec;

    let fixture = Fixture::new();
    let receiver_model = super::send_receive_tests::receive_model();
    let receiver_version = publish_model(&fixture, &receiver_model);
    let receiver_a = start_version(&fixture, &receiver_version);
    let receiver_b = start_version(&fixture, &receiver_version);
    let mut model = super::send_boundary_tests::timer_bound_send_model(
        &receiver_version.definition_id,
        &receiver_a.instance_id,
    );
    model.variables.insert(
        "items".into(),
        json!([
            {"instance_id":receiver_a.instance_id,"label":"alpha"},
            {"instance_id":receiver_b.instance_id,"label":"beta"},
        ]),
    );
    let send = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Send_1")
        .unwrap();
    send.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::SendTask {
        target,
        correlation_expression,
        payload_expression,
        ..
    } = &mut send.kind
    else {
        panic!("pinned SendTask")
    };
    *target = ProcessMessageTargetSpec::Catch {
        definition_id: receiver_version.definition_id.clone(),
        instance_id_expression: Some("repeat.item.instance_id".into()),
        subscription_id_expression: None,
    };
    *correlation_expression = "'case-1'".into();
    *payload_expression = "{'label': repeat.item.label, 'ordinal': repeat.index}".into();
    model.variables.insert("results".into(), json!([]));
    let version = publish_model(&fixture, &model);
    let sender = start_version(&fixture, &version);
    let initial =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &sender.instance_id).unwrap();
    assert_eq!(initial.repetition_occurrences.len(), 2);
    assert_eq!(
        initial
            .timers
            .iter()
            .filter(
                |timer| timer.node_id == "SendTimer" && timer.status == ProcessTimerStatus::Pending
            )
            .count(),
        1
    );
    for (ordinal, target, label) in [
        (0_u32, receiver_a.instance_id.as_str(), "alpha"),
        (1_u32, receiver_b.instance_id.as_str(), "beta"),
    ] {
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &sender.instance_id).unwrap();
        let occurrence = snapshot
            .repetition_occurrences
            .iter()
            .find(|row| row.ordinal == ordinal)
            .unwrap();
        let waiting = snapshot
            .tokens
            .iter()
            .find(|token| token.token_id == occurrence.token_id && token.status == "waiting")
            .unwrap();
        let pending_event =
            repository::list_events(&fixture.db, &fixture.owner, &sender.instance_id, 0, 200)
                .unwrap()
                .0
                .into_iter()
                .find(|event| {
                    event.kind == "send_task_pending"
                        && event.data["pending_token_id"] == waiting.token_id
                })
                .unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let entry = repository::AcceptedInputRef::SendAdmission {
            instance_id: sender.instance_id.clone(),
            scope_id: waiting.scope_id.clone(),
            node_id: waiting.node_id.clone(),
            pending_token_id: waiting.token_id.clone(),
            pending_event_id: pending_event.event_id.clone(),
            expected_instance_revision: snapshot.instance.revision,
        };
        let plan = runtime::plan_send_admission(
            &snapshot,
            &waiting.token_id,
            &pending_event.event_id,
            at_ms,
            entry,
            None,
            None,
        )
        .unwrap();
        assert_eq!(plan.create_messages.len(), 1);
        assert_eq!(
            plan.create_messages[0].message.payload,
            json!({"label":label,"ordinal":ordinal})
        );
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged.create_messages[0].message.payload = json!({"label":"forged"});
        assert!(repository::admit_pending_send(
            &fixture.db,
            &waiting.token_id,
            at_ms,
            ProcessPlanInput::Supplied(&forged)
        )
        .is_err());
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before
        );
        let sibling = snapshot
            .repetition_occurrences
            .iter()
            .find(|row| row.ordinal != ordinal)
            .unwrap();
        let sibling_waiting = snapshot.tokens.iter().any(|token|
            token.token_id == sibling.token_id && token.status == "waiting");
        if ordinal == 0 {
            assert_eq!(sibling.status, ProcessRepetitionOccurrenceStatus::Active);
            assert!(sibling_waiting, "the other real ordinal must still be waiting");
        } else {
            assert_eq!(sibling.status, ProcessRepetitionOccurrenceStatus::Completed);
            assert!(!sibling_waiting, "the completed ordinal cannot remain waiting");
            let source_id = sibling.accepted_source_event_id.as_deref().unwrap();
            let history = repository::list_events(
                &fixture.db, &fixture.owner, &sender.instance_id, 0, 200,
            ).unwrap().0;
            assert_eq!(history.iter().filter(|event|
                event.kind == "send_task_admitted"
                    && event.event_id == source_id
                    && event.data["source_activation_id"] == sibling.token_id
            ).count(), 1);
            assert_eq!(history.iter().filter(|event|
                event.kind == "repetition_occurrence_completed"
                    && event.data["occurrence_id"] == sibling.occurrence_id
                    && event.data["source_event_id"] == source_id
            ).count(), 1);
        }
        let coordinator = &snapshot.repetition_groups[0].parent_token_id;
        assert!(snapshot.tokens.iter().any(|token|
            token.token_id.as_str() == coordinator.as_str() && token.status == "waiting"));
        let admission = plan.events.iter().position(|event|
            event.kind == "send_task_admitted").unwrap();
        for (source, source_id) in [
            ("coordinator", coordinator.as_str()),
            ("sibling ordinal", sibling.token_id.as_str()),
        ] {
            assert_ne!(source_id, waiting.token_id.as_str());
            let mut wrong_outbox = plan.clone();
            wrong_outbox.create_messages[0].source_activation_id = source_id.to_owned();
            let mut wrong_event = plan.clone();
            wrong_event.events[admission].data["source_activation_id"] = json!(source_id);
            for (field, forged) in [
                ("outbox source", wrong_outbox),
                ("admission event source", wrong_event),
            ] {
                let error = repository::admit_pending_send(
                    &fixture.db,
                    &waiting.token_id,
                    at_ms,
                    ProcessPlanInput::Supplied(&forged),
                )
                .unwrap_err();
                assert!(
                    !format!("{error:#}").is_empty(),
                    "{source} {field} lacked a rejection reason"
                );
                assert_eq!(
                    super::signal_proof_tests::all_transition_rows(&fixture),
                    before,
                    "{source} {field} changed durable process rows"
                );
            }
        }
        repository::admit_pending_send(
            &fixture.db,
            &waiting.token_id,
            at_ms,
            ProcessPlanInput::Canonical,
        )
        .unwrap()
        .unwrap();
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &sender.instance_id, 0, 200)
                .unwrap()
                .0;
        let admitted = history
            .iter()
            .find(|event| {
                event.kind == "send_task_admitted"
                    && event.data["source_activation_id"] == waiting.token_id
            })
            .unwrap();
        let message = repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            admitted.data["message_id"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            message.payload,
            Some(json!({"label":label,"ordinal":ordinal}))
        );
        assert!(matches!(&message.message.target,
            tentaflow_protocol::processes::ProcessMessageTarget::Catch {
                instance_id: Some(id), .. } if id == target));
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert!(repository::admit_pending_send(
            &reopened,
            &waiting.token_id,
            at_ms,
            ProcessPlanInput::Canonical
        )
        .unwrap()
        .is_none());
    }
    let completed =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &sender.instance_id).unwrap();
    assert_eq!(completed.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        completed.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Completed
    );
    assert!(
        completed
            .timers
            .iter()
            .any(|timer| timer.node_id == "SendTimer"
                && timer.status == ProcessTimerStatus::Cancelled)
    );
}

#[tokio::test]
async fn repeated_service_error_interrupts_both_ordinals_from_its_fenced_result() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"Error","code":"REJECTED",
        "summary":"Actual business rejection","outputs":{"case":"alpha"},
        "evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[
        {"id":"trigger","type":"trigger","config":{
            "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output",
            "from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]})
    .to_string();
    let flow_id = runtime::test_support::flow(&fixture.db, &fixture.owner, &graph);
    let mut model = super::repetition_service_tests::repeated_service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let service = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ServiceTask {
        result_expression, ..
    } = &mut service.kind
    else {
        panic!("pinned repeated ServiceTask")
    };
    *result_expression = Some("outputs.variables.actual_result".into());
    model.errors.push(ProcessErrorDeclaration {
        error_id: "BusinessError".into(),
        name: "Business rejection".into(),
        error_code: "REJECTED".into(),
    });
    model.nodes.extend([
        ProcessNode {
            id: "HandleError".into(),
            name: "Handle error".into(),
            kind: ProcessNodeKind::BoundaryError {
                attached_to_id: "Service".into(),
                error_ref: Some("BusinessError".into()),
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "ReviewError".into(),
            name: "Review error".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
    ]);
    model.sequence_flows.extend([
        edge("ErrorToReview", "HandleError", "ReviewError"),
        edge("ReviewToEnd", "ReviewError", "End_1"),
    ]);
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let at = chrono::Utc::now().timestamp_millis();
    let first = repository::claim_job(&fixture.db, "first-error-ordinal", at)
        .unwrap()
        .unwrap();
    let second = repository::claim_job(&fixture.db, "second-error-ordinal", at)
        .unwrap()
        .unwrap();
    assert_ne!(first.job.job_id, second.job.job_id);
    let observed = super::repetition_service_tests::observed_flow_result(&fixture, &first).await;
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let job = snapshot
        .jobs
        .iter()
        .find(|job| job.job_id == first.job.job_id)
        .unwrap();
    let plan = runtime::plan_job_result(&snapshot, job, &observed, at, None).unwrap();
    assert!(plan
        .events
        .iter()
        .any(|event| event.kind == "business_error_caught"));
    assert!(plan.cancel_job_ids.contains(&second.job.job_id));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("foreign attached coordinator", {
            let mut forged = plan.clone();
            forged
                .events
                .iter_mut()
                .find(|event| event.kind == "business_error_caught")
                .unwrap()
                .data["attached_token_id"] = json!(Uuid::new_v4().to_string());
            forged
        }),
        ("missing sibling cancellation", {
            let mut forged = plan.clone();
            forged.cancel_job_ids.retain(|id| id != &second.job.job_id);
            forged
        }),
        ("coordinator not closed", {
            let mut forged = plan.clone();
            forged
                .repetition_groups
                .iter_mut()
                .find(|group| group.status == ProcessRepetitionGroupStatus::Cancelled)
                .unwrap()
                .status = ProcessRepetitionGroupStatus::Open;
            forged
        }),
    ] {
        let error = repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &first.job.job_id,
            first.job.attempt,
            first.job.fence,
            "first-error-ordinal",
            &observed,
            snapshot.instance.revision,
            ProcessPlanInput::Supplied(&forged),
            at,
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let accepted = repository::accept_job_result(
        &reopened,
        &fixture.owner,
        &first.job.job_id,
        first.job.attempt,
        first.job.fence,
        "first-error-ordinal",
        &observed,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    let final_state =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        final_state.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert_eq!(
        final_state
            .repetition_occurrences
            .iter()
            .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Cancelled)
            .count(),
        2
    );
    assert!(final_state
        .user_tasks
        .iter()
        .any(|task| task.node_id == "ReviewError" && task.status == ProcessUserTaskStatus::Open));
    let after = super::signal_proof_tests::all_transition_rows(&fixture);
    let replayed = repository::accept_job_result(
        &reopened,
        &fixture.owner,
        &first.job.job_id,
        first.job.attempt,
        first.job.fence,
        "first-error-ordinal",
        &observed,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&plan),
        at
    )
    .unwrap();
    assert_eq!(replayed.instance.instance_id, accepted.instance.instance_id);
    assert_eq!(replayed.instance.revision, accepted.instance.revision);
    assert_eq!(replayed.instance.status, accepted.instance.status);
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        after
    );
    let mut conflicting = observed.clone();
    conflicting.result.summary.push_str(" changed");
    assert!(repository::accept_job_result(
        &reopened, &fixture.owner, &first.job.job_id, first.job.attempt,
        first.job.fence, "first-error-ordinal", &conflicting,
        snapshot.instance.revision, ProcessPlanInput::Supplied(&plan), at,
    ).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), after);
}

#[tokio::test]
async fn repeated_service_contract_error_maps_outer_handler_from_fenced_ordinal_once() {
    for mapped in [false, true] {
        let fixture = Fixture::new();
        let business = json!({"outcome":"Error","code":"REJECTED",
            "summary":"Actual business rejection","outputs":{"case":"alpha"},
            "evidence":["registered_flow_result"]});
        let graph = json!({"nodes":[
            {"id":"trigger","type":"trigger","config":{
                "output_mapping":{"actual_result":business.to_string()}}},
            {"id":"output","type":"output","config":{}}],
            "edges":[{"from":"trigger","to":"output",
                "from_port":"text","to_port":"text"}],
            "variables":[{"name":"actual_result","type":"json"}]})
            .to_string();
        let flow_id = runtime::test_support::flow(&fixture.db, &fixture.owner, &graph);
        let mut model = super::repetition_service_tests::repeated_service_model(
            &flow_id, ActivityVerification::Condition { expression: "true".into() });
        model.variables.insert("caught_case".into(), serde_json::Value::Null);
        let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
        service.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
        let ProcessNodeKind::ServiceTask { result_expression, .. } = &mut service.kind else {
            panic!("pinned repeated ServiceTask")
        };
        *result_expression = Some("outputs.variables.actual_result".into());
        model.errors.push(ProcessErrorDeclaration {
            error_id: "BusinessError".into(),
            name: "Business rejection".into(),
            error_code: "REJECTED".into(),
        });
        model.nodes.extend([
            ProcessNode {
                id: "HandleError".into(), name: "Handle error".into(),
                kind: ProcessNodeKind::BoundaryError {
                    attached_to_id: "Service".into(),
                    error_ref: Some("BusinessError".into()),
                    output_mapping: if mapped {
                        BTreeMap::from([("caught_case".into(), "outputs.case".into())])
                    } else { BTreeMap::new() },
                }, repeat: None, activity_io: None,
            },
            ProcessNode {
                id: "ReviewError".into(), name: "Review error".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None, output_mapping: BTreeMap::new(),
                }, repeat: None, activity_io: None,
            },
        ]);
        model.sequence_flows.extend([
            edge("ErrorToReview", "HandleError", "ReviewError"),
            edge("ReviewToEnd", "ReviewError", "End_1"),
        ]);
        let version = publish_model(&fixture, &model);
        let started = start_version(&fixture, &version);
        let at = chrono::Utc::now().timestamp_millis();
        let worker = if mapped { "mapped-error-worker" } else { "empty-error-worker" };
        let first = repository::claim_job(&fixture.db, worker, at).unwrap().unwrap();
        let second = repository::claim_job(&fixture.db, "sibling-error-worker", at)
            .unwrap().unwrap();
        let observed = super::repetition_service_tests::observed_flow_result(&fixture, &first).await;
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let job = snapshot.jobs.iter().find(|job| job.job_id == first.job.job_id).unwrap();
        let plan = runtime::plan_job_result(&snapshot, job, &observed, at, None).unwrap();
        let coordinator = &snapshot.repetition_groups[0].parent_token_id;
        let effect = plan.variable_effects.iter().find(|effect| matches!(effect,
            repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleError"));
        let Some(repository::VariableEffect::Mapped {
            source_token_id, outputs, result, extra, ..
        }) = effect else { panic!("outer Error handler lacks its factual mapped effect") };
        assert_eq!(source_token_id, coordinator);
        assert_eq!(outputs, &observed.result.outputs);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "activity_result");
        if mapped {
            assert_eq!(result["caught_case"], "alpha");
        } else {
            assert_eq!(result, &snapshot.instance.variables);
        }
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged_handler = plan.clone();
        forged_handler.events.iter_mut().find(|event|
            event.kind == "business_error_caught").unwrap().data["subscription_id"] =
            json!(Uuid::new_v4().to_string());
        let mut forged_group = plan.clone();
        forged_group.repetition_groups.iter_mut().find(|group|
            group.status == ProcessRepetitionGroupStatus::Cancelled).unwrap().group_id =
            Uuid::new_v4().to_string();
        let mut forged_effect_source = plan.clone();
        let mut forged_effect_job = plan.clone();
        if mapped {
            if let Some(repository::VariableEffect::Mapped { source_token_id, .. }) =
                forged_effect_source.variable_effects.iter_mut().find(|effect| matches!(effect,
                    repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleError")) {
                *source_token_id = second.job.token_id.clone();
            }
            if let Some(repository::VariableEffect::Mapped {
                accepted_input: Some(repository::AcceptedInputRef::Service { job_id, .. }), ..
            }) = forged_effect_job.variable_effects.iter_mut().find(|effect| matches!(effect,
                repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleError")) {
                *job_id = second.job.job_id.clone();
            }
        }
        let mut forged_plans = vec![("handler", forged_handler), ("group", forged_group)];
        if mapped {
            forged_plans.extend([("effect source", forged_effect_source),
                ("effect job", forged_effect_job)]);
        }
        for (case, forged) in forged_plans {
            assert!(repository::accept_job_result(&fixture.db, &fixture.owner,
                &first.job.job_id, first.job.attempt, first.job.fence, worker,
                &observed, snapshot.instance.revision,
                ProcessPlanInput::Supplied(&forged), at).is_err(),
                "forged {case} was accepted");
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
                "forged {case} changed durable process rows");
        }
        repository::accept_job_result(&fixture.db, &fixture.owner,
            &first.job.job_id, first.job.attempt, first.job.fence, worker,
            &observed, snapshot.instance.revision,
            ProcessPlanInput::Supplied(&plan), at).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let after = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(after.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
        assert_eq!(after.jobs.iter().find(|job| job.job_id == first.job.job_id).unwrap().status,
            "error");
        assert_eq!(after.jobs.iter().find(|job| job.job_id == second.job.job_id).unwrap().status,
            "cancelled");
        assert!(after.user_tasks.iter().any(|task| task.node_id == "ReviewError"
            && task.status == ProcessUserTaskStatus::Open));
        let actual = repository::get_instance(&reopened, &fixture.owner,
            &started.instance_id, None).unwrap();
        assert_eq!(actual.variables["caught_case"], if mapped { json!("alpha") }
            else { serde_json::Value::Null });
        let committed = super::signal_proof_tests::all_transition_rows(&fixture);
        repository::accept_job_result(&reopened, &fixture.owner,
            &first.job.job_id, first.job.attempt, first.job.fence, worker,
            &observed, snapshot.instance.revision, ProcessPlanInput::Supplied(&plan), at)
            .unwrap();
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
    }
}

#[tokio::test]
async fn repeated_service_escalation_binds_ordinal_source_to_outer_handler() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"NeedsHuman","code":"REVIEW",
        "summary":"Actual review request","outputs":{"case":"alpha"},
        "evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[
        {"id":"trigger","type":"trigger","config":{
            "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output",
            "from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]})
    .to_string();
    let flow_id = runtime::test_support::flow(&fixture.db, &fixture.owner, &graph);
    let mut model = super::repetition_service_tests::repeated_service_model(
        &flow_id,
        ActivityVerification::Human,
    );
    let service = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ServiceTask {
        result_expression, ..
    } = &mut service.kind
    else {
        panic!("pinned repeated ServiceTask")
    };
    *result_expression = Some("outputs.variables.actual_result".into());
    model.escalations.push(ProcessEscalationDeclaration {
        escalation_id: "HumanReview".into(),
        name: "Human review".into(),
        escalation_code: "REVIEW".into(),
    });
    model.nodes.extend([
        ProcessNode {
            id: "HandleEscalation".into(),
            name: "Handle escalation".into(),
            kind: ProcessNodeKind::BoundaryEscalation {
                attached_to_id: "Service".into(),
                escalation_ref: Some("HumanReview".into()),
                cancel_activity: true,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "ReviewEscalation".into(),
            name: "Review escalation".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
    ]);
    model.sequence_flows.extend([
        edge("EscalationToReview", "HandleEscalation", "ReviewEscalation"),
        edge("EscalationReviewToEnd", "ReviewEscalation", "End_1"),
    ]);
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let at = chrono::Utc::now().timestamp_millis();
    let claim = repository::claim_job(&fixture.db, "escalating-ordinal", at)
        .unwrap()
        .unwrap();
    let sibling = repository::claim_job(&fixture.db, "other-ordinal", at)
        .unwrap()
        .unwrap();
    let observed = super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let job = snapshot
        .jobs
        .iter()
        .find(|job| job.job_id == claim.job.job_id)
        .unwrap();
    let plan = runtime::plan_job_result(&snapshot, job, &observed, at, None).unwrap();
    let source = snapshot
        .repetition_occurrences
        .iter()
        .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str()))
        .unwrap();
    let group = snapshot
        .repetition_groups
        .iter()
        .find(|row| row.group_id == source.group_id)
        .unwrap();
    let caught = plan
        .events
        .iter()
        .find(|event| event.kind == "escalation_caught")
        .unwrap();
    assert_eq!(caught.data["source_token_id"], source.token_id);
    assert_eq!(caught.data["attached_token_id"], group.parent_token_id);
    assert!(plan.cancel_job_ids.contains(&sibling.job.job_id));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("foreign ordinal source", {
            let mut forged = plan.clone();
            forged
                .events
                .iter_mut()
                .find(|event| event.kind == "escalation_caught")
                .unwrap()
                .data["source_token_id"] = json!(Uuid::new_v4().to_string());
            forged
        }),
        ("foreign coordinator", {
            let mut forged = plan.clone();
            forged
                .events
                .iter_mut()
                .find(|event| event.kind == "escalation_caught")
                .unwrap()
                .data["attached_token_id"] = json!(Uuid::new_v4().to_string());
            forged
        }),
        ("missing sibling job closure", {
            let mut forged = plan.clone();
            forged.cancel_job_ids.retain(|id| id != &sibling.job.job_id);
            forged
        }),
    ] {
        let error = repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "escalating-ordinal",
            &observed,
            snapshot.instance.revision,
            ProcessPlanInput::Supplied(&forged),
            at,
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").is_empty(),
            "{case} lacked a rejection reason"
        );
        assert_eq!(
            super::signal_proof_tests::all_transition_rows(&fixture),
            before,
            "{case} changed durable process rows"
        );
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::accept_job_result(
        &reopened,
        &fixture.owner,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "escalating-ordinal",
        &observed,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    let final_state =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        final_state.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Cancelled
    );
    assert!(final_state.user_tasks.iter().any(
        |task| task.node_id == "ReviewEscalation" && task.status == ProcessUserTaskStatus::Open
    ));
    let history = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 200)
        .unwrap()
        .0;
    assert_eq!(
        history
            .iter()
            .filter(|event| event.kind == "escalation_caught")
            .count(),
        1
    );
}

#[tokio::test]
async fn repeated_needs_human_maps_outer_escalation_from_fenced_ordinal_once() {
    for mapped in [false, true] {
        let fixture = Fixture::new();
        let business = json!({"outcome":"NeedsHuman","code":"REVIEW",
            "summary":"Review the actual result","outputs":{"case":"alpha"},
            "evidence":["registered_flow_result"]});
        let graph = json!({"nodes":[
            {"id":"trigger","type":"trigger","config":{
                "output_mapping":{"actual_result":business.to_string()}}},
            {"id":"output","type":"output","config":{}}],
            "edges":[{"from":"trigger","to":"output",
                "from_port":"text","to_port":"text"}],
            "variables":[{"name":"actual_result","type":"json"}]})
            .to_string();
        let flow_id = runtime::test_support::flow(&fixture.db, &fixture.owner, &graph);
        let mut model = super::repetition_service_tests::repeated_service_model(
            &flow_id, ActivityVerification::Human);
        model.variables.insert("caught_case".into(), serde_json::Value::Null);
        let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
        service.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::CollectionExpression {
                expression: "vars.items".into(),
            },
            output_collection_variable: "results".into(),
        });
        let ProcessNodeKind::ServiceTask { result_expression, .. } = &mut service.kind else {
            panic!("pinned repeated ServiceTask")
        };
        *result_expression = Some("outputs.variables.actual_result".into());
        model.escalations.push(ProcessEscalationDeclaration {
            escalation_id: "HumanReview".into(), name: "Human review".into(),
            escalation_code: "REVIEW".into(),
        });
        model.nodes.extend([
            ProcessNode {
                id: "HandleEscalation".into(), name: "Handle escalation".into(),
                kind: ProcessNodeKind::BoundaryEscalation {
                    attached_to_id: "Service".into(),
                    escalation_ref: Some("HumanReview".into()),
                    cancel_activity: true,
                    output_mapping: if mapped {
                        BTreeMap::from([("caught_case".into(), "outputs.case".into())])
                    } else { BTreeMap::new() },
                }, repeat: None, activity_io: None,
            },
            ProcessNode {
                id: "ReviewEscalation".into(), name: "Review escalation".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None, output_mapping: BTreeMap::new(),
                }, repeat: None, activity_io: None,
            },
        ]);
        model.sequence_flows.extend([
            edge("EscalationToReview", "HandleEscalation", "ReviewEscalation"),
            edge("EscalationReviewToEnd", "ReviewEscalation", "End_1"),
        ]);
        let version = publish_model(&fixture, &model);
        let started = start_version(&fixture, &version);
        let at = chrono::Utc::now().timestamp_millis();
        let worker = if mapped { "mapped-review-worker" } else { "empty-review-worker" };
        let claim = repository::claim_job(&fixture.db, worker, at).unwrap().unwrap();
        let sibling = repository::claim_job(&fixture.db, "sibling-review-worker", at)
            .unwrap().unwrap();
        let observed = super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let job = snapshot.jobs.iter().find(|job| job.job_id == claim.job.job_id).unwrap();
        let plan = runtime::plan_job_result(&snapshot, job, &observed, at, None).unwrap();
        let coordinator = &snapshot.repetition_groups[0].parent_token_id;
        let caught = plan.events.iter().find(|event| event.kind == "escalation_caught").unwrap();
        assert_eq!(caught.data["source_token_id"], claim.job.token_id);
        assert_eq!(caught.data["attached_token_id"], *coordinator);
        let effect = plan.variable_effects.iter().find(|effect| matches!(effect,
            repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleEscalation"));
        let Some(repository::VariableEffect::Mapped {
            source_token_id, outputs, result, extra, ..
        }) = effect else { panic!("outer Escalation handler lacks its factual mapped effect") };
        assert_eq!(source_token_id, coordinator);
        assert_eq!(outputs, &observed.result.outputs);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "activity_result");
        if mapped {
            assert_eq!(result["caught_case"], "alpha");
        } else {
            assert_eq!(result, &snapshot.instance.variables);
        }
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged_handler = plan.clone();
        forged_handler.events.iter_mut().find(|event|
            event.kind == "escalation_caught").unwrap().data["subscription_id"] =
            json!(Uuid::new_v4().to_string());
        let mut forged_group = plan.clone();
        forged_group.repetition_groups.iter_mut().find(|group|
            group.status == ProcessRepetitionGroupStatus::Cancelled).unwrap().group_id =
            Uuid::new_v4().to_string();
        let mut forged_effect_source = plan.clone();
        let mut forged_effect_job = plan.clone();
        if mapped {
            if let Some(repository::VariableEffect::Mapped { source_token_id, .. }) =
                forged_effect_source.variable_effects.iter_mut().find(|effect| matches!(effect,
                    repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleEscalation")) {
                *source_token_id = sibling.job.token_id.clone();
            }
            if let Some(repository::VariableEffect::Mapped {
                accepted_input: Some(repository::AcceptedInputRef::Service { job_id, .. }), ..
            }) = forged_effect_job.variable_effects.iter_mut().find(|effect| matches!(effect,
                repository::VariableEffect::Mapped { node_id, .. } if node_id == "HandleEscalation")) {
                *job_id = sibling.job.job_id.clone();
            }
        }
        let mut forged_plans = vec![("handler", forged_handler), ("group", forged_group)];
        if mapped { forged_plans.extend([("effect source", forged_effect_source),
            ("effect job", forged_effect_job)]); }
        for (case, forged) in forged_plans {
            assert!(repository::accept_job_result(&fixture.db, &fixture.owner,
                &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
                &observed, snapshot.instance.revision,
                ProcessPlanInput::Supplied(&forged), at).is_err(),
                "forged {case} was accepted");
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
                "forged {case} changed durable process rows");
        }
        repository::accept_job_result(&fixture.db, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
            &observed, snapshot.instance.revision,
            ProcessPlanInput::Supplied(&plan), at).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let after = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(after.repetition_groups[0].status, ProcessRepetitionGroupStatus::Cancelled);
        assert_eq!(after.jobs.iter().find(|job| job.job_id == claim.job.job_id).unwrap().status,
            "completed");
        assert_eq!(after.jobs.iter().find(|job| job.job_id == sibling.job.job_id).unwrap().status,
            "cancelled");
        assert!(after.user_tasks.iter().any(|task| task.node_id == "ReviewEscalation"
            && task.status == ProcessUserTaskStatus::Open));
        let actual = repository::get_instance(&reopened, &fixture.owner,
            &started.instance_id, None).unwrap();
        assert_eq!(actual.variables["caught_case"], if mapped { json!("alpha") }
            else { serde_json::Value::Null });
        let committed = super::signal_proof_tests::all_transition_rows(&fixture);
        repository::accept_job_result(&reopened, &fixture.owner,
            &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
            &observed, snapshot.instance.revision, ProcessPlanInput::Supplied(&plan), at)
            .unwrap();
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
    }
}

#[tokio::test]
async fn noninterrupting_outer_escalation_keeps_both_ordinals_and_factual_verification() {
    let fixture = Fixture::new();
    let business = json!({"outcome":"NeedsHuman","code":"REVIEW",
        "summary":"Review one ordinal","outputs":{"case":"alpha"},
        "evidence":["registered_flow_result"]});
    let graph = json!({"nodes":[
        {"id":"trigger","type":"trigger","config":{
            "output_mapping":{"actual_result":business.to_string()}}},
        {"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output",
            "from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"}]})
    .to_string();
    let flow_id = runtime::test_support::flow(&fixture.db, &fixture.owner, &graph);
    let mut model = super::repetition_service_tests::repeated_service_model(
        &flow_id,
        ActivityVerification::Human,
    );
    let service = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ServiceTask {
        result_expression, ..
    } = &mut service.kind
    else {
        panic!("pinned repeated ServiceTask")
    };
    *result_expression = Some("outputs.variables.actual_result".into());
    model.escalations.push(ProcessEscalationDeclaration {
        escalation_id: "HumanReview".into(),
        name: "Human review".into(),
        escalation_code: "REVIEW".into(),
    });
    model.nodes.extend([
        ProcessNode {
            id: "ObserveEscalation".into(),
            name: "Observe escalation".into(),
            kind: ProcessNodeKind::BoundaryEscalation {
                attached_to_id: "Service".into(),
                escalation_ref: Some("HumanReview".into()),
                cancel_activity: false,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "ObserveWork".into(),
            name: "Observe work".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
    ]);
    model.sequence_flows.extend([
        edge("EscalationObserve", "ObserveEscalation", "ObserveWork"),
        edge("ObserveToEnd", "ObserveWork", "End_1"),
    ]);
    let version = publish_model(&fixture, &model);
    let started = start_version(&fixture, &version);
    let at = chrono::Utc::now().timestamp_millis();
    let claim = repository::claim_job(&fixture.db, "observed-ordinal", at)
        .unwrap()
        .unwrap();
    let other = repository::claim_job(&fixture.db, "other-ordinal", at)
        .unwrap()
        .unwrap();
    let observed = super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let job = snapshot
        .jobs
        .iter()
        .find(|job| job.job_id == claim.job.job_id)
        .unwrap();
    let plan = runtime::plan_job_result(&snapshot, job, &observed, at, None).unwrap();
    assert!(plan.cancel_token_ids.is_empty());
    assert!(plan.cancel_job_ids.is_empty());
    assert!(plan
        .create_user_tasks
        .iter()
        .any(|task| task.node_id == "Service"
            && task.kind == tentaflow_protocol::processes::ProcessUserTaskKind::Verification));
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged
        .repetition_occurrences
        .iter_mut()
        .find(|row| row.job_id.as_deref() == Some(claim.job.job_id.as_str()))
        .unwrap()
        .accepted_source_event_id = Some(Uuid::new_v4().to_string());
    assert!(repository::accept_job_result(
        &fixture.db,
        &fixture.owner,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "observed-ordinal",
        &observed,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&forged),
        at
    )
    .is_err());
    assert_eq!(
        super::signal_proof_tests::all_transition_rows(&fixture),
        before
    );
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::accept_job_result(
        &reopened,
        &fixture.owner,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "observed-ordinal",
        &observed,
        snapshot.instance.revision,
        ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    let after =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        after.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Open
    );
    assert_eq!(
        after
            .repetition_occurrences
            .iter()
            .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::AwaitingVerification)
            .count(),
        1
    );
    assert!(after
        .jobs
        .iter()
        .any(|job| job.job_id == other.job.job_id && job.status == "running"));
    assert!(after
        .user_tasks
        .iter()
        .any(|task| task.node_id == "ObserveWork" && task.status == ProcessUserTaskStatus::Open));
    assert!(after.user_tasks.iter().any(|task| task.node_id == "Service"
        && task.kind == tentaflow_protocol::processes::ProcessUserTaskKind::Verification
        && task.status == ProcessUserTaskStatus::Open));
    assert_eq!(
        after
            .subscriptions
            .iter()
            .filter(|sub| sub.node_id == "ObserveEscalation"
                && sub.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed)
            .count(),
        1
    );
}
