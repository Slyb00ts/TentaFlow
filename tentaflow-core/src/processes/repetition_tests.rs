// ============ File: repetition_tests.rs — file-backed repeated activity state and writer regressions ============

use serde_json::json;
use uuid::Uuid;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessCallActivity, ProcessInstanceStatus, ProcessMultiInstanceInput, ProcessMultiInstanceMode,
    ProcessNode, ProcessNodeKind, ProcessRepeatSpec, ProcessRepetitionOccurrenceStatus,
    ProcessUserTaskStatus,
};

use super::model::starter_model;
use super::repository;
use super::runtime::{self, StartCause,
    test_support::{edge, flow, graph, human_input, manual_input, publish_model, service_model,
        stamp, start_model, Fixture}};

#[test]
fn repeated_manual_acknowledgments_aggregate_null_with_exact_factual_source() {
    let fixture = Fixture::new();
    let mut model = super::manual_tests::manual_model(None, false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Manual").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let (instance_id, _, _) = super::manual_tests::start_manual(&fixture, &model);
    for ordinal in 0..2 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.node_id == "Manual" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp(&format!("acknowledge repeated Manual ordinal {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
        let source_index = plan.events.iter().position(|event|
            event.kind == "manual_task_acknowledged").unwrap();
        let completion = plan.events.iter().position(|event|
            event.kind == "repetition_occurrence_completed").unwrap();
        assert!(source_index < completion);
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged.repetition_occurrences.iter_mut().find(|row|
            row.ordinal == ordinal && row.status == ProcessRepetitionOccurrenceStatus::Completed)
            .unwrap().accepted_source_event_id = Some(Uuid::new_v4().to_string());
        assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([null, null]));
}

#[test]
fn parallel_manual_acknowledgments_keep_the_other_ordinal_live() {
    let fixture = Fixture::new();
    let mut model = super::manual_tests::manual_model(None, false);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Manual").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    for ordinal in [1, 0] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let task_id = occurrence.user_task_id.as_ref().unwrap();
        assert!(snapshot.user_tasks.iter().any(|task|
            task.user_task_id == *task_id && task.status == ProcessUserTaskStatus::Open));
        let command = stamp(&format!("acknowledge parallel Manual ordinal {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, task_id, &command), None).unwrap();
        if ordinal == 1 {
            let other = snapshot.repetition_occurrences.iter().find(|row|
                row.ordinal == 0).unwrap();
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            let mut foreign_task = plan.clone();
            foreign_task.repetition_occurrences.iter_mut().find(|row|
                row.ordinal == ordinal).unwrap().user_task_id = other.user_task_id.clone();
            assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
                &command, &started.instance_id, task_id, snapshot.instance.revision,
                repository::ProcessPlanInput::Supplied(&foreign_task), at_ms).is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        }
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &started.instance_id, task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 { 1 } else { 2 });
        if ordinal == 1 {
            let other = persisted.repetition_occurrences.iter().find(|row|
                row.ordinal == 0).unwrap();
            assert_eq!(other.status, ProcessRepetitionOccurrenceStatus::Active);
            assert!(persisted.user_tasks.iter().any(|task|
                task.user_task_id == *other.user_task_id.as_ref().unwrap()
                    && task.status == ProcessUserTaskStatus::Open));
        }
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.variables["results"], json!([null, null]));
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
}

#[test]
fn repeated_send_admits_two_distinct_outbox_facts_before_delivery() {
    let fixture = Fixture::new();
    let receiving = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture, &receiving);
    let mut model = super::send_receive_tests::send_model(
        &receiving.definition_id, &receiver.instance_id);
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Send_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("admit both factual Send ordinals");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let admitted = plan.events.iter().enumerate().filter(|(_, event)|
        event.kind == "send_task_admitted").collect::<Vec<_>>();
    assert_eq!(admitted.len(), 2);
    assert_eq!(plan.create_messages.len(), 2);
    assert_ne!(plan.create_messages[0].message.message_id,
        plan.create_messages[1].message.message_id);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.repetition_occurrences.iter_mut().find(|row| row.ordinal == 1).unwrap()
        .accepted_source_event_id = Some(plan.event_ids[&admitted[0].0].clone());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let mut foreign_ordinal = plan.clone();
    let second_token = plan.events.iter().find(|event|
        event.kind == "repetition_occurrence_started"
            && event.data["ordinal"] == 1).unwrap().data["token_id"]
        .as_str().unwrap().to_owned();
    let first_token = plan.events.iter().find(|event|
        event.kind == "repetition_occurrence_started"
            && event.data["ordinal"] == 0).unwrap().data["token_id"]
        .as_str().unwrap().to_owned();
    foreign_ordinal.token_sources.insert(second_token, first_token);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&foreign_ordinal), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([null, null]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    for message in &plan.create_messages {
        let receipt = repository::get_message(&reopened, &fixture.owner,
            &fixture.owner.user_id, &message.message.message_id).unwrap();
        assert_eq!(receipt.message.status,
            tentaflow_protocol::processes::ProcessMessageStatus::Pending);
    }
}

#[test]
fn parallel_and_structured_send_ordinals_keep_distinct_admission_sources() {
    for repeat in [
        ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        },
        ProcessRepeatSpec::StructuredLoop {
            condition: "true".into(),
            test_before: false,
            max_iterations: 2,
            output_collection_variable: "results".into(),
        },
    ] {
        let fixture = Fixture::new();
        let receiving = publish_model(&fixture, &super::send_receive_tests::receive_model());
        let receiver = super::messages::test_support::start_version(&fixture, &receiving);
        let mut model = super::send_receive_tests::send_model(
            &receiving.definition_id, &receiver.instance_id);
        model.variables.insert("results".into(), json!([]));
        model.nodes.iter_mut().find(|node| node.id == "Send_1").unwrap().repeat = Some(repeat);
        let version = publish_model(&fixture, &model);
        let instance_id = Uuid::new_v4().to_string();
        let command = stamp("admit parallel or structured Send ordinals");
        let variables = serde_json::to_value(&model.variables).unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
            &version.definition_id, version.version, variables.clone(), StartCause::Manual,
            at_ms, manual_input(&command), None).unwrap();
        let admissions = plan.events.iter().enumerate().filter(|(_, event)|
            event.kind == "send_task_admitted").map(|(index, _)| index).collect::<Vec<_>>();
        assert_eq!(admissions.len(), 2);
        assert_eq!(plan.repetition_occurrences.iter().filter(|row|
            row.status == ProcessRepetitionOccurrenceStatus::Completed).count(), 2);
        assert!(plan.repetition_occurrences.iter().all(|row|
            row.accepted_source_event_id.as_ref().is_some_and(|source|
                admissions.iter().any(|index| plan.event_ids.get(index) == Some(source)))));
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged.repetition_occurrences.iter_mut().find(|row| row.ordinal == 1).unwrap()
            .accepted_source_event_id = Some(plan.event_ids[&admissions[0]].clone());
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        let started = repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        assert_eq!(started.variables["results"], json!([null, null]));
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id)
            .unwrap().repetition_groups[0].completed_count, 2);
    }
}

#[test]
fn repeated_receive_consumes_distinct_messages_and_preserves_null_payload() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let receiver = super::messages::test_support::start_version(&fixture, &version);
    for (ordinal, payload) in [serde_json::Value::Null, json!({"value": 2})]
        .into_iter().enumerate() {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &receiver.instance_id).unwrap();
        let subscription = snapshot.subscriptions.iter().find(|row|
            row.node_id == "Catch_1"
                && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .unwrap();
        let message = super::messages::test_support::envelope(
            super::messages::test_support::catch_target(&version,
                Some(&receiver.instance_id), Some(&subscription.subscription_id)), payload.clone());
        super::messages::test_support::send(&fixture, &message);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
        let repository::MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap()
            else { panic!("the repeated Receive ordinal is not the selected message target") };
        let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        let delivery = plan.events.iter().position(|event| event.kind == "message_delivered").unwrap();
        let completion = plan.events.iter().position(|event|
            event.kind == "repetition_occurrence_completed").unwrap();
        assert!(delivery < completion);
        if ordinal == 0 {
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            let mut forged = plan.clone();
            forged.repetition_occurrences.iter_mut().find(|row|
                row.ordinal == ordinal as u32).unwrap().accepted_source_event_id =
                Some(Uuid::new_v4().to_string());
            assert!(repository::deliver_message(&fixture.db, &prepared,
                repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        }
        repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &receiver.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal as u32 + 1);
    }
    let complete = repository::get_instance(&fixture.db, &fixture.owner,
        &receiver.instance_id, None).unwrap();
    assert_eq!(complete.status, ProcessInstanceStatus::Completed);
    assert_eq!(complete.variables["results"], json!([null, {"value": 2}]));
}

#[test]
fn structured_receive_uses_accepted_payload_and_source_order_for_next_ordinal() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("results".into(), json!([]));
    model.variables.insert("step".into(), json!(0));
    let node = model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap();
    node.repeat = Some(ProcessRepeatSpec::StructuredLoop {
        condition: "vars.step < 2".into(),
        test_before: false,
        max_iterations: 3,
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ReceiveTask { output_mapping, .. } = &mut node.kind else {
        panic!("the repeated catch changed node kind")
    };
    output_mapping.insert("step".into(), "vars.step + 1".into());
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    for ordinal in 0..2 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let subscription = snapshot.subscriptions.iter().find(|row|
            row.node_id == "Catch_1"
                && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .unwrap();
        let payload = json!({"ordinal": ordinal});
        let message = super::messages::test_support::envelope(
            super::messages::test_support::catch_target(&version,
                Some(&started.instance_id), Some(&subscription.subscription_id)), payload.clone());
        super::messages::test_support::send(&fixture, &message);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
        let repository::MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap() else {
                panic!("the real Receive ordinal was not selected")
            };
        let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        assert_eq!(plan.events.iter().filter(|event|
            event.kind == "repetition_occurrence_completed").count(), 1);
        repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
        assert_eq!(persisted.repetition_groups[0].loop_state.as_ref().unwrap()["step"], ordinal + 1);
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{"ordinal":0}, {"ordinal":1}]));
    assert_eq!(completed.variables["step"], 2);
}

#[test]
fn repeated_embedded_children_return_from_distinct_scopes_in_ordinal_order() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, false);
    let mut model = super::runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    for ordinal in 0..2 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.node_id == "Manual" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let child_scope = task.scope_id.clone();
        assert_ne!(child_scope, started.instance_id);
        let command = stamp(&format!("finish repeated child scope {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
        let source = plan.events.iter().position(|event|
            event.kind == "scope_completed" && event.scope_id == child_scope).unwrap();
        let complete = plan.events.iter().position(|event|
            event.kind == "repetition_occurrence_completed").unwrap();
        assert!(source < complete);
        if ordinal == 0 {
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            let mut forged = plan.clone();
            forged.repetition_occurrences.iter_mut().find(|row|
                row.status == ProcessRepetitionOccurrenceStatus::Completed).unwrap()
                .accepted_source_event_id = Some(Uuid::new_v4().to_string());
            assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
                &command, &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        }
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
        let children = persisted.scopes.iter().filter(|scope|
            scope.parent_scope_id.as_deref() == Some(started.instance_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 2);
        assert_eq!(children.iter().filter(|scope|
            scope.status == ProcessInstanceStatus::Completed).count(), ordinal as usize + 1);
        assert_eq!(children.iter().filter(|scope|
            matches!(scope.status, ProcessInstanceStatus::Running
                | ProcessInstanceStatus::Waiting)).count(), 1 - ordinal as usize);
        let parent_tokens = children.iter().map(|scope|
            scope.parent_token_id.as_deref().unwrap()).collect::<std::collections::HashSet<_>>();
        assert_eq!(parent_tokens.len(), children.len());
        assert!(children.iter().all(|scope| persisted.repetition_occurrences.iter().any(|row|
            scope.parent_token_id.as_deref() == Some(row.token_id.as_str()))));
        let history = repository::list_events(&reopened, &fixture.owner,
            &started.instance_id, 0, 200).unwrap().0;
        let returns = history.iter().filter(|event| event.kind == "scope_completed"
            && children.iter().any(|scope| scope.scope_id == event.scope_id
                && event.data["parent_token_id"] == scope.parent_token_id.as_deref().unwrap()))
            .count();
        assert_eq!(returns, ordinal as usize + 1);
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{}, {}]));
}

#[test]
fn repeated_embedded_script_children_complete_from_same_start_command() {
    let fixture = Fixture::new();
    let mut inner = starter_model();
    inner.nodes.insert(1, ProcessNode {
        id: "Compute".into(), name: "Compute child result".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "{\"value\": 7}".into(), output_mapping: Default::default(),
        }, repeat: None,
        activity_io: None,
    });
    inner.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    let mut model = super::runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("start two real embedded Script children");
    let variables = serde_json::to_value(&model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let child_returns = plan.events.iter().enumerate().filter(|(_, event)|
        event.kind == "scope_completed").collect::<Vec<_>>();
    assert_eq!(child_returns.len(), 2);
    assert_eq!(plan.events.iter().filter(|event| event.kind == "script_completed").count(), 2);
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "repetition_occurrence_completed").count(), 2);
    for (index, event) in &child_returns {
        let ordinal = plan.repetition_occurrences.iter().find(|row|
            event.data["parent_token_id"] == row.token_id).unwrap();
        assert_eq!(ordinal.accepted_source_event_id.as_ref(), plan.event_ids.get(index));
        assert!(plan.events[..*index].iter().any(|earlier|
            earlier.kind == "script_completed" && earlier.scope_id == event.scope_id));
    }
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut wrong_source = plan.clone();
    let first_source = wrong_source.repetition_occurrences.iter().find(|row|
        row.ordinal == 0).unwrap().accepted_source_event_id.clone();
    wrong_source.repetition_occurrences.iter_mut().find(|row|
        row.ordinal == 1).unwrap().accepted_source_event_id = first_source;
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_source), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let mut wrong_child = plan.clone();
    let first_end = wrong_child.events.iter().enumerate().find(|(_, event)|
        event.kind == "end_reached" && event.scope_id == child_returns[0].1.scope_id).unwrap();
    let first_end_id = wrong_child.event_sources[&first_end.0].clone();
    let first_predecessor = wrong_child.token_sources[&first_end_id].clone();
    let second_end = wrong_child.events.iter().enumerate().find(|(_, event)|
        event.kind == "end_reached" && event.scope_id == child_returns[1].1.scope_id).unwrap();
    let second_end_id = wrong_child.event_sources[&second_end.0].clone();
    wrong_child.token_sources.insert(second_end_id, first_predecessor);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_child), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{}, {}]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(persisted.repetition_groups[0].completed_count, 2);
    assert_eq!(persisted.scopes.iter().filter(|scope|
        scope.parent_scope_id.as_deref() == Some(instance_id.as_str())
            && scope.status == ProcessInstanceStatus::Completed).count(), 2);
}

#[test]
fn repeated_embedded_receive_children_consume_distinct_real_messages() {
    let fixture = Fixture::new();
    let mut inner = super::send_receive_tests::receive_model();
    inner.variables.insert("received".into(), serde_json::Value::Null);
    let mut model = super::runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let receiver = super::messages::test_support::start_version(&fixture, &version);
    for ordinal in [1, 0] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &receiver.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let child = snapshot.scopes.iter().find(|scope|
            scope.parent_token_id.as_deref() == Some(occurrence.token_id.as_str())).unwrap();
        let subscription = snapshot.subscriptions.iter().find(|row|
            row.scope_id == child.scope_id && row.node_id == "Catch_1"
                && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .unwrap();
        let payload = json!({"ordinal": ordinal});
        let message = super::messages::test_support::envelope(
            super::messages::test_support::catch_target(&version,
                Some(&receiver.instance_id), Some(&subscription.subscription_id)), payload.clone());
        super::messages::test_support::send(&fixture, &message);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
        let repository::MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap()
            else { panic!("the real child Receive subscription was not selected") };
        let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        let source_index = plan.events.iter().position(|event|
            event.kind == "scope_completed" && event.scope_id == child.scope_id).unwrap();
        let completion_index = plan.events.iter().position(|event|
            event.kind == "repetition_occurrence_completed").unwrap();
        assert!(plan.events[..source_index].iter().any(|event|
            event.kind == "message_delivered" && event.scope_id == child.scope_id
                && event.data["subscription_id"] == subscription.subscription_id));
        assert!(source_index < completion_index);
        let completed_ordinal = plan.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        assert_eq!(completed_ordinal.accepted_source_event_id.as_ref(),
            plan.event_ids.get(&source_index));
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let other = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal != ordinal).unwrap();
        let mut wrong_parent = plan.clone();
        wrong_parent.events[source_index].data["parent_token_id"] = json!(other.token_id);
        assert!(repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&wrong_parent), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        let mut wrong_source = plan.clone();
        wrong_source.repetition_occurrences.iter_mut().find(|row|
            row.ordinal == ordinal).unwrap().accepted_source_event_id =
            Some(Uuid::new_v4().to_string());
        assert!(repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&wrong_source), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &receiver.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 { 1 } else { 2 });
        if ordinal == 1 {
            assert!(persisted.subscriptions.iter().any(|row|
                row.scope_id != child.scope_id && row.node_id == "Catch_1"
                    && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open));
        }
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let completed = repository::get_instance(&reopened, &fixture.owner,
        &receiver.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"],
        json!([{"received":{"ordinal":0}}, {"received":{"ordinal":1}}]));
}

#[test]
fn repeated_calls_return_from_distinct_pinned_children_once() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::manual_tests::manual_model(None, false));
    let mut model = super::call_tests::caller(&target, Default::default());
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    let mut child_ids = std::collections::HashSet::new();
    for ordinal in 0..2 {
        let parent = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let call = parent.calls.iter().find(|call|
            call.status == tentaflow_protocol::processes::ProcessCallStatus::Waiting).unwrap();
        assert!(child_ids.insert(call.child_instance_id.clone()));
        assert_eq!(call.called_definition_id, target.definition_id);
        assert_eq!(call.called_version, target.version);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &call.child_instance_id).unwrap();
        let task = child.user_tasks.iter().find(|task|
            task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp(&format!("acknowledge repeated Call child {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&child, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&child, &task.user_task_id, &command), None).unwrap();
        if ordinal == 0 {
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| {
                *mutator.borrow_mut() = Some(Box::new(|composite| {
                    let returned = composite.call_steps.iter_mut().find_map(|step| match step {
                        repository::CallStep::Return { plan, .. } => Some(plan),
                        _ => None,
                    }).expect("factual repeated Call return step");
                    returned.repetition_occurrences.iter_mut().find(|row|
                        row.status == ProcessRepetitionOccurrenceStatus::Completed).unwrap()
                        .accepted_source_event_id = Some(Uuid::new_v4().to_string());
                }));
            });
            let forged = repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
                &command, &call.child_instance_id, &task.user_task_id,
                child.instance.revision, repository::ProcessPlanInput::Supplied(&plan), at_ms);
            repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| *mutator.borrow_mut() = None);
            assert!(forged.is_err());
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        }
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &call.child_instance_id, &task.user_task_id, child.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
        assert_eq!(persisted.calls.iter().filter(|call|
            call.status == tentaflow_protocol::processes::ProcessCallStatus::Returned).count(),
            ordinal as usize + 1);
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["results"], json!([{}, {}]));
}

#[test]
fn parallel_repeated_calls_with_immediate_child_termination_reconcile_both_pinned_requests() {
    let fixture = Fixture::new();
    let mut child_model = starter_model();
    child_model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let target = publish_model(&fixture, &child_model);
    let mut model = super::call_tests::caller(&target, Default::default());
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start parallel pinned Calls with immediate child termination");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.call_requests.len(), 2);
    assert_ne!(plan.call_requests[0].call_id, plan.call_requests[1].call_id);
    assert_ne!(plan.call_requests[0].parent_token_id, plan.call_requests[1].parent_token_id);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.call_requests[1].parent_token_id = forged.call_requests[0].parent_token_id.clone();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);

    let started = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let parent = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(parent.repetition_groups[0].completed_count, 2);
    assert_eq!(parent.calls.len(), 2);
    assert!(parent.calls.iter().all(|call|
        call.status == tentaflow_protocol::processes::ProcessCallStatus::Returned
            && call.called_definition_id == target.definition_id
            && call.called_version == target.version
            && call.model_sha256 == target.model_sha256));
    let (events, _, _) = repository::list_events(&reopened, &fixture.owner,
        &instance_id, 0, 200).unwrap();
    assert_eq!(events.iter().filter(|event| event.kind == "call_requested").count(), 2);
    assert_eq!(events.iter().filter(|event| event.kind == "call_entered").count(), 2);
    let mut children = std::collections::HashSet::new();
    for call in &parent.calls {
        assert!(children.insert(call.child_instance_id.clone()));
        let (events, _, _) = repository::list_events(&reopened, &fixture.owner,
            &call.child_instance_id, 0, 200).unwrap();
        assert_eq!(events.iter().filter(|event|
            event.kind == "terminate_end_reached").count(), 1);
    }
    let committed = super::signal_proof_tests::all_transition_rows(&fixture);
    assert_eq!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().status,
        ProcessInstanceStatus::Completed);
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
}

pub(super) fn sequential_human_model(owner: &str, count: u8) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.variables.insert("results".into(), json!([]));
    model.nodes.insert(1, ProcessNode {
        id: "RepeatedWork".into(),
        name: "Review every item".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(owner.into()),
            output_mapping: Default::default(),
        },
        repeat: Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count },
            output_collection_variable: "results".into(),
        }),
        activity_io: None,
    });
    model.sequence_flows = vec![
        edge("EnterRepeatedWork", "Start_1", "RepeatedWork"),
        edge("LeaveRepeatedWork", "RepeatedWork", "End_1"),
    ];
    model
}

#[test]
fn sequential_human_ordinals_commit_once_and_forged_item_rolls_back_all_transition_tables() {
    let fixture = Fixture::new();
    assert!(fixture.directory.path().join("processes.db").is_file());
    let model = sequential_human_model(&fixture.owner.user_id, 3);
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    for ordinal in 0_u32..3 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let outputs = json!({"answer":ordinal});
        let command = stamp("accept the actual repeated Work ordinal");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
            None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
        if ordinal == 0 {
            let before = super::call_tests::transition_rows(&fixture);
            let completed = plan.events.iter().find(|event|
                event.kind == "repetition_occurrence_completed").unwrap().clone();
            for (fabricated, expected) in [
                (completed.clone(), "completed occurrence lacks one source-linked aggregate fact"),
                (super::repository::PlannedEvent {
                    kind: "repetition_unknown".into(), ..completed.clone()
                }, "repetition history differs"),
            ] {
                let mut forged = plan.clone();
                forged.events.push(fabricated);
                let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                    &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                    &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
                let message = format!("{error:#}");
                assert!(message.contains(expected), "{message}");
                assert_eq!(super::call_tests::transition_rows(&fixture), before);
            }
            let source_index = plan.events.iter().position(|event|
                event.kind == "user_task_completed").unwrap();
            let completed_index = plan.events.iter().position(|event|
                event.kind == "repetition_occurrence_completed").unwrap();
            assert!(source_index < completed_index);
            let remap = |index: usize| {
                if index == source_index { completed_index }
                else if index == completed_index { source_index }
                else { index }
            };
            let mut reordered = plan.clone();
            reordered.events.swap(source_index, completed_index);
            reordered.event_ids = plan.event_ids.iter().map(|(index, id)|
                (remap(*index), id.clone())).collect();
            reordered.event_sources = plan.event_sources.iter().map(|(index, source)|
                (remap(*index), source.clone())).collect();
            let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&reordered), at_ms).unwrap_err();
            assert!(format!("{error:#}").contains("repetition completion precedes its accepted source"));
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
            let mut foreign_source = plan.clone();
            foreign_source.repetition_occurrences.iter_mut()
                .find(|row| row.ordinal == ordinal).unwrap().accepted_source_event_id =
                Some(Uuid::new_v4().to_string());
            assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&foreign_source), at_ms).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
            let mut extra_closure = plan.clone();
            extra_closure.cancel_scope_roots.push(started.instance_id.clone());
            assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&extra_closure), at_ms).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }
        if ordinal < 2 {
            let before = super::call_tests::transition_rows(&fixture);
            assert_eq!(before.len(), 18);
            let mut forged = plan.clone();
            let next = forged.repetition_occurrences.iter_mut()
                .find(|row| row.ordinal == ordinal + 1).unwrap();
            next.item = json!("foreign item");
            let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
            assert!(format!("{error:#}").contains("repetition item"));
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups.len(), 1);
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["results"], json!([
        {"answer":0},{"answer":1},{"answer":2}
    ]));
}

#[test]
fn empty_multi_instance_finishes_without_an_occurrence_and_reopens() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &sequential_human_model(&fixture.owner.user_id, 0));
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups.len(), 1);
    assert!(snapshot.repetition_occurrences.is_empty());
    assert_eq!(snapshot.repetition_groups[0].total_count, Some(0));
    assert_eq!(snapshot.repetition_groups[0].completed_count, 0);
    assert_eq!(snapshot.instance.variables["results"], json!([]));
}

#[test]
fn empty_repetition_then_terminate_has_exact_history_and_atomic_forgery_rollback() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 0);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start an empty repetition then terminate");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert!(plan.events.iter().any(|event| event.kind == "repetition_completed"));
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 18);
    let completed = plan.events.iter().find(|event|
        event.kind == "repetition_completed").unwrap().clone();
    for fabricated in [completed.clone(), repository::PlannedEvent {
        kind: "repetition_unknown".into(), ..completed.clone()
    }] {
        let mut forged = plan.clone();
        forged.events.push(fabricated);
        repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
    }
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(persisted.repetition_groups.len(), 1);
    assert_eq!(persisted.repetition_groups[0].completed_count, 0);
    assert_eq!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn completed_repetition_returns_through_persisted_parent_before_terminate() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 1);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task|
        task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let outputs = json!({"answer":41});
    let command = stamp("complete a repeated Work item before termination");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
        None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    let accepted_index = plan.events.iter().position(|event|
        event.kind == "user_task_completed").unwrap();
    let completed_index = plan.events.iter().position(|event|
        event.kind == "repetition_completed").unwrap();
    let terminated_index = plan.events.iter().position(|event|
        event.kind == "terminate_end_reached").unwrap();
    assert!(accepted_index < completed_index && completed_index < terminated_index);
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 18);
    let mut wrong_source = plan.clone();
    wrong_source.repetition_occurrences.iter_mut().find(|row|
        row.ordinal == 0).unwrap().accepted_source_event_id = Some(Uuid::new_v4().to_string());
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&wrong_source), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut wrong_parent = plan.clone();
    wrong_parent.repetition_groups.iter_mut().find(|row|
        row.node_id == "RepeatedWork").unwrap().parent_token_id = Uuid::new_v4().to_string();
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&wrong_parent), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut duplicate_completion = plan.clone();
    duplicate_completion.events.push(plan.events[completed_index].clone());
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&duplicate_completion), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().instance;
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.repetition_groups[0].completed_count, 1);
    assert_eq!(persisted.instance.variables["results"], json!([{"answer":41}]));
    assert_eq!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().instance.status,
        ProcessInstanceStatus::Completed);
}

#[test]
fn parallel_human_occurrences_wait_independently_and_join_once() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 3);
    let repeat = model.nodes.iter_mut().find(|node| node.id == "RepeatedWork").unwrap()
        .repeat.as_mut().unwrap();
    let ProcessRepeatSpec::MultiInstance { mode, .. } = repeat else {
        panic!("fixture repetition is not multi-instance")
    };
    *mode = ProcessMultiInstanceMode::Parallel;
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    let initial = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(initial.repetition_occurrences.len(), 3);
    assert_eq!(initial.user_tasks.iter().filter(|task|
        task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).count(), 3);
    for (completed, ordinal) in [2_u32, 0, 1].into_iter().enumerate() {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row| row.ordinal == ordinal).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.user_task_id == *occurrence.user_task_id.as_ref().unwrap()).unwrap();
        let command = stamp("accept one parallel repeated Work item");
        let outputs = json!({"answer":ordinal});
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
            None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
        if completed == 0 {
            let foreign = snapshot.repetition_occurrences.iter().find(|row|
                row.ordinal != ordinal).unwrap();
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            assert_eq!(before.len(), super::call_tests::TRANSITION_TABLES.len() + 2);
            let mut wrong_ordinal_source = plan.clone();
            *wrong_ordinal_source.consume_token_ids.iter_mut().find(|token_id|
                token_id.as_str() == occurrence.token_id.as_str())
                .expect("factual ordinal consumption") =
                foreign.token_id.clone();
            repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&wrong_ordinal_source),
                at_ms).unwrap_err();
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, completed as u32 + 1);
        if completed == 0 {
            let committed = super::signal_proof_tests::all_transition_rows(&fixture);
            repository::complete_user_task(&reopened, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
            assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), committed);
        }
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["results"], json!([
        {"answer":0},{"answer":1},{"answer":2}
    ]));
}

#[test]
fn active_occurrence_limit_latches_once_and_closes_every_parallel_wait() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.nodes = vec![
        ProcessNode { id:"Start_1".into(), name:"Start".into(), kind:ProcessNodeKind::Start, repeat:None ,  activity_io: None,},
        ProcessNode { id:"Fanout".into(), name:"Fan out".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None ,  activity_io: None,},
    ];
    model.sequence_flows = vec![edge("EnterFanout", "Start_1", "Fanout")];
    for index in 0..5 {
        let node_id = format!("RepeatedWork_{index}");
        let target = format!("results_{index}");
        model.variables.insert(target.clone(), json!([]));
        model.nodes.push(ProcessNode {
            id:node_id.clone(), name:format!("Review branch {index}"),
            kind:ProcessNodeKind::UserTask {
                assignee_user_id:Some(fixture.owner.user_id.clone()),
                output_mapping:Default::default(),
            },
            repeat:Some(ProcessRepeatSpec::MultiInstance {
                mode:ProcessMultiInstanceMode::Parallel,
                input:ProcessMultiInstanceInput::Cardinality { count:16 },
                output_collection_variable:target,
            }),
            activity_io: None,
        });
        model.sequence_flows.push(edge(&format!("Branch_{index}"), "Fanout", &node_id));
        model.sequence_flows.push(edge(&format!("Join_{index}"), &node_id, "Join"));
    }
    model.nodes.push(ProcessNode { id:"Join".into(), name:"Join".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None ,  activity_io: None,});
    model.nodes.push(ProcessNode { id:"End_1".into(), name:"End".into(), kind:ProcessNodeKind::End, repeat:None ,  activity_io: None,});
    model.sequence_flows.push(edge("LeaveJoin", "Join", "End_1"));
    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    let latched = snapshot.repetition_groups.iter().filter(|group| group.terminal_capacity)
        .collect::<Vec<_>>();
    assert_eq!(latched.len(), 1);
    assert_eq!(latched[0].status, tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Incident);
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Cancelled);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == tentaflow_protocol::processes::ProcessRepetitionOccurrenceStatus::Cancelled));
    assert!(snapshot.user_tasks.iter().all(|task| task.status != ProcessUserTaskStatus::Open));
    let (events, _, _) = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap();
    let blocked = events.iter().filter(|event| event.kind == "repetition_group_blocked")
        .collect::<Vec<_>>();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].data["phase"], "capacity");
    assert_eq!(blocked[0].data["reason"], "active_occurrences");
    assert_eq!(super::call_tests::transition_rows(&fixture).len(), 18);
}

#[test]
fn active_service_job_limit_latches_before_the_thirty_third_claimable_job() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("repeated service capacity", None));
    let service = service_model(&flow_id, ActivityVerification::Human).nodes
        .into_iter().find(|node| node.id == "Service").unwrap();
    let mut model = starter_model();
    model.nodes = vec![
        ProcessNode { id:"Start_1".into(), name:"Start".into(), kind:ProcessNodeKind::Start, repeat:None ,  activity_io: None,},
        ProcessNode { id:"Fanout".into(), name:"Fan out".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None ,  activity_io: None,},
    ];
    model.sequence_flows = vec![edge("EnterFanout", "Start_1", "Fanout")];
    for branch in 0..3 {
        let node_id = format!("RepeatedService_{branch}");
        let target = format!("results_{branch}");
        model.variables.insert(target.clone(), json!([]));
        let mut node = service.clone();
        node.id.clone_from(&node_id);
        node.name = format!("Process branch {branch}");
        node.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode:ProcessMultiInstanceMode::Parallel,
            input:ProcessMultiInstanceInput::Cardinality { count:16 },
            output_collection_variable:target,
        });
        model.nodes.push(node);
        model.sequence_flows.push(edge(&format!("Branch_{branch}"), "Fanout", &node_id));
        model.sequence_flows.push(edge(&format!("Join_{branch}"), &node_id, "Join"));
    }
    model.nodes.push(ProcessNode { id:"Join".into(), name:"Join".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None ,  activity_io: None,});
    model.nodes.push(ProcessNode { id:"End_1".into(), name:"End".into(), kind:ProcessNodeKind::End, repeat:None ,  activity_io: None,});
    model.sequence_flows.push(edge("LeaveJoin", "Join", "End_1"));
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start parallel repeated Service branches");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.repetition_capacity.as_ref().unwrap().reason, "active_service_jobs");
    assert_eq!(plan.repetition_occurrences.len(), 48);
    assert_eq!(plan.create_jobs.len(), 32);
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 18);
    let mut forged = plan.clone();
    forged.repetition_capacity.as_mut().unwrap().reason = "active_occurrences".into();
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut extra_job = plan.clone();
    let mut invented = extra_job.create_jobs[0].clone();
    invented.job_id = Uuid::new_v4().to_string();
    extra_job.create_jobs.push(invented);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&extra_job), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Cancelled);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(persisted.repetition_occurrences.len(), 48);
    assert!(persisted.repetition_occurrences.iter().all(|row|
        row.status == tentaflow_protocol::processes::ProcessRepetitionOccurrenceStatus::Cancelled));
    assert_eq!(persisted.repetition_groups.iter().filter(|group|
        group.terminal_capacity
            && group.status == tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Incident).count(), 1);
    assert!(persisted.repetition_groups.iter().filter(|group| !group.terminal_capacity)
        .all(|group| group.status == tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Cancelled));
    assert_eq!(persisted.jobs.len(), 32);
    assert!(persisted.jobs.iter().all(|job| job.status == "cancelled"));
    assert_eq!(persisted.repetition_groups.iter().filter(|group| group.terminal_capacity).count(), 1);
    let mut after_seq = 0;
    let mut blocked = Vec::new();
    loop {
        let (events, next_seq, has_more) = repository::list_events(
            &reopened, &fixture.owner, &instance_id, after_seq, 200).unwrap();
        blocked.extend(events.into_iter().filter(|event|
            event.kind == "repetition_group_blocked"));
        if !has_more {
            break;
        }
        assert!(next_seq > after_seq);
        after_seq = next_seq;
    }
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].data["reason"], "active_service_jobs");
    let after = super::call_tests::transition_rows(&fixture);
    assert_eq!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().status, ProcessInstanceStatus::Cancelled);
    assert_eq!(super::call_tests::transition_rows(&fixture), after);
}

#[test]
fn parallel_receive_ordinals_require_one_factual_subscription_each() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    let initial = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(initial.subscriptions.iter().filter(|row|
        row.node_id == "Catch_1"
            && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open).count(), 2);
    let broad = super::messages::test_support::envelope(
        super::messages::test_support::catch_target(&version,
            Some(&started.instance_id), None), json!({"broad":true}));
    super::messages::test_support::send(&fixture, &broad);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let broad_candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|row| row.key.message_id == broad.message_id).unwrap();
    assert!(matches!(repository::message_snapshot(&fixture.db, &broad_candidate).unwrap(),
        repository::MessageSelection::Ambiguous));
    for ordinal in [1, 0] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let subscription = snapshot.subscriptions.iter().find(|row|
            row.token_id == occurrence.token_id
                && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .unwrap();
        let payload = json!({"ordinal":ordinal});
        let message = super::messages::test_support::envelope(
            super::messages::test_support::catch_target(&version,
                Some(&started.instance_id), Some(&subscription.subscription_id)), payload);
        super::messages::test_support::send(&fixture, &message);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
        let repository::MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap() else {
                panic!("the exact repeated Receive subscription was not selected")
            };
        let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        if ordinal == 1 {
            let other = snapshot.repetition_occurrences.iter().find(|row|
                row.ordinal != ordinal && row.status == ProcessRepetitionOccurrenceStatus::Active)
                .unwrap();
            let other_subscription = snapshot.subscriptions.iter().find(|row|
                row.token_id == other.token_id
                    && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
                .unwrap();
            assert_ne!(subscription.subscription_id, other_subscription.subscription_id);
            let delivery = plan.events.iter().position(|event|
                event.kind == "message_delivered").unwrap();
            let before = super::signal_proof_tests::all_transition_rows(&fixture);
            let mut wrong_subscription = plan.clone();
            wrong_subscription.events[delivery].data["subscription_id"] =
                json!(other_subscription.subscription_id.as_str());
            let mut wrong_token = plan.clone();
            wrong_token.events[delivery].data["attached_token_id"] = json!(other.token_id.as_str());
            for (case, forged) in [
                ("sibling subscription", wrong_subscription),
                ("sibling ordinal token", wrong_token),
            ] {
                let error = repository::deliver_message(&fixture.db, &prepared,
                    repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
                assert!(!format!("{error:#}").is_empty(), "{case} lacked a rejection reason");
                assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before,
                    "{case} changed durable process rows");
            }
        }
        repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 { 1 } else { 2 });
        if ordinal == 1 {
            let other = persisted.repetition_occurrences.iter().find(|row|
                row.ordinal == 0).unwrap();
            assert!(persisted.subscriptions.iter().any(|row|
                row.token_id == other.token_id
                    && row.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open));
        }
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.variables["results"], json!([{"ordinal":0},{"ordinal":1}]));
}

#[test]
fn parallel_embedded_ordinals_return_out_of_order_without_aliasing_child_scopes() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, false);
    let mut model = super::runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    for ordinal in [1, 0] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let scope = snapshot.scopes.iter().find(|scope|
            scope.parent_token_id.as_deref() == Some(occurrence.token_id.as_str())).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.scope_id == scope.scope_id && task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp(&format!("complete parallel embedded ordinal {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 { 1 } else { 2 });
        if ordinal == 1 {
            let other = persisted.repetition_occurrences.iter().find(|row|
                row.ordinal == 0).unwrap();
            assert!(persisted.scopes.iter().any(|scope|
                scope.parent_token_id.as_deref() == Some(other.token_id.as_str())
                    && matches!(scope.status,
                        ProcessInstanceStatus::Running | ProcessInstanceStatus::Waiting)));
        }
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.variables["results"], json!([{}, {}]));
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
}

#[test]
fn parallel_called_children_return_once_in_ordinal_order() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::manual_tests::manual_model(None, false));
    let mut model = super::call_tests::caller(&target, Default::default());
    model.variables.insert("results".into(), json!([]));
    model.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap().repeat =
        Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 2 },
            output_collection_variable: "results".into(),
        });
    let started = start_model(&fixture, &model);
    let initial = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(initial.calls.iter().filter(|call|
        call.status == tentaflow_protocol::processes::ProcessCallStatus::Waiting).count(), 2);
    for ordinal in [1, 0] {
        let parent = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = parent.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let call = parent.calls.iter().find(|call|
            call.parent_token_id == occurrence.token_id
                && call.status == tentaflow_protocol::processes::ProcessCallStatus::Waiting).unwrap();
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &call.child_instance_id).unwrap();
        let task = child.user_tasks.iter().find(|task|
            task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp(&format!("return parallel called ordinal {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&child, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&child, &task.user_task_id, &command), None).unwrap();
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &call.child_instance_id, &task.user_task_id, child.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 { 1 } else { 2 });
        if ordinal == 1 {
            let other = persisted.repetition_occurrences.iter().find(|row|
                row.ordinal == 0).unwrap();
            assert!(persisted.calls.iter().any(|call|
                call.parent_token_id == other.token_id
                    && call.status == tentaflow_protocol::processes::ProcessCallStatus::Waiting));
        }
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.variables["results"], json!([{}, {}]));
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
}

#[test]
fn collection_send_uses_each_frozen_item_for_target_key_and_payload() {
    let fixture = Fixture::new();
    let receiving = publish_model(&fixture, &super::send_receive_tests::receive_model());
    let first = super::messages::test_support::start_version(&fixture, &receiving);
    let second = super::messages::test_support::start_version(&fixture, &receiving);
    let mut model = super::send_receive_tests::send_model(
        &receiving.definition_id, &first.instance_id);
    model.variables.insert("items".into(), json!([
        {"recipient":first.instance_id,"key":"first","amount":11},
        {"recipient":second.instance_id,"key":"second","amount":22}
    ]));
    model.variables.insert("results".into(), json!([]));
    let node = model.nodes.iter_mut().find(|node| node.id == "Send_1").unwrap();
    node.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::SendTask {target,correlation_expression,payload_expression,..} = &mut node.kind else {
        panic!("the repeated node is not SendTask")
    };
    *target = tentaflow_protocol::processes::ProcessMessageTargetSpec::Catch {
        definition_id: receiving.definition_id.clone(),
        instance_id_expression: Some("repeat.item.recipient".into()),
        subscription_id_expression: None,
    };
    *correlation_expression = "repeat.item.key".into();
    *payload_expression = "{'amount': repeat.item.amount}".into();
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("admit distinct collection Send items");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.create_messages.len(), 2);
    for (ordinal, recipient, key, amount) in [
        (0, &first.instance_id, "first", 11),
        (1, &second.instance_id, "second", 22),
    ] {
        let message = &plan.create_messages[ordinal].message;
        let tentaflow_protocol::processes::ProcessMessageTarget::Catch {instance_id:target,..} =
            &message.target else { panic!("collection Send must target an existing catch") };
        assert_eq!(target.as_ref(), Some(recipient));
        assert_eq!(message.correlation_key, key);
        assert_eq!(message.payload, json!({"amount":amount}));
    }
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    for mutate in 0..4 {
        let mut forged = plan.clone();
        let row = forged.repetition_occurrences.iter_mut().find(|row|
            row.ordinal == 1).unwrap();
        match mutate {
            0 => row.item = json!({"recipient":first.instance_id,"key":"first","amount":11}),
            1 => row.ordinal = 0,
            2 => row.token_id = Uuid::new_v4().to_string(),
            _ => row.group_id = Uuid::new_v4().to_string(),
        }
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    }
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(completed.variables["results"], json!([null,null]));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id)
        .unwrap().repetition_groups[0].completed_count, 2);
}

#[test]
fn collection_receive_arms_and_consumes_each_item_key_from_its_own_token() {
    let fixture = Fixture::new();
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("items".into(), json!(["first", "second"]));
    model.variables.insert("results".into(), json!([]));
    let node = model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap();
    node.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::ReceiveTask {correlation_expression,..} = &mut node.kind else {
        panic!("the repeated node is not ReceiveTask")
    };
    *correlation_expression = "repeat.item".into();
    let version = publish_model(&fixture, &model);
    let started = super::messages::test_support::start_version(&fixture, &version);
    let initial = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    for (ordinal, key) in [(0,"first"),(1,"second")] {
        let occurrence = initial.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let subscription = initial.subscriptions.iter().find(|row|
            row.token_id == occurrence.token_id).unwrap();
        assert_eq!(subscription.correlation_key.as_deref(), Some(key));
    }
    for (ordinal, key) in [(1,"second"),(0,"first")] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap();
        let subscription = snapshot.subscriptions.iter().find(|row|
            row.token_id == occurrence.token_id).unwrap();
        let mut message = super::messages::test_support::envelope(
            super::messages::test_support::catch_target(&version,
                Some(&started.instance_id), Some(&subscription.subscription_id)),
            json!({"key":key}));
        message.correlation_key = key.into();
        super::messages::test_support::send(&fixture, &message);
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
            .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
        let repository::MessageSelection::Ready(prepared) =
            repository::message_snapshot(&fixture.db, &candidate).unwrap() else {
                panic!("the item-keyed Receive is not the selected target")
            };
        let plan = super::messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
        let before = super::signal_proof_tests::all_transition_rows(&fixture);
        let mut forged = plan.clone();
        forged.repetition_occurrences.iter_mut().find(|row|
            row.ordinal == ordinal).unwrap().item = json!("foreign");
        assert!(repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
        repository::deliver_message(&fixture.db, &prepared,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count,
            if ordinal == 1 {1} else {2});
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(completed.variables["results"],
        json!([{"key":"first"},{"key":"second"}]));
}

#[test]
fn collection_subprocess_maps_only_the_frozen_ordinal_item_into_each_child_scope() {
    let fixture = Fixture::new();
    let inner = super::manual_tests::manual_model(None, false);
    let mut model = super::runtime::test_support::embedded_model(inner, "RepeatedScope");
    model.variables.insert("items".into(), json!(["alpha","beta"]));
    model.variables.insert("results".into(), json!([]));
    let node = model.nodes.iter_mut().find(|node| node.id == "RepeatedScope").unwrap();
    node.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::SubProcess {input_mapping,..} = &mut node.kind else {
        panic!("the repeated node is not SubProcess")
    };
    input_mapping.insert("mapped".into(), "repeat.item".into());
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start item-mapped child scope");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.repetition_occurrences.iter_mut().find(|row| row.ordinal == 0)
        .unwrap().item = json!("beta");
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    for (ordinal, item) in [(0,"alpha"),(1,"beta")] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|row|
            row.node_id == "Manual" && row.status == ProcessUserTaskStatus::Open).unwrap();
        let child = snapshot.scopes.iter().find(|scope| scope.scope_id == task.scope_id).unwrap();
        assert_eq!(snapshot.scope_variables[&child.scope_id]["mapped"], item);
        let command = stamp(&format!("acknowledge mapped child {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command), None).unwrap();
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &instance_id, &task.user_task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id)
            .unwrap().repetition_groups[0].completed_count, ordinal + 1);
    }
    let completed = repository::get_instance(&fixture.db, &fixture.owner,
        &instance_id, None).unwrap();
    assert_eq!(completed.variables["results"],
        json!([{"mapped":"alpha"},{"mapped":"beta"}]));
}

#[test]
fn collection_call_maps_each_item_into_its_own_pinned_child() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &super::manual_tests::manual_model(None, false));
    let mut model = super::call_tests::caller(&target, Default::default());
    model.variables.insert("items".into(), json!(["alpha","beta"]));
    model.variables.insert("results".into(), json!([]));
    let node = model.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap();
    node.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.items".into(),
        },
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::CallActivity(ProcessCallActivity {input_mapping,..}) = &mut node.kind else {
        panic!("the repeated node is not CallActivity")
    };
    input_mapping.insert("mapped".into(), "repeat.item".into());
    let started = start_model(&fixture, &model);
    for (ordinal, item) in [(0,"alpha"),(1,"beta")] {
        let parent = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let call = parent.calls.iter().find(|row|
            row.status == tentaflow_protocol::processes::ProcessCallStatus::Waiting).unwrap();
        assert_eq!(call.called_version, target.version);
        let child = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &call.child_instance_id).unwrap();
        assert_eq!(child.instance.variables["mapped"], item);
        let task = child.user_tasks.iter().find(|row|
            row.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp(&format!("acknowledge mapped Call child {ordinal}"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&child, &task.user_task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&child, &task.user_task_id, &command), None).unwrap();
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &call.child_instance_id, &task.user_task_id, child.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        assert_eq!(repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap().repetition_groups[0].completed_count, ordinal + 1);
    }
    let complete = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(complete.variables["results"],
        json!([{"mapped":"alpha"},{"mapped":"beta"}]));
}
