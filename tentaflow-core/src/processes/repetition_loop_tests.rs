// ============ File: repetition_loop_tests.rs — file-backed collection and structured-loop regressions ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessInstanceStatus, ProcessMultiInstanceInput, ProcessNode, ProcessNodeKind,
    ProcessRepeatSpec, ProcessRepetitionGroupStatus, ProcessRepetitionOccurrenceStatus,
    ProcessUserTask, ProcessUserTaskStatus,
};

use super::repetition_tests::sequential_human_model;
use super::repository;
use super::runtime::{self, test_support::{edge, human_input, stamp, start_model, Fixture}};

fn collection_model(owner: &str, items: Value) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = sequential_human_model(owner, 0);
    model.variables.insert("items".into(), items);
    let repeat = model.nodes.iter_mut().find(|node| node.id == "RepeatedWork").unwrap()
        .repeat.as_mut().unwrap();
    let ProcessRepeatSpec::MultiInstance { input, .. } = repeat else {
        panic!("fixture activity is not multi-instance")
    };
    *input = ProcessMultiInstanceInput::CollectionExpression {
        expression: "vars.items".into(),
    };
    model
}

fn loop_model(owner: &str, condition: &str, test_before: bool, max_iterations: u8,
    mapping: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = sequential_human_model(owner, 0);
    model.variables.insert("step".into(), json!(0));
    let node = model.nodes.iter_mut().find(|node| node.id == "RepeatedWork").unwrap();
    node.repeat = Some(ProcessRepeatSpec::StructuredLoop {
        condition: condition.into(), test_before, max_iterations,
        output_collection_variable: "results".into(),
    });
    let ProcessNodeKind::UserTask { output_mapping, .. } = &mut node.kind else {
        panic!("fixture activity is not a UserTask")
    };
    output_mapping.insert("step".into(), mapping.into());
    model
}

fn open_ordinal<'a>(snapshot: &'a repository::RuntimeSnapshot, ordinal: u32)
    -> (&'a repository::RepetitionOccurrence, &'a ProcessUserTask) {
    let occurrence = snapshot.repetition_occurrences.iter()
        .find(|row| row.ordinal == ordinal).expect("factual occurrence");
    let task = snapshot.user_tasks.iter().find(|task|
        task.user_task_id == *occurrence.user_task_id.as_ref().expect("factual Work task")
            && task.status == ProcessUserTaskStatus::Open).expect("open ordinal task");
    (occurrence, task)
}

fn complete_work(fixture: &Fixture, instance_id: &str, ordinal: u32, outputs: &Value) {
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    let (_, task) = open_ordinal(&snapshot, ordinal);
    let command = stamp("complete actual repeated Work item");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, outputs,
        None, at_ms, human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        instance_id, &task.user_task_id, snapshot.instance.revision,
        outputs, None, &plan, at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, instance_id).unwrap();
    assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
    let before_replay = super::call_tests::transition_rows(fixture);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        instance_id, &task.user_task_id, snapshot.instance.revision,
        outputs, None, &plan, at_ms).unwrap();
    assert_eq!(replay.instance.revision, persisted.instance.revision);
    assert_eq!(super::call_tests::transition_rows(fixture), before_replay);
}

#[test]
fn collection_empty_finishes_without_an_ordinal_and_keeps_a_factual_zero_total() {
    let fixture = Fixture::new();
    let started = start_model(&fixture,
        &collection_model(&fixture.owner.user_id, json!([])));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].total_count, Some(0));
    assert_eq!(snapshot.repetition_groups[0].frozen_collection, Some(json!([])));
    assert!(snapshot.repetition_occurrences.is_empty());
    assert_eq!(snapshot.instance.variables["results"], json!([]));
}

#[test]
fn collection_null_object_and_list_items_stay_frozen_after_a_real_sibling_changes_parent_vars() {
    let fixture = Fixture::new();
    let items = json!([null,{"customer":{"key":"ä","flags":[true,null]}},[1,{"nested":[2,3]}]]);
    let mut model = collection_model(&fixture.owner.user_id, items.clone());
    model.nodes.push(ProcessNode { id:"Fork".into(), name:"Fork".into(),
        kind:ProcessNodeKind::ParallelGateway, repeat:None });
    model.nodes.push(ProcessNode { id:"Join".into(), name:"Join".into(),
        kind:ProcessNodeKind::ParallelGateway, repeat:None });
    model.nodes.push(ProcessNode { id:"ChangeItems".into(), name:"Change parent items".into(),
        kind:ProcessNodeKind::UserTask {
            assignee_user_id:Some(fixture.owner.user_id.clone()),
            output_mapping:BTreeMap::from([("items".into(),"outputs.items".into())]),
        }, repeat:None });
    model.sequence_flows = vec![
        edge("ToFork", "Start_1", "Fork"),
        edge("ToRepeat", "Fork", "RepeatedWork"),
        edge("ToChange", "Fork", "ChangeItems"),
        edge("RepeatToJoin", "RepeatedWork", "Join"),
        edge("ChangeToJoin", "ChangeItems", "Join"),
        edge("JoinToEnd", "Join", "End_1"),
    ];
    let started = start_model(&fixture, &model);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(before.repetition_groups[0].frozen_collection, Some(items.clone()));
    let sibling = before.user_tasks.iter().find(|task|
        task.node_id == "ChangeItems" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let command = stamp("change only the parent's live items");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let changed_items = json!([{"changed":"after repetition entry"}]);
    let outputs = json!({"items":changed_items});
    let plan = runtime::plan_user_completion(&before, &sibling.user_task_id, &outputs,
        None, at_ms, human_input(&before, &sibling.user_task_id, &command)).unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &sibling.user_task_id, before.instance.revision,
        &outputs, None, &plan, at_ms).unwrap();
    let changed = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(changed.instance.variables["items"], changed_items);
    assert_eq!(changed.repetition_groups[0].frozen_collection, Some(items.clone()));
    assert_eq!(changed.repetition_groups[0].entry_variables["items"], items);
    for ordinal in 0_u32..3 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (occurrence, task) = open_ordinal(&snapshot, ordinal);
        assert_eq!(occurrence.item, items[usize::try_from(ordinal).unwrap()]);
        let outputs = json!({"ordinal":ordinal});
        let command = stamp("accept frozen collection item");
        let at = chrono::Utc::now().timestamp_millis();
        let canonical = runtime::plan_user_completion(&snapshot, &task.user_task_id,
            &outputs, None, at, human_input(&snapshot, &task.user_task_id, &command)).unwrap();
        if ordinal < 2 {
            let next = canonical.repetition_occurrences.iter()
                .find(|row| row.ordinal == ordinal + 1).unwrap();
            let mut forged = canonical.clone();
            forged.repetition_occurrences.iter_mut()
                .find(|row| row.occurrence_id == next.occurrence_id).unwrap()
                .item = json!({"changed":"after repetition entry"});
            let before_rows = super::call_tests::transition_rows(&fixture);
            assert_eq!(before_rows.len(), 16);
            let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, &forged, at).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("repetition occurrence changed immutable ordinal input")
                || message.contains("repetition item differs from its frozen collection ordinal"),
                "{message}");
            assert_eq!(super::call_tests::transition_rows(&fixture), before_rows);
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            &outputs, None, &canonical, at).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].frozen_collection, Some(items.clone()));
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["items"], changed_items);
    assert_eq!(final_state.variables["results"],
        json!([{"ordinal":0},{"ordinal":1},{"ordinal":2}]));
}

#[test]
fn structured_loop_pretest_false_completes_zero_occurrences() {
    let fixture = Fixture::new();
    let model = loop_model(&fixture.owner.user_id, "vars.step < 0", true, 32,
        "outputs.next_step");
    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].total_count, Some(0));
    assert_eq!(snapshot.repetition_groups[0].loop_state.as_ref().unwrap()["step"], 0);
    assert!(snapshot.repetition_occurrences.is_empty());
    let events = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "repetition_condition_checked"
        && event.data["phase"] == "before_first"
        && event.data["candidate_ordinal"] == 0
        && event.data["matched"] == false).count(), 1);
}

#[test]
fn structured_loop_posttest_uses_each_committed_state_and_next_ordinal_three_times() {
    let fixture = Fixture::new();
    let model = loop_model(&fixture.owner.user_id,
        "vars.step < 3 && repeat.index == vars.step", false, 32,
        "outputs.next_step");
    let started = start_model(&fixture, &model);
    for ordinal in 0_u32..3 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (occurrence, _) = open_ordinal(&snapshot, ordinal);
        assert_eq!(occurrence.input_variables["step"], ordinal);
        let outputs = json!({"next_step":ordinal + 1});
        complete_work(&fixture, &started.instance_id, ordinal, &outputs);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].loop_state.as_ref().unwrap()["step"], ordinal + 1);
        assert_eq!(persisted.instance.variables["step"], 0);
        assert_eq!(persisted.repetition_occurrences.iter().find(|row|
            row.ordinal == ordinal).unwrap().state_after.as_ref().unwrap()["step"], ordinal + 1);
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["step"], 0);
    assert_eq!(final_state.variables["results"],
        json!([{"next_step":1},{"next_step":2},{"next_step":3}]));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 200).unwrap().0;
    let decisions = events.iter().filter(|event| event.kind == "repetition_condition_checked"
        && event.data["phase"] == "after_accepted")
        .map(|event| (event.data["candidate_ordinal"].as_u64().unwrap(),
            event.data["matched"].as_bool().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(decisions, vec![(1, true), (2, true), (3, false)]);
    assert_eq!(events.iter().filter(|event| event.kind == "repetition_completed").count(), 1);
}

#[test]
fn structured_loop_reaches_its_normal_thirty_two_iteration_ceiling() {
    let fixture = Fixture::new();
    let model = loop_model(&fixture.owner.user_id, "true", false, 32,
        "outputs.next_step");
    let started = start_model(&fixture, &model);
    for ordinal in 0_u32..32 {
        complete_work(&fixture, &started.instance_id, ordinal,
            &json!({"next_step":ordinal + 1}));
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].status, ProcessRepetitionGroupStatus::Completed);
    assert_eq!(snapshot.repetition_groups[0].completed_count, 32);
    assert_eq!(snapshot.repetition_groups[0].total_count, Some(32));
    assert_eq!(snapshot.repetition_occurrences.len(), 32);
    assert_eq!(snapshot.instance.variables["results"].as_array().unwrap().len(), 32);
    assert!(snapshot.instance.incidents.is_empty());
}

#[test]
fn nonboolean_posttest_preserves_accepted_source_and_blocks_parent_continuation() {
    let fixture = Fixture::new();
    let model = loop_model(&fixture.owner.user_id, "vars.step", false, 32,
        "outputs.next_step");
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let (_, task) = open_ordinal(&snapshot, 0);
    let outputs = json!({"next_step":1});
    let command = stamp("accept ordinal before nonboolean postcondition");
    let at = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id,
        &outputs, None, at, human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 16);
    let mut forged = plan.clone();
    forged.repetition_groups[0].status = ProcessRepetitionGroupStatus::Completed;
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &forged, at).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &plan, at).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].completed_count, 1);
    assert_eq!(persisted.repetition_occurrences.len(), 1);
    assert_eq!(persisted.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::Completed);
    assert_eq!(persisted.repetition_occurrences[0].aggregate_item, Some(outputs));
    assert_eq!(persisted.instance.variables["step"], 0);
    assert_eq!(persisted.instance.variables["results"], json!([]));
    let events = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    let source_id = persisted.repetition_occurrences[0].accepted_source_event_id.as_deref().unwrap();
    assert!(events.iter().any(|event|
        event.event_id == source_id && event.kind == "user_task_completed"));
    assert_eq!(events.iter().filter(|event| event.kind == "repetition_group_blocked"
        && event.data["code"] == "REPETITION_MAPPING_FAILED").count(), 1);
    assert!(!events.iter().any(|event| event.kind == "repetition_completed"));
}

#[test]
fn loop_mapping_error_keeps_accepted_work_fact_and_no_next_ordinal() {
    let fixture = Fixture::new();
    let model = loop_model(&fixture.owner.user_id, "vars.step < 3", false, 32,
        "1 / 0");
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let (_, task) = open_ordinal(&snapshot, 0);
    let outputs = json!({"next_step":1});
    let command = stamp("accept ordinal with invalid local mapping result");
    let at = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id,
        &outputs, None, at, human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 16);
    let mut forged = plan.clone();
    forged.repetition_occurrences[0].item = json!("foreign loop item");
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &forged, at).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, &plan, at).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].completed_count, 0);
    assert_eq!(persisted.repetition_occurrences.len(), 1);
    assert_eq!(persisted.repetition_occurrences[0].status,
        ProcessRepetitionOccurrenceStatus::AcceptedBlocked);
    assert!(persisted.repetition_occurrences[0].accepted_source_event_id.is_some());
    assert_eq!(persisted.instance.variables["results"], json!([]));
    let events = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    let source_id = persisted.repetition_occurrences[0].accepted_source_event_id.as_deref().unwrap();
    assert_eq!(events.iter().filter(|event|
        event.event_id == source_id && event.kind == "user_task_completed").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "repetition_group_blocked"
        && event.data["code"] == "REPETITION_MAPPING_FAILED").count(), 1);
    assert!(!events.iter().any(|event| event.kind == "repetition_completed"));
}
