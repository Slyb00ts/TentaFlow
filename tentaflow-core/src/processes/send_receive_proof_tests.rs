// ============ File: send_receive_proof_tests.rs — Full-table provenance controls for durable SendTask and ReceiveTask ============

use super::call_tests::transition_rows;
use super::messages::{self, test_support::*};
use super::repository::{self, MessageSelection, TerminationAttempt, VariableEffect};
use super::runtime::{self, test_support::*};
use serde_json::json;
use tentaflow_protocol::processes::{ProcessInstanceStatus, ProcessNodeKind,
    ProcessSubscriptionKind, ProcessSubscriptionStatus};
use uuid::Uuid;

#[test]
fn send_task_rejects_omitted_or_forged_admission_before_canonical_commit() {
    let fixture = Fixture::new();
    let receiving = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = start_version(&fixture, &receiving);
    let source = publish_model(&fixture,
        &super::send_receive_tests::send_model(&receiving.definition_id, &receiver.instance_id));
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("send-source-proof");
    let vars = serde_json::to_value(&source.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&source.model, &source.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&source.model), &instance_id, &fixture.owner,
        &source.definition_id, source.version, vars.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.create_messages.len(), 1);
    let before = transition_rows(&fixture);
    let mut omitted = plan.clone();
    omitted.events.iter_mut().find(|event| event.kind == "send_task_admitted")
        .unwrap().kind = "node_completed".into();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&omitted), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut forged = plan.clone();
    forged.create_messages[0].message.payload = json!({"value":43});
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_target = plan.clone();
    wrong_target.create_messages[0].message.target = catch_target(&receiving,
        Some(&Uuid::new_v4().to_string()), Some(&receiver.subscriptions[0].subscription_id));
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&wrong_target), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_ttl = plan.clone();
    wrong_ttl.create_messages[0].message.ttl_seconds += 1;
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&wrong_ttl), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_event = plan.clone();
    wrong_event.create_messages[0].source_event_index = plan.events.len() - 1;
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&wrong_event), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign = plan.clone();
    foreign.create_messages[0].source_activation_id = Uuid::new_v4().to_string();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&foreign), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut duplicate = plan.clone();
    duplicate.events.push(plan.events.iter().find(|event| event.kind == "send_task_admitted")
        .unwrap().clone());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&duplicate), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut extra_successor = plan.clone();
    let direct = plan.create_tokens.iter().find(|token|
        token.node_id == "End_1").unwrap().clone();
    let source_id = plan.token_sources.get(&direct.token_id).unwrap().clone();
    let mut extra = direct;
    extra.token_id = Uuid::new_v4().to_string();
    extra_successor.token_sources.insert(extra.token_id.clone(), source_id);
    extra_successor.create_tokens.push(extra);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&extra_successor), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &source.definition_id, source.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_ne!(transition_rows(&fixture), before);
}

#[test]
fn receive_or_its_message_boundary_requires_exact_own_subscription_fate() {
    for receive_first in [true, false] {
        let fixture = Fixture::new();
        let version = publish_model(&fixture,
            &super::send_receive_tests::receive_boundary_model(true, true));
        let receiver = start_version(&fixture, &version);
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &receiver.instance_id).unwrap();
        let own = snapshot.subscriptions.iter().find(|sub|
            sub.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
        let boundary = snapshot.subscriptions.iter().find(|sub|
            sub.node_id == "ReceiveMessage").unwrap();
        let selected_sub = if receive_first { own } else { boundary };
        let message = envelope(catch_target(&version, Some(&receiver.instance_id),
            Some(&selected_sub.subscription_id)), json!({"source":"real envelope"}));
        let sent = send(&fixture, &message);
        let at_ms = sent.received_at_ms;
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
            .find(|row| row.key.message_id == message.message_id).unwrap();
        let MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap()
            else { panic!("the selected Receive path must be ready") };
        let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        assert_eq!(plan.subscription_updates.iter().filter(|update|
            update.subscription_id == own.subscription_id
                && update.status == if receive_first {
                    ProcessSubscriptionStatus::Consumed
                } else { ProcessSubscriptionStatus::Cancelled }).count(), 1);
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        for (case, forged) in [
            ("missing own subscription update", {
                let mut plan = plan.clone();
                plan.subscription_updates.retain(|update|
                    update.subscription_id != own.subscription_id);
                plan
            }),
            ("foreign own subscription update", {
                let mut plan = plan.clone();
                plan.subscription_updates.iter_mut().find(|update|
                    update.subscription_id == own.subscription_id).unwrap().subscription_id =
                    Uuid::new_v4().to_string();
                plan
            }),
            ("wrong envelope source", {
                let mut plan = plan.clone();
                plan.events.iter_mut().find(|event| event.kind == "message_delivered")
                    .unwrap().data["message_id"] = json!(Uuid::new_v4().to_string());
                plan
            }),
            ("wrong selected scope", {
                let mut plan = plan.clone();
                plan.events.iter_mut().find(|event| event.kind == "message_delivered")
                    .unwrap().scope_id = Uuid::new_v4().to_string();
                plan
            }),
            ("wrong source activation", {
                let mut plan = plan.clone();
                let index = plan.events.iter().position(|event|
                    event.kind == "message_delivered").unwrap();
                plan.event_sources.insert(index, Uuid::new_v4().to_string());
                plan
            }),
            ("wrong activity completion", {
                let mut plan = plan.clone();
                if receive_first {
                    plan.events.iter_mut().find(|event|
                        event.kind == "receive_task_completed").unwrap()
                        .data["subscription_id"] = json!(Uuid::new_v4().to_string());
                } else {
                    plan.events.push(super::repository::PlannedEvent {
                        kind: "receive_task_completed".into(),
                        scope_id: own.scope_id.clone(),
                        node_id: Some(own.node_id.clone()),
                        data: json!({"subscription_id":own.subscription_id,
                            "attached_token_id":own.token_id,
                            "message_id":message.message_id}),
                    });
                }
                plan
            }),
        ] {
            assert!(repository::deliver_message(&fixture.db, &prepared,
                repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err(), "{case}");
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
                "{case}");
        }
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        repository::deliver_message(&reopened, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let committed = super::signal_proof_tests::all_transition_rows(&fixture);
        assert!(repository::deliver_message(&reopened, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
    }
}

#[test]
fn interrupting_receive_timer_cancels_only_the_pinned_own_subscription() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture,
        &super::send_receive_tests::receive_boundary_model(false, true));
    let receiver = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &receiver.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().find(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let timer = snapshot.timers.iter().find(|row| row.node_id == "ReceiveTimer").unwrap();
    let at_ms = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|row| row.timer_id == timer.timer_id).unwrap();
    let selected = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&selected, at_ms, None, None).unwrap();
    assert_eq!(plan.subscription_updates.iter().filter(|update|
        update.subscription_id == own.subscription_id
            && update.status == ProcessSubscriptionStatus::Cancelled).count(), 1);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("missing own cancellation", {
            let mut plan = plan.clone();
            plan.subscription_updates.retain(|update|
                update.subscription_id != own.subscription_id);
            plan
        }),
        ("foreign own cancellation", {
            let mut plan = plan.clone();
            plan.subscription_updates.iter_mut().find(|update|
                update.subscription_id == own.subscription_id).unwrap().subscription_id =
                Uuid::new_v4().to_string();
            plan
        }),
        ("wrong timer UUID", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "timer_fired")
                .unwrap().data["timer_id"] = json!(Uuid::new_v4().to_string());
            plan
        }),
        ("wrong source scope", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event| event.kind == "timer_fired")
                .unwrap().scope_id = Uuid::new_v4().to_string();
            plan
        }),
        ("wrong timer source", {
            let mut plan = plan.clone();
            let index = plan.events.iter().position(|event|
                event.kind == "timer_fired").unwrap();
            plan.event_sources.insert(index, Uuid::new_v4().to_string());
            plan
        }),
        ("fabricated Receive completion", {
            let mut plan = plan.clone();
            plan.events.push(repository::PlannedEvent {
                scope_id: own.scope_id.clone(),
                kind: "receive_task_completed".into(),
                node_id: Some(own.node_id.clone()),
                data: json!({"subscription_id":own.subscription_id,
                    "attached_token_id":own.token_id,
                    "message_id":Uuid::new_v4().to_string()}),
            });
            plan
        }),
    ] {
        assert!(repository::fire_timer(&fixture.db, &candidate, &fixture.owner,
            Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&forged),
            at_ms).is_err(), "{case}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before, "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap().unwrap();
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::fire_timer(&reopened, &candidate, &fixture.owner,
        Some(snapshot.instance.revision), repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

#[test]
fn direct_receive_race_rejects_swapped_winner_loser_and_source_before_commit() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture,
        &super::send_receive_tests::receive_race_model());
    let receiver = start_version(&fixture, &version);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &receiver.instance_id).unwrap();
    let own = snapshot.subscriptions.iter().find(|row|
        row.kind == ProcessSubscriptionKind::ReceiveTask).unwrap();
    let timer = snapshot.timers.iter().find(|row| row.node_id == "Timer_1").unwrap();
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&own.subscription_id)), json!({"actual":true}));
    let sent = send(&fixture, &message);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|row| row.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("Receive winner requires its open branch") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert_eq!(plan.race_updates.len(), 1);
    assert_eq!(plan.timer_updates.iter().filter(|update|
        update.timer_id == timer.timer_id).count(), 1);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for (case, forged) in [
        ("foreign race", {
            let mut plan = plan.clone();
            plan.race_updates[0].race_id = Uuid::new_v4().to_string();
            plan
        }),
        ("wrong winner subscription", {
            let mut plan = plan.clone();
            plan.race_updates[0].winner_subscription_id =
                Some(Uuid::new_v4().to_string());
            plan
        }),
        ("missing loser timer closure", {
            let mut plan = plan.clone();
            plan.timer_updates.retain(|update| update.timer_id != timer.timer_id);
            plan
        }),
        ("missing loser activation", {
            let mut plan = plan.clone();
            plan.cancel_token_ids.retain(|id|
                Some(id.as_str()) != timer.token_id.as_deref());
            plan
        }),
        ("foreign source event", {
            let mut plan = plan.clone();
            let index = plan.events.iter().position(|event|
                event.kind == "message_delivered").unwrap();
            plan.event_sources.insert(index, Uuid::new_v4().to_string());
            plan
        }),
        ("missing Receive completion", {
            let mut plan = plan.clone();
            plan.events.iter_mut().find(|event|
                event.kind == "receive_task_completed").unwrap().kind =
                "node_completed".into();
            plan
        }),
    ] {
        assert!(repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err(), "{case}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
            "{case}");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    assert!(repository::deliver_message(&reopened, &prepared,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

#[test]
fn receive_task_rejects_missing_duplicate_foreign_and_reordered_completion_before_commit() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("receive-arm-kind-proof");
    let vars = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let start = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, vars.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let opened = start.events.iter().find(|event| event.kind == "receive_task_opened").unwrap();
    assert_eq!(opened.data["kind"], "receive_task");
    let before_start = transition_rows(&fixture);
    let mut wrong_kind = start.clone();
    wrong_kind.events.iter_mut().find(|event| event.kind == "receive_task_opened")
        .unwrap().data["kind"] = json!("ReceiveTask");
    let error = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&wrong_kind), at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("ReceiveTask correlation differs from its pinned entry variables"));
    assert_eq!(transition_rows(&fixture), before_start);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&start), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_opened"
        && event.data["kind"] == "receive_task").count(), 1);
    let committed_start = transition_rows(&fixture);
    repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &vars, None, None, repository::ProcessPlanInput::Supplied(&start), at_ms).unwrap();
    assert_eq!(transition_rows(&fixture), committed_start);
    let receiver = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let foreign_receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the real ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    let before = transition_rows(&fixture);
    let mut omitted = plan.clone();
    omitted.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().kind = "node_completed".into();
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&omitted), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut duplicate = plan.clone();
    duplicate.events.push(plan.events.iter().find(|event|
        event.kind == "receive_task_completed").unwrap().clone());
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&duplicate), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign = plan.clone();
    foreign.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().data["subscription_id"] = json!(Uuid::new_v4().to_string());
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&foreign), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_message = plan.clone();
    wrong_message.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().data["message_id"] = json!(Uuid::new_v4().to_string());
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_message), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_payload = plan.clone();
    wrong_payload.events.iter_mut().find(|event| event.kind == "message_delivered")
        .unwrap().data["payload"] = json!({"value":43});
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_payload), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut foreign_cancellation = plan.clone();
    foreign_cancellation.cancel_token_ids.push(foreign_receiver.subscriptions[0].token_id.clone());
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&foreign_cancellation), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_mapping = plan.clone();
    wrong_mapping.variables["received"] = json!({"value":44});
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_mapping), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_cutoff), at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    let mut reordered = plan.clone();
    let delivery = reordered.events.iter().position(|event| event.kind == "message_delivered").unwrap();
    let completion = reordered.events.iter().position(|event| event.kind == "receive_task_completed").unwrap();
    reordered.events.swap(delivery, completion);
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&reordered), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    assert_ne!(transition_rows(&fixture), before);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &receiver.instance.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(&reopened, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
}

#[test]
fn receive_task_termination_requires_its_exact_delivery_and_terminal_source() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    let sent = send(&fixture, &message);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the actual ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert!(matches!(plan.termination_attempts.as_slice(), [TerminationAttempt::Success(_)]));
    let before = transition_rows(&fixture);
    let mut wrong_source = plan.clone();
    let TerminationAttempt::Success(source) = &mut wrong_source.termination_attempts[0]
        else { panic!("the root termination must be successful") };
    source.source_event_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_source), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut missing_completion = plan.clone();
    missing_completion.events.iter_mut().find(|event| event.kind == "receive_task_completed")
        .unwrap().kind = "node_completed".into();
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&missing_completion), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_cutoff), at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap().status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    assert!(repository::deliver_message(&reopened, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
}

#[test]
fn embedded_receive_return_failure_keeps_its_factual_message_and_child_source() {
    let fixture = Fixture::new();
    let mut child = super::send_receive_tests::receive_model();
    child.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let mut model = embedded_model(child, "Scope");
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind
        else { panic!("the pinned child scope is missing") };
    output_mapping.insert("mapped_result".into(), "outputs.missing.required".into());
    let version = publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&receiver.subscriptions[0].subscription_id)), json!({"value":42}));
    let sent = send(&fixture, &message);
    let at_ms = sent.received_at_ms;
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap().into_iter()
        .find(|item| item.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate).unwrap()
        else { panic!("the embedded ReceiveTask must be selected") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    assert!(matches!(plan.termination_attempts.as_slice(), [TerminationAttempt::ReturnFailure(_)]));
    let before = transition_rows(&fixture);
    let mut wrong_child_source = plan.clone();
    let TerminationAttempt::ReturnFailure(source) = &mut wrong_child_source.termination_attempts[0]
        else { panic!("the failed return must be factual") };
    source.source_event_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_child_source), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_parent = plan.clone();
    let TerminationAttempt::ReturnFailure(source) = &mut wrong_parent.termination_attempts[0]
        else { panic!("the failed return must be factual") };
    source.parent_token_id = Uuid::new_v4().to_string();
    assert!(repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_parent), at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    let mut wrong_cutoff = plan.clone();
    let completion_index = wrong_cutoff.events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    let mapped = wrong_cutoff.variable_effects.iter_mut().find(|effect|
        matches!(effect, VariableEffect::Mapped { node_id, .. } if node_id == "Catch_1")).unwrap();
    let VariableEffect::Mapped { event_index, .. } = mapped else { unreachable!() };
    assert_ne!(*event_index, completion_index);
    *event_index = completion_index;
    let error = repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&wrong_cutoff), at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains("mapped message output has no exact source-time delivered envelope"));
    assert_eq!(transition_rows(&fixture), before);
    repository::deliver_message(&fixture.db, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner,
        &receiver.instance_id).unwrap();
    assert_eq!(actual.instance.status, ProcessInstanceStatus::Incident);
    let TerminationAttempt::ReturnFailure(source) = &plan.termination_attempts[0]
        else { unreachable!() };
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == source.source_scope_id)
        .unwrap().status, ProcessInstanceStatus::Incident);
    assert!(actual.tokens.iter().any(|token| token.token_id == source.parent_token_id
        && token.status == "waiting"));
    assert!(actual.tokens.iter().any(|token| token.token_id == source.waiting_token_id
        && token.scope_id == source.source_scope_id && token.status == "waiting"));
    assert_eq!(actual.incidents.iter().filter(|incident|
        incident.incident_id == source.incident_id && incident.code == "SCOPE_RETURN_ERROR").count(), 1);
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 1);
    assert!(!events.iter().any(|event| event.kind == "terminate_end_reached"));
    let source_events = events.iter().filter(|event| event.kind == "incident"
        && event.event_id == source.source_event_id
        && event.data["source_kind"] == "terminate_end_return_failure"
        && event.data["parent_token_id"] == source.parent_token_id
        && event.data["source_event_id"] == source.source_event_id).collect::<Vec<_>>();
    assert_eq!(source_events.len(), 1);
    assert!(repository::deliver_message(&reopened, &prepared, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().is_none());
}
