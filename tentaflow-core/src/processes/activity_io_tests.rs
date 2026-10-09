// ============ File: activity_io_tests.rs — authenticated activity IO results and retained output failures ============

use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;
use tentaflow_protocol::processes::{
    ProcessActivityIo, ProcessBodyModeling, ProcessDataObject, ProcessDataObjectReference,
    ProcessInputAssociation, ProcessIoDataInput, ProcessIoDataOutput, ProcessMessageStatus,
    ProcessCallStatus, ProcessNodeKind, ProcessOutputAssociation, ProcessTimerStatus,
    ProcessNode, ProcessUserTaskInputValue, ProcessUserTaskStatus,
};

use super::repository::{self, AcceptedInputRef, ProcessPlanInput};
use super::runtime::{
    self,
    test_support::{edge, stamp, start_model, user_model, Fixture},
};

fn configured_user(expression: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = user_model(None);
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Mapped".into(),
            name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Mapped".into(),
            name: None,
            data_object_ref: "Object_Mapped".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Mapped".into(),
            name: None,
            value_expression: expression.into(),
        }],
        input_set_id: "InputSet_Work".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_Work".into(),
        output_set: vec!["Output_Mapped".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Mapped".into(),
            source_output_id: "Output_Mapped".into(),
            target_object_ref_id: "Ref_Mapped".into(),
        }],
        coordinator_output: None,
    });
    model
}

fn configured_user_with_shared_inputs() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = user_model(None);
    model.variables.insert("shared".into(), Value::Null);
    let mut data_objects = Vec::with_capacity(16);
    let mut data_object_references = Vec::with_capacity(16);
    let mut data_inputs = Vec::with_capacity(16);
    let mut input_set = Vec::with_capacity(16);
    let mut input_associations = Vec::with_capacity(16);
    for position in 0..16 {
        let input_id = format!("Input_{position}");
        let object_id = format!("Object_Input_{position}");
        let reference_id = format!("Ref_Input_{position}");
        data_objects.push(ProcessDataObject {
            id: object_id.clone(),
            name: None,
        });
        data_object_references.push(ProcessDataObjectReference {
            id: reference_id.clone(),
            name: None,
            data_object_ref: object_id,
            variable_binding_key: Some("shared".into()),
        });
        data_inputs.push(ProcessIoDataInput {
            id: input_id.clone(),
            name: Some(format!("Input {position}")),
        });
        input_set.push(input_id.clone());
        input_associations.push(ProcessInputAssociation::DirectRef {
            id: format!("Association_Input_{position}"),
            source_object_ref_id: reference_id,
            target_input_id: input_id,
        });
    }
    model.modeling = Some(ProcessBodyModeling {
        data_objects,
        data_object_references,
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs,
        data_outputs: Vec::new(),
        input_set_id: "InputSet_Work".into(),
        input_set,
        output_set_id: "OutputSet_Work".into(),
        output_set: Vec::new(),
        input_associations,
        output_associations: Vec::new(),
        coordinator_output: None,
    });
    model
}

fn configured_script(
    script: &str,
    legacy_expression: &str,
    io_expression: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::model::starter_model();
    model.variables.insert("seed".into(), json!(4));
    model.variables.insert("answer".into(), Value::Null);
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Compute".into(),
            name: "Compute".into(),
            kind: ProcessNodeKind::ScriptTask {
                script: script.into(),
                output_mapping: BTreeMap::from([("answer".into(), legacy_expression.into())]),
            },
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows = vec![
        edge("ToCompute", "Start_1", "Compute"),
        edge("FromCompute", "Compute", "End_1"),
    ];
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Script".into(),
            name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Script".into(),
            name: None,
            data_object_ref: "Object_Script".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Script".into(),
            name: None,
            value_expression: io_expression.into(),
        }],
        input_set_id: "InputSet_Script".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_Script".into(),
        output_set: vec!["Output_Script".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Script".into(),
            source_output_id: "Output_Script".into(),
            target_object_ref_id: "Ref_Script".into(),
        }],
        coordinator_output: None,
    });
    model
}

fn mapped_parent_modeling(object_id: &str, reference_id: &str, variable: &str) -> ProcessBodyModeling {
    ProcessBodyModeling {
        data_objects: vec![ProcessDataObject { id: object_id.into(), name: None }],
        data_object_references: vec![ProcessDataObjectReference {
            id: reference_id.into(), name: None, data_object_ref: object_id.into(),
            variable_binding_key: Some(variable.into()),
        }],
        ..ProcessBodyModeling::default()
    }
}

fn configured_parent_output_io(output_expression: &str, output_id: &str,
    association_id: &str, reference_id: &str) -> ProcessActivityIo {
    ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: output_id.into(), name: None, value_expression: output_expression.into(),
        }],
        input_set_id: "InputSet_Parent".into(), input_set: Vec::new(),
        output_set_id: "OutputSet_Parent".into(), output_set: vec![output_id.into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: association_id.into(), source_output_id: output_id.into(),
            target_object_ref_id: reference_id.into(),
        }],
        coordinator_output: None,
    }
}

#[test]
fn subprocess_activity_io_preserves_body_base_and_binds_authored_input() {
    let fixture = Fixture::new();
    let mut child_body = super::model::starter_model();
    child_body.variables.insert("answer".into(), json!(42));
    child_body.variables.insert("captured".into(), Value::Null);
    let mut model = runtime::test_support::embedded_model(child_body, "Scope");
    model.variables.insert("seed".into(), json!(4));
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(mapped_parent_modeling("Object_Scope", "Ref_Scope", "mapped"));
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope")
        .expect("configured SubProcess node");
    let ProcessNodeKind::SubProcess { input_mapping, .. } = &mut scope.kind else {
        unreachable!("embedded fixture must contain a SubProcess");
    };
    input_mapping.insert("captured".into(), "inputs.Input_Scope".into());
    let mut io = configured_parent_output_io(
        "outputs.captured", "Output_Scope", "Association_Scope", "Ref_Scope",
    );
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Scope".into(),
        name: Some("SubProcess input".into()),
    });
    io.input_set.push("Input_Scope".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Scope_Input".into(),
        from_expression: "vars.seed + 10".into(),
        target_input_id: "Input_Scope".into(),
    });
    scope.activity_io = Some(io);

    let started = start_model(&fixture, &model);
    assert_eq!(started.status, tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(started.variables["seed"], json!(4));
    assert_eq!(started.variables["mapped"], json!(14));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    let child_scope_id = events.iter().find(|event| event.kind == "scope_result_accepted")
        .and_then(|event| event.data["child_scope_id"].as_str())
        .expect("SubProcess result source")
        .to_owned();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    // A completed child leaves the live snapshot, so its retained locals are read from the row.
    let stored: String = reopened.read().unwrap().query_row(
        "SELECT local_variables_json FROM bpmn_scopes WHERE instance_id=?1 AND scope_id=?2",
        [&started.instance_id, &child_scope_id], |row| row.get(0)).unwrap();
    let child_variables: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(child_variables["answer"], json!(42));
    assert_eq!(child_variables["captured"], json!(14));
    assert!(snapshot.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "subprocess"
            && witness.phase == "output_applied"
            && witness.result == Some(child_variables.clone())));
}

#[test]
fn repeated_user_activity_io_maps_each_ordinal_and_the_coordinator_once() {
    let fixture = Fixture::new();
    let mut model = user_model(None);
    model.variables.insert("seed".into(), json!(4));
    model.variables.insert("mapped_ordinal".into(), Value::Null);
    model.variables.insert("coordinator_result".into(), Value::Null);
    model.variables.insert("results".into(), json!([]));
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![
            ProcessDataObject { id: "Object_Ordinal".into(), name: None },
            ProcessDataObject { id: "Object_Coordinator".into(), name: None },
        ],
        data_object_references: vec![
            ProcessDataObjectReference {
                id: "Ref_Ordinal".into(), name: None,
                data_object_ref: "Object_Ordinal".into(),
                variable_binding_key: Some("mapped_ordinal".into()),
            },
            ProcessDataObjectReference {
                id: "Ref_Coordinator".into(), name: None,
                data_object_ref: "Object_Coordinator".into(),
                variable_binding_key: Some("coordinator_result".into()),
            },
        ],
        ..ProcessBodyModeling::default()
    });
    let work = model.nodes.iter_mut().find(|node| node.id == "Work").unwrap();
    work.repeat = Some(tentaflow_protocol::processes::ProcessRepeatSpec::MultiInstance {
        mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
        input: tentaflow_protocol::processes::ProcessMultiInstanceInput::Cardinality { count: 1 },
        output_collection_variable: "results".into(),
    });
    work.activity_io = Some(ProcessActivityIo {
        data_inputs: vec![ProcessIoDataInput {
            id: "Input_Ordinal".into(),
            name: Some("Ordinal input".into()),
        }],
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Ordinal".into(), name: None,
            value_expression: "outputs.answer".into(),
        }],
        input_set_id: "InputSet_Ordinal".into(), input_set: vec!["Input_Ordinal".into()],
        output_set_id: "OutputSet_Ordinal".into(),
        output_set: vec!["Output_Ordinal".into()],
        input_associations: vec![ProcessInputAssociation::CelAssignment {
            id: "Association_Ordinal_Input".into(),
            from_expression: "vars.seed + 10".into(),
            target_input_id: "Input_Ordinal".into(),
        }],
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Ordinal".into(),
            source_output_id: "Output_Ordinal".into(),
            target_object_ref_id: "Ref_Ordinal".into(),
        }],
        coordinator_output: Some(tentaflow_protocol::processes::ProcessCoordinatorOutputIo {
            data_outputs: vec![ProcessIoDataOutput {
                id: "Output_Coordinator".into(), name: None,
                value_expression: "outputs".into(),
            }],
            output_set_id: "OutputSet_Coordinator".into(),
            output_set: vec!["Output_Coordinator".into()],
            output_associations: vec![ProcessOutputAssociation {
                id: "Association_Coordinator".into(),
                source_output_id: "Output_Coordinator".into(),
                target_object_ref_id: "Ref_Coordinator".into(),
            }],
        }),
    });

    let started = runtime::test_support::start_model(&fixture, &model);
    assert_eq!(started.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Waiting);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = before.user_tasks.iter().find(|task|
        task.node_id == "Work" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let detail = repository::get_user_task(&fixture.db, &fixture.owner,
        &started.instance_id, &task.user_task_id).unwrap();
    assert!(matches!(detail.activity_inputs.as_slice(), [input]
        if matches!(&input.value,
            ProcessUserTaskInputValue::Present(value) if value == &json!(14))));
    let outputs = json!({"answer": 7});
    let command = stamp("complete repeated user activity IO");
    let plan = runtime::plan_user_completion(
        &before, &task.user_task_id, &outputs, None, started.updated_at_ms + 1,
        runtime::test_support::human_input(&before, &task.user_task_id, &command), None,
    ).unwrap();
    repository::complete_user_task(
        &fixture.db, &fixture.owner, &command, &started.instance_id,
        &task.user_task_id, before.instance.revision, &outputs, None,
        ProcessPlanInput::Supplied(&plan), started.updated_at_ms + 1,
    ).unwrap();

    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(after.instance.variables["mapped_ordinal"], json!(7));
    assert_eq!(after.instance.variables["results"], json!([{"answer": 7}]));
    assert_eq!(after.instance.variables["coordinator_result"], json!([{"answer": 7}]));
    assert_eq!(after.repetition_groups.iter().filter(|group|
        group.status == tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Completed).count(), 1);
    assert_eq!(after.repetition_occurrences.iter().filter(|occurrence|
        occurrence.status == tentaflow_protocol::processes::ProcessRepetitionOccurrenceStatus::Completed).count(), 1);
    assert_eq!(after.activity_io_witnesses.iter().filter(|witness|
        witness.phase == "output_applied").count(), 2);
    assert_eq!(after.activity_io_witnesses.iter().filter(|witness|
        witness.phase_owner == "ordinal" && witness.phase == "output_applied").count(), 1);
    assert_eq!(after.activity_io_witnesses.iter().filter(|witness|
        witness.phase_owner == "coordinator" && witness.phase == "output_applied").count(), 1);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event|
        event.kind == "repetition_completed").count(), 1);
    assert_eq!(events.iter().filter(|event|
        event.kind == "activity_io_output_applied").count(), 2);
}

#[test]
fn configured_coordinator_requires_one_result_fact_and_aggregate_effect() {
    let fixture = Fixture::new();
    let mut model = user_model(None);
    model.variables.insert("mapped_ordinal".into(), Value::Null);
    model.variables.insert("coordinator_result".into(), Value::Null);
    model.variables.insert("results".into(), json!([]));
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![
            ProcessDataObject { id: "Object_Ordinal".into(), name: None },
            ProcessDataObject { id: "Object_Coordinator".into(), name: None },
        ],
        data_object_references: vec![
            ProcessDataObjectReference {
                id: "Ref_Ordinal".into(), name: None,
                data_object_ref: "Object_Ordinal".into(),
                variable_binding_key: Some("mapped_ordinal".into()),
            },
            ProcessDataObjectReference {
                id: "Ref_Coordinator".into(), name: None,
                data_object_ref: "Object_Coordinator".into(),
                variable_binding_key: Some("coordinator_result".into()),
            },
        ],
        ..ProcessBodyModeling::default()
    });
    let work = model.nodes.iter_mut().find(|node| node.id == "Work").unwrap();
    work.repeat = Some(tentaflow_protocol::processes::ProcessRepeatSpec::MultiInstance {
        mode: tentaflow_protocol::processes::ProcessMultiInstanceMode::Sequential,
        input: tentaflow_protocol::processes::ProcessMultiInstanceInput::Cardinality { count: 1 },
        output_collection_variable: "results".into(),
    });
    work.activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Ordinal".into(), name: None,
            value_expression: "outputs.answer".into(),
        }],
        input_set_id: "InputSet_Ordinal".into(), input_set: Vec::new(),
        output_set_id: "OutputSet_Ordinal".into(),
        output_set: vec!["Output_Ordinal".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Ordinal".into(),
            source_output_id: "Output_Ordinal".into(),
            target_object_ref_id: "Ref_Ordinal".into(),
        }],
        coordinator_output: Some(tentaflow_protocol::processes::ProcessCoordinatorOutputIo {
            data_outputs: vec![ProcessIoDataOutput {
                id: "Output_Coordinator".into(), name: None,
                value_expression: "outputs".into(),
            }],
            output_set_id: "OutputSet_Coordinator".into(),
            output_set: vec!["Output_Coordinator".into()],
            output_associations: vec![ProcessOutputAssociation {
                id: "Association_Coordinator".into(),
                source_output_id: "Output_Coordinator".into(),
                target_object_ref_id: "Ref_Coordinator".into(),
            }],
        }),
    });
    let started = runtime::test_support::start_model(&fixture, &model);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = before.user_tasks.iter().find(|task|
        task.node_id == "Work" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let command = stamp("reject omitted repetition coordinator proof");
    let valid = runtime::plan_user_completion(
        &before, &task.user_task_id, &json!({"answer": 7}), None,
        started.updated_at_ms + 1,
        runtime::test_support::human_input(&before, &task.user_task_id, &command), None,
    ).unwrap();
    let coordinator_event_index = valid.events.iter().position(|event|
        event.kind == "repetition_completed").expect("coordinator completion event");
    let before_rows = super::call_tests::transition_rows(&fixture);

    let mut omitted_fact = valid.clone();
    omitted_fact.activity_io_results.retain(|fact|
        fact.result_event_index != coordinator_event_index);
    assert!(repository::complete_user_task(
        &fixture.db, &fixture.owner, &command, &started.instance_id,
        &task.user_task_id, before.instance.revision, &json!({"answer": 7}), None,
        ProcessPlanInput::Supplied(&omitted_fact), started.updated_at_ms + 1,
    ).unwrap_err().to_string().contains("coordinator"));
    assert_eq!(super::call_tests::transition_rows(&fixture), before_rows);

    let mut omitted_effect = valid.clone();
    for effect in &mut omitted_effect.variable_effects {
        if let repository::VariableEffect::RepetitionAggregate {
            event_index, activity_io_outputs, output_event_index, ..
        } = effect {
            if *event_index == coordinator_event_index {
                *activity_io_outputs = None;
                *output_event_index = None;
            }
        }
    }
    assert!(repository::complete_user_task(
        &fixture.db, &fixture.owner, &command, &started.instance_id,
        &task.user_task_id, before.instance.revision, &json!({"answer": 7}), None,
        ProcessPlanInput::Supplied(&omitted_effect), started.updated_at_ms + 1,
    ).unwrap_err().to_string().contains("coordinator"));
    assert_eq!(super::call_tests::transition_rows(&fixture), before_rows);
}

#[test]
fn subprocess_io_accepts_the_ordered_child_end_ledger_before_mapping() {
    let fixture = Fixture::new();
    let mut child_body = super::model::starter_model();
    child_body.variables.insert("answer".into(), json!(42));
    let mut model = runtime::test_support::embedded_model(child_body, "Scope");
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(mapped_parent_modeling("Object_Scope", "Ref_Scope", "mapped"));
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    scope.activity_io = Some(configured_parent_output_io(
        "outputs.answer", "Output_Scope", "Association_Scope", "Ref_Scope",
    ));

    let started = runtime::test_support::start_model(&fixture, &model);
    assert_eq!(started.status, tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(started.variables["mapped"], json!(42));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    let accepted_index = events.iter().position(|event| event.kind == "scope_result_accepted")
        .expect("SubProcess must publish its accepted child result");
    let mapped_index = events.iter().position(|event| event.kind == "activity_io_output_applied")
        .expect("SubProcess must publish its applied association");
    let completed_index = events.iter().position(|event| event.kind == "scope_completed")
        .expect("SubProcess must publish its child completion");
    assert!(accepted_index < mapped_index && mapped_index < completed_index);
    let child_scope_id = events[accepted_index].data["child_scope_id"].as_str().unwrap();
    let end_sources = events[accepted_index].data["end_source_event_ids"].as_array().unwrap();
    assert!(!end_sources.is_empty());
    assert_eq!(events[accepted_index].data["final_end_source_event_id"],
        end_sources.last().unwrap().clone());
    assert_eq!(events.iter().filter(|event| event.kind == "end_reached"
        && event.scope_id == child_scope_id).count(), end_sources.len());
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let witness = snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "subprocess" && row.phase == "output_applied")
        .expect("SubProcess witness must be durable after mapping");
    assert_eq!(witness.result_sources.as_ref().unwrap().len(), end_sources.len());
    assert_eq!(witness.result, Some(json!({"answer": 42})));
    assert_eq!(snapshot.scopes.iter().find(|scope| scope.scope_id == child_scope_id)
        .unwrap().status, tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
}

#[test]
fn same_plan_subprocess_io_rejects_forged_pre_return_status_and_revision() {
    let fixture = Fixture::new();
    let mut child_body = super::model::starter_model();
    child_body.variables.insert("answer".into(), json!(42));
    let mut model = runtime::test_support::embedded_model(child_body, "Scope");
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(mapped_parent_modeling("Object_Scope", "Ref_Scope", "mapped"));
    model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap().activity_io =
        Some(configured_parent_output_io(
            "outputs.answer", "Output_Scope", "Association_Scope", "Ref_Scope",
        ));
    let version = runtime::test_support::publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let command = stamp("start configured SubProcess with forged child proof");
    let valid = runtime::plan_start(
        &version.model, &version.model.process_id,
        runtime::test_support::ordinary_start_id(&version.model), &instance_id,
        &fixture.owner, &version.definition_id, version.version, variables.clone(),
        runtime::StartCause::Manual, at_ms,
        runtime::test_support::manual_input(&command), None,
    ).unwrap();
    let accepted_index = valid.events.iter().position(|event|
        event.kind == "scope_result_accepted").unwrap();
    let result_index = valid.activity_io_results.iter().position(|fact|
        fact.child_scope_id.is_some()).unwrap();
    let before = super::call_tests::transition_rows(&fixture);

    let mut forged_status = valid.clone();
    forged_status.activity_io_results[result_index].child_status = Some("completed".into());
    forged_status.events[accepted_index].data["child_status"] = json!("completed");
    assert!(repository::start_instance(
        &fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        ProcessPlanInput::Supplied(&forged_status), at_ms,
    ).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);

    let mut forged_revision = valid.clone();
    forged_revision.activity_io_results[result_index].child_expected_revision = Some(7);
    forged_revision.events[accepted_index].data["child_revision"] = json!(7);
    assert!(repository::start_instance(
        &fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        ProcessPlanInput::Supplied(&forged_revision), at_ms,
    ).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);

    let committed = repository::start_instance(
        &fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        ProcessPlanInput::Supplied(&valid), at_ms,
    ).unwrap();
    assert_eq!(committed.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
}

#[test]
fn blocked_subprocess_io_keeps_the_child_drained_and_parent_waiting() {
    let fixture = Fixture::new();
    let mut child_body = super::model::starter_model();
    child_body.variables.insert("answer".into(), json!(42));
    let mut model = runtime::test_support::embedded_model(child_body, "Scope");
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(mapped_parent_modeling("Object_Scope", "Ref_Scope", "mapped"));
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    scope.activity_io = Some(configured_parent_output_io(
        "1 / 0", "Output_Scope", "Association_Scope", "Ref_Scope",
    ));

    let started = runtime::test_support::start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "scope_result_accepted").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "scope_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    let child_scope_id = events.iter().find(|event| event.kind == "scope_result_accepted")
        .and_then(|event| event.data["child_scope_id"].as_str()).unwrap();
    assert_eq!(snapshot.scopes.iter().find(|scope| scope.scope_id == child_scope_id)
        .unwrap().status, tentaflow_protocol::processes::ProcessInstanceStatus::Waiting);
    assert!(snapshot.tokens.iter().any(|token|
        token.node_id == "Scope" && token.status == "waiting"));
    assert!(snapshot.activity_io_witnesses.iter().any(|row|
        row.activity_kind == "subprocess" && row.phase == "output_blocked"));

    let retry = runtime::plan_advance(&snapshot, chrono::Utc::now().timestamp_millis(), None)
        .unwrap();
    assert!(retry.activity_io_results.is_empty());
    assert!(!retry.events.iter().any(|event|
        matches!(event.kind.as_str(), "scope_result_accepted" | "scope_completed")));
}

#[test]
fn subprocess_io_retains_persisted_child_locals_on_legacy_mapping_failure() {
    let fixture = Fixture::new();
    let child = user_model(None);
    let mut model = runtime::test_support::embedded_model(child, "Scope");
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(mapped_parent_modeling("Object_Scope", "Ref_Scope", "mapped"));
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    scope.activity_io = Some(configured_parent_output_io(
        "outputs.answer", "Output_Scope", "Association_Scope", "Ref_Scope",
    ));
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind else {
        unreachable!("embedded fixture must contain a SubProcess");
    };
    output_mapping.insert("legacy".into(), "1 / 0".into());

    let started = runtime::test_support::start_model(&fixture, &model);
    assert_eq!(started.status, tentaflow_protocol::processes::ProcessInstanceStatus::Waiting);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
        .unwrap();
    let child_scope = before.scopes.iter().find(|scope|
        scope.subprocess_node_id.as_deref() == Some("Scope"))
        .expect("embedded child scope must be persisted before its return");
    let task = before.user_tasks.iter().find(|task|
        task.scope_id == child_scope.scope_id && task.status == ProcessUserTaskStatus::Open)
        .expect("persisted child must expose its user task");
    let outputs = json!({"answer": 42});
    let command = stamp("complete persisted SubProcess child with legacy mapping failure");
    let at_ms = started.updated_at_ms + 1;
    let plan = runtime::plan_user_completion(
        &before,
        &task.user_task_id,
        &outputs,
        None,
        at_ms,
        runtime::test_support::human_input(&before, &task.user_task_id, &command),
        None,
    ).unwrap();
    let transition_before_forged = super::call_tests::transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.scope_return_failures[0].child_locals = json!({"answer": 43});
    assert!(repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Supplied(&forged),
        at_ms,
    ).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), transition_before_forged);
    repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Supplied(&plan),
        at_ms,
    ).unwrap();

    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
        .unwrap();
    let retained_child = after.scopes.iter().find(|scope|
        scope.scope_id == child_scope.scope_id).expect("child scope must remain persisted");
    assert_eq!(retained_child.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Incident);
    assert_eq!(after.scope_variables.get(&child_scope.scope_id), Some(&json!({"answer": 42})));
    assert!(after.tokens.iter().any(|token|
        token.node_id == "Scope" && token.status == "waiting"));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &started.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "scope_result_accepted").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "SCOPE_RETURN_ERROR").count(), 1);
    let witness = after.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "subprocess" && row.phase == "result_accepted")
        .expect("retained SubProcess result must preserve its accepted witness");
    assert_eq!(witness.retained_route_kind.as_deref(), Some("final_scope_return_failure"));
}

#[test]
fn call_activity_io_preserves_child_base_and_binds_authored_input() {
    let fixture = Fixture::new();
    let mut target_model = user_model(None);
    target_model.variables.insert("captured".into(), Value::Null);
    let target = runtime::test_support::publish_model(&fixture, &target_model);
    let mut caller = super::call_tests::caller(&target, BTreeMap::new());
    caller.variables.insert("seed".into(), json!(4));
    caller.variables.insert("mapped".into(), Value::Null);
    caller.modeling = Some(mapped_parent_modeling("Object_Call", "Ref_Call", "mapped"));
    let call = caller.nodes.iter_mut().find(|node| node.id == "Call_1")
        .expect("configured Call node");
    let ProcessNodeKind::CallActivity(call_kind) = &mut call.kind else {
        unreachable!("configured caller must contain a CallActivity");
    };
    call_kind.input_mapping.insert("captured".into(), "inputs.Input_Call".into());
    let mut io = configured_parent_output_io(
        "outputs.answer", "Output_Call", "Association_Call", "Ref_Call",
    );
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Call".into(),
        name: Some("Call input".into()),
    });
    io.input_set.push("Input_Call".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Call_Input".into(),
        from_expression: "vars.seed + 10".into(),
        target_input_id: "Input_Call".into(),
    });
    call.activity_io = Some(io);

    let caller_version = runtime::test_support::publish_model(&fixture, &caller);
    let parent = super::messages::test_support::start_version(&fixture, &caller_version);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child.instance.variables["captured"], json!(14));
    let task = child.user_tasks.iter().find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("called child must expose its real work task");
    let outputs = json!({"answer": "accepted"});
    let command = stamp("complete child for body input Call IO");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&child, &task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&child, &task.user_task_id, &command), None)
        .unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &command, &child_id,
        &task.user_task_id, child.instance.revision, &outputs, None,
        ProcessPlanInput::Supplied(&plan), at_ms).unwrap();

    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted_child = repository::get_instance(&reopened, &fixture.owner, &child_id, None)
        .unwrap();
    assert_eq!(persisted_child.variables["captured"], json!(14));
    let persisted_parent = repository::runtime_snapshot(&reopened, &fixture.owner,
        &parent.instance_id).unwrap();
    assert_eq!(persisted_parent.instance.variables["seed"], json!(4));
    assert_eq!(persisted_parent.instance.variables["mapped"], json!("accepted"));
    assert!(persisted_parent.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "call"
            && witness.phase == "output_applied"
            && witness.result == Some(persisted_child.variables.clone())));
}

#[test]
fn call_io_uses_the_child_instance_completed_uuid_as_its_source_event() {
    let fixture = Fixture::new();
    let target = runtime::test_support::publish_model(&fixture, &user_model(None));
    let mut caller = super::call_tests::caller(&target, BTreeMap::new());
    caller.variables.insert("mapped".into(), Value::Null);
    caller.modeling = Some(mapped_parent_modeling("Object_Call", "Ref_Call", "mapped"));
    caller.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap().activity_io =
        Some(configured_parent_output_io(
            "outputs.answer", "Output_Call", "Association_Call", "Ref_Call",
        ));
    let caller_version = runtime::test_support::publish_model(&fixture, &caller);
    let parent = super::messages::test_support::start_version(&fixture, &caller_version);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let task = child.user_tasks.iter().find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("called child must expose its real work task");
    let outputs = json!({"answer": "accepted"});
    let command = stamp("complete child for configured Call IO");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&child, &task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&child, &task.user_task_id, &command), None)
        .unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &command, &child_id,
        &task.user_task_id, child.instance.revision, &outputs, None,
        ProcessPlanInput::Supplied(&plan), at_ms).unwrap();

    let child_events = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 100)
        .unwrap().0;
    let child_completed = child_events.iter().find(|event| event.kind == "instance_completed")
        .expect("called child must have one instance_completed source");
    let parent_events = repository::list_events(&fixture.db, &fixture.owner,
        &parent.instance_id, 0, 100).unwrap().0;
    let accepted = parent_events.iter().find(|event| event.kind == "call_result_accepted")
        .expect("Call must publish its accepted child result");
    let applied = parent_events.iter().find(|event| event.kind == "activity_io_output_applied")
        .expect("Call must publish its applied association");
    let returned = parent_events.iter().find(|event| event.kind == "call_returned")
        .expect("Call must return after its association mapping");
    assert!(accepted.seq < applied.seq && applied.seq < returned.seq);
    assert_eq!(accepted.data["child_completed_event_id"], child_completed.event_id);
    let parent_snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    assert_eq!(parent_snapshot.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    assert_eq!(parent_snapshot.instance.variables["mapped"], json!("accepted"));
    let witness = parent_snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "call" && row.phase == "output_applied")
        .expect("Call witness must be durable after mapping");
    assert_eq!(witness.source_instance_id.as_deref(), Some(child_id.as_str()));
    assert_eq!(witness.source_event_id.as_deref(), Some(child_completed.event_id.as_str()));
    let expected_sources = vec![child_completed.event_id.clone()];
    assert_eq!(witness.result_sources.as_ref(), Some(&expected_sources));
}

#[test]
fn blocked_call_io_keeps_the_completed_child_and_parent_waiting() {
    let fixture = Fixture::new();
    let target = runtime::test_support::publish_model(&fixture, &user_model(None));
    let mut caller = super::call_tests::caller(&target, BTreeMap::new());
    caller.variables.insert("mapped".into(), Value::Null);
    caller.modeling = Some(mapped_parent_modeling("Object_Call", "Ref_Call", "mapped"));
    caller.nodes.iter_mut().find(|node| node.id == "Call_1").unwrap().activity_io =
        Some(configured_parent_output_io(
            "1 / 0", "Output_Call", "Association_Call", "Ref_Call",
        ));
    let caller_version = runtime::test_support::publish_model(&fixture, &caller);
    let parent = super::messages::test_support::start_version(&fixture, &caller_version);
    let child_id = super::call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let task = child.user_tasks.iter().find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("called child must expose its real work task");
    let outputs = json!({"answer": "accepted"});
    let command = stamp("complete child for blocked Call IO");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&child, &task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&child, &task.user_task_id, &command), None)
        .unwrap();
    repository::complete_user_task(&fixture.db, &fixture.owner, &command, &child_id,
        &task.user_task_id, child.instance.revision, &outputs, None,
        ProcessPlanInput::Supplied(&plan), at_ms).unwrap();

    let child_snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id)
        .unwrap();
    assert_eq!(child_snapshot.instance.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed);
    let parent_snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    let call = parent_snapshot.calls.iter().find(|call| call.call_node_id == "Call_1")
        .expect("parent must retain its Call activation");
    assert_eq!(call.status, ProcessCallStatus::ReturnIncident);
    assert!(parent_snapshot.tokens.iter().any(|token|
        token.node_id == "Call_1" && token.status == "waiting"));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &parent.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "call_result_accepted").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "call_returned").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    let child_completed = repository::list_events(&fixture.db, &fixture.owner, &child_id,
        0, 100).unwrap().0.into_iter().find(|event| event.kind == "instance_completed")
        .expect("blocked Call must retain the child completion source");
    let witness = parent_snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "call" && row.phase == "output_blocked")
        .expect("blocked Call witness must be durable");
    assert_eq!(witness.source_instance_id.as_deref(), Some(child_id.as_str()));
    assert_eq!(witness.source_event_id.as_deref(), Some(child_completed.event_id.as_str()));
    assert_eq!(witness.result_sources.as_ref(), Some(&vec![child_completed.event_id]));

    let retry = runtime::plan_advance(&parent_snapshot,
        chrono::Utc::now().timestamp_millis(), None).unwrap();
    assert!(retry.activity_io_results.is_empty());
    assert!(!retry.events.iter().any(|event|
        matches!(event.kind.as_str(), "call_result_accepted" | "call_returned")));
}

fn configured_send(
    target_definition: &str,
    target_instance: &str,
    expression: &str,
    with_boundary: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = if with_boundary {
        super::send_boundary_tests::timer_bound_send_model(target_definition, target_instance)
    } else {
        super::send_receive_tests::send_model(target_definition, target_instance)
    };
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Send".into(), name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Send".into(), name: None,
            data_object_ref: "Object_Send".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Send".into(), name: None,
            value_expression: expression.into(),
        }],
        input_set_id: "InputSet_Send".into(), input_set: Vec::new(),
        output_set_id: "OutputSet_Send".into(),
        output_set: vec!["Output_Send".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Send".into(),
            source_output_id: "Output_Send".into(),
            target_object_ref_id: "Ref_Send".into(),
        }],
        coordinator_output: None,
    });
    model
}

#[test]
fn send_activity_io_binds_authored_input_into_prepared_message_body() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture, &receiver_version);
    let mut model = configured_send(
        &receiver_version.definition_id, &receiver.instance_id, "outputs", false,
    );
    model.variables.insert("seed".into(), json!(4));
    let send = model.nodes.iter_mut().find(|node| node.id == "Send_1")
        .expect("configured Send node");
    let io = send.activity_io.as_mut().expect("configured Send IO");
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Send_Body".into(),
        name: Some("Send body input".into()),
    });
    io.input_set.push("Input_Send_Body".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Send_Body_Input".into(),
        from_expression: "vars.seed + 10".into(),
        target_input_id: "Input_Send_Body".into(),
    });
    let ProcessNodeKind::SendTask { payload_expression, .. } = &mut send.kind else {
        unreachable!("configured Send node has the SendTask kind");
    };
    *payload_expression = "inputs.Input_Send_Body".into();

    let sender = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &sender.instance_id)
        .unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &sender.instance_id, 0, 100)
        .unwrap().0;
    let admitted = events.iter().find(|event| event.kind == "send_task_admitted")
        .expect("persisted Send admission");
    let message_id = admitted.data["message_id"].as_str().unwrap();
    let message = repository::get_message(
        &reopened, &fixture.owner, &fixture.owner.user_id, message_id,
    )
    .unwrap();
    assert_eq!(message.payload, Some(json!(14)));
    assert_eq!(snapshot.instance.variables["mapped"], json!(14));
    assert!(snapshot.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "send"
            && witness.phase == "output_applied"
            && witness.result == Some(json!(14))));
}

#[test]
fn send_io_maps_only_the_admitted_payload_and_retains_one_outbox_message() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture,
        &receiver_version);
    let sender = start_model(&fixture, &configured_send(
        &receiver_version.definition_id, &receiver.instance_id, "outputs.value", false,
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &sender.instance_id)
        .unwrap();
    assert_eq!(snapshot.instance.variables["mapped"], 42);
    let events = repository::list_events(&reopened, &fixture.owner, &sender.instance_id, 0, 100)
        .unwrap().0;
    let captured = events.iter().position(|event| event.kind == "activity_io_input_captured")
        .unwrap();
    let admitted = events.iter().position(|event| event.kind == "send_task_admitted")
        .unwrap();
    let applied = events.iter().position(|event| event.kind == "activity_io_output_applied")
        .unwrap();
    assert!(captured < admitted && admitted < applied);
    let message_id = events[admitted].data["message_id"].as_str().unwrap();
    let outbox = repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap();
    assert_eq!(outbox.message.status, ProcessMessageStatus::Pending);
    assert_eq!(outbox.payload, Some(json!({"value":42})));
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "output_applied"
            && row.resource_id.as_deref() == Some(message_id)
            && row.result == Some(json!({"value":42}))).count(), 1);
}

#[test]
fn send_io_output_blockage_keeps_the_admitted_outbox_and_wait_without_resending() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture,
        &receiver_version);
    let sender = start_model(&fixture, &configured_send(
        &receiver_version.definition_id, &receiver.instance_id, "1 / 0", false,
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &sender.instance_id)
        .unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &sender.instance_id, 0, 100)
        .unwrap().0;
    let admitted = events.iter().filter(|event| event.kind == "send_task_admitted")
        .collect::<Vec<_>>();
    assert_eq!(admitted.len(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    let message_id = admitted[0].data["message_id"].as_str().unwrap();
    let outbox = repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap();
    assert_eq!(outbox.message.status, ProcessMessageStatus::Pending);
    assert_eq!(outbox.payload, Some(json!({"value":42})));
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "output_blocked"
            && row.resource_id.as_deref() == Some(message_id)
            && row.result == Some(json!({"value":42}))).count(), 1);
    assert!(snapshot.tokens.iter().any(|token|
        token.node_id == "Send_1" && token.status == "waiting"));
    assert_eq!(snapshot.instance.variables["mapped"], Value::Null);
}

#[test]
fn pending_send_io_admission_preserves_its_captured_input_and_rejects_forged_outbox_facts() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture, &receiver_version);
    let sender = start_model(&fixture, &configured_send(
        &receiver_version.definition_id, &receiver.instance_id, "outputs.value", true,
    ));
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &sender.instance_id).unwrap();
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let timer = snapshot.timers.iter().find(|timer|
        timer.node_id == "SendTimer" && timer.token_id.as_deref() == Some(waiting.token_id.as_str())).unwrap();
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &sender.instance_id, 0, 100).unwrap().0;
    let captured = events.iter().position(|event|
        event.kind == "activity_io_input_captured").unwrap();
    let pending = events.iter().position(|event| event.kind == "send_task_pending").unwrap();
    assert!(captured < pending);
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "input_captured"
            && row.token_id == waiting.token_id).count(), 1);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let entry = AcceptedInputRef::SendAdmission {
        instance_id: sender.instance_id.clone(),
        scope_id: waiting.scope_id.clone(),
        node_id: waiting.node_id.clone(),
        pending_token_id: waiting.token_id.clone(),
        pending_event_id: events[pending].event_id.clone(),
        expected_instance_revision: snapshot.instance.revision,
    };
    let plan = runtime::plan_send_admission(&snapshot, &waiting.token_id,
        &events[pending].event_id, at_ms, entry, None, None).unwrap();
    let admitted = plan.events.iter().position(|event|
        event.kind == "send_task_admitted").unwrap();
    let applied = plan.events.iter().position(|event|
        event.kind == "activity_io_output_applied").unwrap();
    assert!(admitted < applied);
    assert_eq!(plan.create_messages.len(), 1);
    let before = super::call_tests::transition_rows(&fixture);
    let mut wrong_id = plan.clone();
    wrong_id.create_messages[0].message.message_id = Uuid::new_v4().to_string();
    let mut wrong_payload = plan.clone();
    wrong_payload.create_messages[0].message.payload = json!({"value": 43});
    let mut wrong_event = plan.clone();
    wrong_event.event_ids.insert(admitted, Uuid::new_v4().to_string());
    for (label, forged) in [
        ("outbox message ID", wrong_id),
        ("accepted payload", wrong_payload),
        ("admission event UUID", wrong_event),
    ] {
        let rejected = repository::admit_pending_send(&fixture.db, &waiting.token_id,
            at_ms, ProcessPlanInput::Supplied(&forged)).unwrap_err();
        assert!(!format!("{rejected:#}").is_empty(), "{label} lacked a refusal");
        assert_eq!(super::call_tests::transition_rows(&fixture), before,
            "{label} changed durable rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::admit_pending_send(&reopened, &waiting.token_id,
        at_ms, ProcessPlanInput::Supplied(&plan)).unwrap().unwrap();
    assert_eq!(committed.instance.variables["mapped"], 42);
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        at_ms, ProcessPlanInput::Supplied(&plan)).unwrap().is_none());
    let final_events = repository::list_events(&reopened, &fixture.owner,
        &sender.instance_id, 0, 100).unwrap().0;
    assert_eq!(final_events.iter().filter(|event|
        event.kind == "send_task_admitted").count(), 1);
    let final_snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &sender.instance_id).unwrap();
    assert_eq!(final_snapshot.timers.iter().find(|candidate|
        candidate.timer_id == timer.timer_id).unwrap().status,
        ProcessTimerStatus::Cancelled);
    assert_eq!(final_snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "output_applied"
            && row.result == Some(json!({"value": 42}))).count(), 1);
    let message_id = final_events.iter().find(|event|
        event.kind == "send_task_admitted").unwrap().data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        ProcessMessageStatus::Pending);
}

#[test]
fn pending_send_io_blockage_keeps_one_outbox_and_its_live_boundary() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture, &receiver_version);
    let sender = start_model(&fixture, &configured_send(
        &receiver_version.definition_id, &receiver.instance_id, "1 / 0", true,
    ));
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &sender.instance_id).unwrap();
    let waiting = before.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    let timer = before.timers.iter().find(|timer|
        timer.node_id == "SendTimer" && timer.token_id.as_deref() == Some(waiting.token_id.as_str())).unwrap();
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &sender.instance_id, 0, 100).unwrap().0;
    let pending = events.iter().find(|event| event.kind == "send_task_pending").unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let entry = AcceptedInputRef::SendAdmission {
        instance_id: sender.instance_id.clone(), scope_id: waiting.scope_id.clone(),
        node_id: waiting.node_id.clone(), pending_token_id: waiting.token_id.clone(),
        pending_event_id: pending.event_id.clone(),
        expected_instance_revision: before.instance.revision,
    };
    let plan = runtime::plan_send_admission(&before, &waiting.token_id,
        &pending.event_id, at_ms, entry, None, None).unwrap();
    assert_eq!(plan.create_messages.len(), 1);
    assert_eq!(plan.activity_io_results.len(), 1);
    assert_eq!(plan.events.iter().filter(|event|
        event.kind == "activity_io_output_applied").count(), 0);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    repository::admit_pending_send(&reopened, &waiting.token_id,
        at_ms, ProcessPlanInput::Supplied(&plan)).unwrap().unwrap();
    assert!(repository::admit_pending_send(&reopened, &waiting.token_id,
        at_ms, ProcessPlanInput::Supplied(&plan)).unwrap().is_none());
    let after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &sender.instance_id).unwrap();
    assert!(after.tokens.iter().any(|token|
        token.token_id == waiting.token_id && token.status == "waiting"));
    assert!(after.timers.iter().any(|candidate|
        candidate.timer_id == timer.timer_id && candidate.status == timer.status));
    assert_eq!(after.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "output_blocked"
            && row.result == Some(json!({"value": 42}))).count(), 1);
    let history = repository::list_events(&reopened, &fixture.owner,
        &sender.instance_id, 0, 100).unwrap().0;
    assert_eq!(history.iter().filter(|event|
        event.kind == "send_task_admitted").count(), 1);
    let admitted = history.iter().find(|event|
        event.kind == "send_task_admitted").unwrap();
    let message_id = admitted.data["message_id"].as_str().unwrap();
    assert_eq!(repository::get_message(&reopened, &fixture.owner,
        &fixture.owner.user_id, message_id).unwrap().message.status,
        ProcessMessageStatus::Pending);
}

#[test]
fn send_io_input_failure_creates_no_pending_boundary_or_outbox() {
    let fixture = Fixture::new();
    let receiver_version = runtime::test_support::publish_model(
        &fixture, &super::send_receive_tests::receive_model());
    let receiver = super::messages::test_support::start_version(&fixture, &receiver_version);
    let mut model = configured_send(&receiver_version.definition_id,
        &receiver.instance_id, "outputs.value", true);
    let io = model.nodes[1].activity_io.as_mut().unwrap();
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Send".into(), name: None,
    });
    io.input_set.push("Input_Send".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Send_Input".into(),
        from_expression: "1 / 0".into(),
        target_input_id: "Input_Send".into(),
    });
    let sender = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &sender.instance_id).unwrap();
    let failed = snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "send" && row.phase == "input_failed")
        .collect::<Vec<_>>();
    assert_eq!(failed.len(), 1);
    let waiting = snapshot.tokens.iter().find(|token|
        token.node_id == "Send_1" && token.status == "waiting").unwrap();
    assert_eq!(failed[0].token_id, waiting.token_id);
    assert!(!snapshot.timers.iter().any(|timer| timer.node_id == "SendTimer"));
    let outbox_count: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id=?1",
        [&sender.instance_id], |row| row.get(0)).unwrap();
    assert_eq!(outbox_count, 0);
    let events = repository::list_events(&reopened, &fixture.owner,
        &sender.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event|
        event.kind == "send_task_pending" || event.kind == "send_task_admitted").count(), 0);
    let incident = events.iter().filter(|event|
        event.kind == "incident" && event.data["code"] == "ACTIVITY_IO_INPUT_FAILED"
            && event.data["activation_token_id"].as_str() == Some(waiting.token_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(incident.len(), 1);
    assert_eq!(incident[0].data["incident_id"].as_str(), failed[0].incident_id.as_deref());
}

#[test]
fn script_io_applies_only_after_a_factual_evaluated_result() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_script(
        "vars.seed + 1", "outputs", "outputs",
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    assert_eq!(snapshot.instance.variables["answer"], 5);
    assert_eq!(snapshot.instance.variables["mapped"], 5);
    let events = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap().0;
    let captured = events.iter().position(|event| event.kind == "activity_io_input_captured")
        .unwrap();
    let evaluated = events.iter().position(|event| event.kind == "script_result_evaluated")
        .unwrap();
    let applied = events.iter().position(|event| event.kind == "activity_io_output_applied")
        .unwrap();
    let completed = events.iter().position(|event| event.kind == "script_completed").unwrap();
    assert!(captured < evaluated && evaluated < applied && applied < completed);
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "script" && row.phase == "output_applied").count(), 1);
}

#[test]
fn script_activity_io_binds_authored_association_value_after_reopen() {
    let fixture = Fixture::new();
    let mut model = configured_script("inputs.Input_Script", "outputs", "outputs");
    let script = model.nodes.iter_mut().find(|node| node.id == "Compute")
        .expect("configured Script node");
    script.activity_io = Some(ProcessActivityIo {
        data_inputs: vec![ProcessIoDataInput {
            id: "Input_Script".into(),
            name: Some("Script input".into()),
        }],
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Script".into(),
            name: Some("Script output".into()),
            value_expression: "outputs".into(),
        }],
        input_set_id: "InputSet_Script".into(),
        input_set: vec!["Input_Script".into()],
        output_set_id: "OutputSet_Script".into(),
        output_set: vec!["Output_Script".into()],
        input_associations: vec![ProcessInputAssociation::CelAssignment {
            id: "Association_Script_Input".into(),
            from_expression: "vars.seed + 10".into(),
            target_input_id: "Input_Script".into(),
        }],
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Script_Output".into(),
            source_output_id: "Output_Script".into(),
            target_object_ref_id: "Ref_Script".into(),
        }],
        coordinator_output: None,
    });

    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();

    assert_eq!(snapshot.instance.variables["seed"], json!(4));
    assert_eq!(snapshot.instance.variables["answer"], json!(14));
    assert_eq!(snapshot.instance.variables["mapped"], json!(14));
    let witness = snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "script" && row.phase == "output_applied")
        .expect("persisted Script input witness");
    assert!(matches!(
        witness.input_values.as_ref().and_then(|inputs| inputs.first())
            .map(|input| &input.observed),
        Some(repository::IoObservedValue::Present { value }) if value == &json!(14)
    ));
    let events = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap().0;
    let result = events.iter().find(|event| event.kind == "script_result_evaluated")
        .expect("persisted Script result event");
    assert_eq!(result.data["outputs"], json!(14));
}

#[test]
fn user_activity_io_reader_preserves_authored_input_after_reopen() {
    let fixture = Fixture::new();
    let mut model = user_model(None);
    model.variables.insert("seed".into(), json!(4));
    let work = model.nodes.iter_mut().find(|node| node.id == "Work")
        .expect("configured User node");
    work.activity_io = Some(ProcessActivityIo {
        data_inputs: vec![ProcessIoDataInput {
            id: "Input_User".into(),
            name: Some("User input".into()),
        }],
        data_outputs: Vec::new(),
        input_set_id: "InputSet_User".into(),
        input_set: vec!["Input_User".into()],
        output_set_id: "OutputSet_User".into(),
        output_set: Vec::new(),
        input_associations: vec![ProcessInputAssociation::CelAssignment {
            id: "Association_User_Input".into(),
            from_expression: "vars.seed + 10".into(),
            target_input_id: "Input_User".into(),
        }],
        output_associations: Vec::new(),
        coordinator_output: None,
    });

    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "Work")
        .expect("persisted User task");
    let detail = repository::get_user_task(
        &reopened, &fixture.owner, &started.instance_id, &task.user_task_id,
    )
    .unwrap();
    assert!(matches!(
        detail.activity_inputs.as_slice(),
        [input] if matches!(&input.value,
            ProcessUserTaskInputValue::Present(value) if value == &json!(14))
    ));
    assert!(snapshot.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "user"
            && witness.input_values.as_ref().is_some_and(|inputs|
                matches!(inputs.as_slice(), [input]
                    if matches!(&input.observed,
                        repository::IoObservedValue::Present { value }
                            if value == &json!(14))))));
}

#[test]
fn script_io_retains_evaluated_result_after_legacy_mapping_failure() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_script(
        "vars.seed + 1", "1 / 0", "outputs",
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "script_result_evaluated").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "script_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "SCRIPT_MAPPING_FAILED").count(), 1);
    assert!(snapshot.tokens.iter().any(|token|
        token.node_id == "Compute" && token.status == "waiting"));
    assert_eq!(snapshot.instance.variables["answer"], Value::Null);
    assert_eq!(snapshot.instance.variables["mapped"], Value::Null);
}

#[test]
fn script_io_blocks_a_failing_generic_association_without_completing_the_body() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_script(
        "vars.seed + 1", "outputs", "1 / 0",
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "script_result_evaluated").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "script_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "script" && row.phase == "output_blocked").count(), 1);
    assert!(snapshot.tokens.iter().any(|token|
        token.node_id == "Compute" && token.status == "waiting"));
    assert_eq!(snapshot.instance.variables["answer"], Value::Null);
    assert_eq!(snapshot.instance.variables["mapped"], Value::Null);
}

#[test]
fn script_io_evaluation_failure_retains_only_its_captured_input_and_parked_source() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_script(
        "1 / 0", "outputs", "outputs",
    ));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
        .unwrap();
    let events = repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_input_captured").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "script_result_evaluated").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "script_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "SCRIPT_EVALUATION_FAILED").count(), 1);
    assert!(snapshot.tokens.iter().any(|token|
        token.node_id == "Compute" && token.status == "waiting"));
    assert_eq!(snapshot.activity_io_witnesses.iter().filter(|row|
        row.activity_kind == "script" && row.phase == "input_captured").count(), 1);
}

#[test]
fn script_io_mapping_failure_rejects_a_forged_action_source_without_writing_rows() {
    let fixture = Fixture::new();
    let model = configured_script("vars.seed + 1", "1 / 0", "outputs");
    let version = runtime::test_support::publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let at_ms = chrono::Utc::now().timestamp_millis();
    let command = stamp("start configured Script with legacy mapping failure");
    let valid = runtime::plan_start(
        &version.model,
        &version.model.process_id,
        runtime::test_support::ordinary_start_id(&version.model),
        &instance_id,
        &fixture.owner,
        &version.definition_id,
        version.version,
        variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        runtime::test_support::manual_input(&command),
        None,
    ).unwrap();
    let route = valid.events.iter().position(|event| event.kind == "incident"
        && event.data["code"] == "SCRIPT_MAPPING_FAILED").unwrap();
    let mut forged = valid.clone();
    forged.event_sources.insert(route, Uuid::new_v4().to_string());
    let before_rows = super::call_tests::transition_rows(&fixture);
    assert!(repository::start_instance(
        &fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        ProcessPlanInput::Supplied(&forged), at_ms,
    ).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before_rows);
    repository::start_instance(
        &fixture.db, &fixture.owner, &command, &instance_id,
        &version.definition_id, version.version, &variables, None, None,
        ProcessPlanInput::Supplied(&valid), at_ms,
    ).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    let witness = snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "script" && row.retained_route_kind.as_deref()
            == Some("final_script_mapping_failure")).unwrap();
    assert_eq!(witness.result_presence.as_deref(), Some("present"));
    assert_eq!(witness.result, Some(json!(5)));
    assert!(witness.result_sha256.as_deref().is_some_and(|hash| hash.len() == 64));
    let events = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 100)
        .unwrap().0;
    let incident = events.iter().find(|event| event.kind == "incident"
        && event.data["code"] == "SCRIPT_MAPPING_FAILED").unwrap();
    assert_eq!(witness.retained_route_event_id.as_deref(), Some(incident.event_id.as_str()));
    assert!(incident.data["incident_id"].as_str().is_some() && witness.incident_id.is_none());
    let parked_id = incident.data["parked_token_id"].as_str().unwrap();
    assert!(snapshot.tokens.iter().any(|token|
        token.token_id == parked_id && token.status == "waiting"));
    assert_eq!(events.iter().filter(|event| event.kind == "activity_io_output_applied").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "script_completed").count(), 0);
}

#[test]
fn user_task_wire_budget_is_rechecked_by_writer_and_survives_near_boundary_reopen() {
    let fixture = Fixture::new();
    let model = configured_user_with_shared_inputs();
    let version = runtime::test_support::publish_model(&fixture, &model);
    let start_node_id = runtime::test_support::ordinary_start_id(&version.model).to_owned();
    // Sixteen captured copies must fit one 384 KiB history event, so the planner and the
    // writer share the same boundary well below the task detail wire budget.
    let low_variables = json!({"shared": "x".repeat(8 * 1024)});
    let forged_instance_id = Uuid::new_v4().to_string();
    let forged_command = stamp("reject forged oversized user task detail");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let valid_plan = runtime::plan_start(
        &version.model,
        &version.model.process_id,
        &start_node_id,
        &forged_instance_id,
        &fixture.owner,
        &version.definition_id,
        version.version,
        low_variables.clone(),
        runtime::StartCause::Manual,
        at_ms,
        runtime::test_support::manual_input(&forged_command),
        None,
    )
    .unwrap();
    let low_inputs = valid_plan
        .activity_io_inputs
        .iter()
        .find_map(|fact| match fact {
            repository::ActivityIoInputFact::Captured { inputs, .. } => Some(inputs.clone()),
            repository::ActivityIoInputFact::Failed { .. } => None,
        })
        .unwrap();
    assert_eq!(low_inputs.len(), 16);

    let high_value = Value::String("x".repeat(super::model::MAX_VARIABLE_BYTES - 64));
    let high_variables = json!({"shared": high_value});
    let high_inputs = low_inputs
        .iter()
        .cloned()
        .map(|mut input| {
            input.observed = repository::IoObservedValue::Present {
                value: high_variables["shared"].clone(),
            };
            input
        })
        .collect::<Vec<_>>();
    let mut forged = valid_plan.clone();
    forged.start_variables = Some(high_variables.clone());
    forged.variables = high_variables.clone();
    for fact in &mut forged.activity_io_inputs {
        if let repository::ActivityIoInputFact::Captured { inputs, .. } = fact {
            *inputs = high_inputs.clone();
        }
    }
    for event in &mut forged.events {
        if event.kind == "activity_io_input_captured" {
            event.data["inputs"] = serde_json::to_value(&high_inputs).unwrap();
        }
    }
    for task in &mut forged.create_user_tasks {
        for input in &mut task.activity_inputs {
            input.value = ProcessUserTaskInputValue::Present(high_variables["shared"].clone());
        }
    }
    let before = super::call_tests::transition_rows(&fixture);
    let error = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &forged_command,
        &forged_instance_id,
        &version.definition_id,
        version.version,
        &high_variables,
        None,
        None,
        repository::ProcessPlanInput::Supplied(&forged),
        at_ms,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("wire budget"), "{error:#}");
    assert_eq!(super::call_tests::transition_rows(&fixture), before);

    let probe_command = stamp("find canonical task wire boundary");
    let probe_instance_id = Uuid::new_v4().to_string();
    let probe = |bytes| {
        runtime::plan_start(
            &version.model,
            &version.model.process_id,
            &start_node_id,
            &probe_instance_id,
            &fixture.owner,
            &version.definition_id,
            version.version,
            json!({"shared": "x".repeat(bytes)}),
            runtime::StartCause::Manual,
            at_ms,
            runtime::test_support::manual_input(&probe_command),
            None,
        )
        .is_ok()
    };
    let maximum_variable_bytes = super::model::MAX_VARIABLE_BYTES - 64;
    assert!(probe(0));
    assert!(!probe(maximum_variable_bytes));
    let mut low = 0;
    let mut high = maximum_variable_bytes;
    while low < high {
        let middle = low + (high - low + 1) / 2;
        if probe(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    assert!(low < maximum_variable_bytes);
    assert!(probe(low));
    assert!(!probe(low + 1));

    let canonical_instance_id = Uuid::new_v4().to_string();
    let canonical_command = stamp("persist canonical near boundary task");
    let canonical_variables = json!({"shared": "x".repeat(low)});
    let started = repository::start_instance(
        &fixture.db,
        &fixture.owner,
        &canonical_command,
        &canonical_instance_id,
        &version.definition_id,
        version.version,
        &canonical_variables,
        None,
        None,
        repository::ProcessPlanInput::Canonical,
        at_ms + 1,
    )
    .unwrap();
    let task_id = started.user_tasks.first().unwrap().user_task_id.clone();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let task = repository::get_user_task(
        &reopened,
        &fixture.owner,
        &canonical_instance_id,
        &task_id,
    )
    .unwrap();
    assert_eq!(task.activity_inputs.len(), 16);
    assert!(task.activity_inputs.iter().all(|input| matches!(
        &input.value,
        ProcessUserTaskInputValue::Present(value) if value == &canonical_variables["shared"]
    )));
    runtime::ensure_user_task_wire_budget(&task).unwrap();
}

#[test]
fn user_output_applies_only_after_one_accepted_result() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_user("outputs.answer"));
    let before =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let task = before
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Work" && task.status == ProcessUserTaskStatus::Open)
        .unwrap();
    let outputs = json!({"answer":"accepted"});
    let command = stamp("accept configured User output");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut forged = runtime::plan_user_completion(
        &before,
        &task.user_task_id,
        &outputs,
        None,
        at_ms,
        runtime::test_support::human_input(&before, &task.user_task_id, &command),
        None,
    )
    .unwrap();
    let accepted_index = forged
        .events
        .iter()
        .position(|event| event.kind == "user_result_accepted")
        .unwrap();
    forged.events[accepted_index].data["outputs"] = json!({"answer":"forged"});
    let original_rows = super::call_tests::transition_rows(&fixture);
    assert!(repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Supplied(&forged),
        at_ms
    )
    .is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), original_rows);
    let result = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(result.instance.variables["answer"], "accepted");
    assert_eq!(result.instance.variables["mapped"], "accepted");
    let events = repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap()
        .0;
    let accepted = events
        .iter()
        .position(|event| event.kind == "user_result_accepted")
        .unwrap();
    let applied = events
        .iter()
        .position(|event| event.kind == "activity_io_output_applied")
        .unwrap();
    let completed = events
        .iter()
        .position(|event| event.kind == "user_task_completed")
        .unwrap();
    assert!(accepted < applied && applied < completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(snapshot.instance.variables["mapped"], "accepted");
    assert_eq!(
        snapshot
            .activity_io_witnesses
            .iter()
            .filter(|row| row.activity_kind == "user" && row.phase == "output_applied")
            .count(),
        1
    );
}

#[test]
fn blocked_user_output_retains_original_response_and_rejects_a_new_completion() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &configured_user("1 / 0"));
    let before =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let task = before
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Work" && task.status == ProcessUserTaskStatus::Open)
        .unwrap();
    let outputs = json!({"answer":"accepted"});
    let command = stamp("block configured User output");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let blocked = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    let retained =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    let open = retained
        .user_tasks
        .iter()
        .find(|row| row.user_task_id == task.user_task_id)
        .unwrap();
    assert_eq!(open.status, ProcessUserTaskStatus::Open);
    assert_eq!(open.outputs, outputs);
    assert!(!open.can_complete);
    assert!(retained
        .tokens
        .iter()
        .any(
            |token| Some(token.token_id.as_str()) == open.token_id.as_deref()
                && token.status == "waiting"
        ));
    assert_eq!(
        retained
            .activity_io_witnesses
            .iter()
            .filter(|row| row.activity_kind == "user"
                && row.phase == "output_blocked"
                && row.resource_id.as_deref() == Some(task.user_task_id.as_str()))
            .count(),
        1
    );
    let events = repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
        .unwrap()
        .0;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "user_result_accepted")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "user_task_completed")
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "incident"
                && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED")
            .count(),
        1
    );
    let replay = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &started.instance_id,
        &task.user_task_id,
        before.instance.revision,
        &outputs,
        None,
        ProcessPlanInput::Canonical,
        at_ms,
    )
    .unwrap();
    assert_eq!(replay.instance.revision, blocked.instance.revision);
    let rows = super::call_tests::transition_rows(&fixture);
    let changed = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &stamp("replace blocked User output"),
        &started.instance_id,
        &task.user_task_id,
        blocked.instance.revision,
        &json!({"answer":"replacement"}),
        None,
        ProcessPlanInput::Canonical,
        at_ms + 1,
    )
    .unwrap_err();
    assert!(format!("{changed:#}").contains("result was accepted and its output is blocked"));
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted =
        repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id).unwrap();
    assert_eq!(
        persisted
            .user_tasks
            .iter()
            .find(|row| row.user_task_id == task.user_task_id)
            .unwrap()
            .outputs,
        outputs
    );
    let witness = persisted
        .activity_io_witnesses
        .iter()
        .find(|row| {
            row.activity_kind == "user"
                && row.phase == "output_blocked"
                && row.resource_id.as_deref() == Some(task.user_task_id.as_str())
        })
        .unwrap();
    assert_eq!(witness.result.as_ref(), Some(&outputs));
    assert_eq!(witness.result_presence.as_deref(), Some("present"));
    let observed = repository::IoObservedValue::Present { value: outputs };
    let expected_hash = repository::activity_io_result_sha256(&observed).unwrap();
    assert_eq!(
        witness.result_sha256.as_deref(),
        Some(expected_hash.as_str())
    );
}

fn configured_manual(expression: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::manual_tests::manual_model(None, false);
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Manual".into(),
            name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Manual".into(),
            name: None,
            data_object_ref: "Object_Manual".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Manual".into(),
            name: None,
            value_expression: expression.into(),
        }],
        input_set_id: "InputSet_Manual".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_Manual".into(),
        output_set: vec!["Output_Manual".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Manual".into(),
            source_output_id: "Output_Manual".into(),
            target_object_ref_id: "Ref_Manual".into(),
        }],
        coordinator_output: None,
    });
    model
}

#[test]
fn manual_activity_io_reader_preserves_authored_input_after_reopen() {
    let fixture = Fixture::new();
    let mut model = configured_manual("42");
    model.variables.insert("seed".into(), json!(4));
    let manual = model.nodes.iter_mut().find(|node| node.id == "Manual")
        .expect("configured Manual node");
    let io = manual.activity_io.as_mut().expect("configured Manual IO");
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Manual".into(),
        name: Some("Manual input".into()),
    });
    io.input_set.push("Input_Manual".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Manual_Input".into(),
        from_expression: "vars.seed + 10".into(),
        target_input_id: "Input_Manual".into(),
    });

    let (instance_id, _, _) = super::manual_tests::start_manual(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "Manual")
        .expect("persisted Manual task");
    let detail = repository::get_user_task(
        &reopened, &fixture.owner, &instance_id, &task.user_task_id,
    )
    .unwrap();
    assert!(matches!(
        detail.activity_inputs.as_slice(),
        [input] if matches!(&input.value,
            ProcessUserTaskInputValue::Present(value) if value == &json!(14))
    ));
    assert!(snapshot.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "manual"
            && witness.input_values.as_ref().is_some_and(|inputs|
                matches!(inputs.as_slice(), [input]
                    if matches!(&input.observed,
                        repository::IoObservedValue::Present { value }
                            if value == &json!(14))))));
}

#[test]
fn manual_acknowledgment_maps_factual_null_before_completion() {
    let fixture = Fixture::new();
    let (instance_id, _, _) =
        super::manual_tests::start_manual(&fixture, &configured_manual("42"));
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = before.user_tasks.iter().find(|task| task.node_id == "Manual").unwrap();
    let command = stamp("acknowledge configured Manual IO");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let mut forged = runtime::plan_manual_acknowledgment(
        &before, &task.user_task_id, &fixture.owner.user_id, at_ms,
        super::manual_tests::manual_entry(&before, &task.user_task_id, &command), None,
    ).unwrap();
    let accepted_index = forged.events.iter().position(|event|
        event.kind == "manual_result_accepted").unwrap();
    forged.events[accepted_index].data["result_presence"] = json!("missing");
    let rows = super::call_tests::transition_rows(&fixture);
    assert!(repository::acknowledge_manual_task(
        &fixture.db, &fixture.owner, &command, &instance_id, &task.user_task_id,
        before.instance.revision, ProcessPlanInput::Supplied(&forged), at_ms,
    ).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    let completed = repository::acknowledge_manual_task(
        &fixture.db, &fixture.owner, &command, &instance_id, &task.user_task_id,
        before.instance.revision, ProcessPlanInput::Canonical, at_ms,
    ).unwrap();
    assert_eq!(completed.instance.variables["mapped"], 42);
    let events = repository::list_events(&fixture.db, &fixture.owner, &instance_id, 0, 100)
        .unwrap().0;
    let accepted = events.iter().position(|event| event.kind == "manual_result_accepted").unwrap();
    let applied = events.iter().position(|event| event.kind == "activity_io_output_applied").unwrap();
    let acknowledged = events.iter().position(|event| event.kind == "manual_task_acknowledged").unwrap();
    assert!(accepted < applied && applied < acknowledged);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    let witness = snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "manual" && row.phase == "output_applied").unwrap();
    assert_eq!(witness.result_presence.as_deref(), Some("present"));
    assert_eq!(witness.result.as_ref(), Some(&Value::Null));
    let expected_hash = repository::activity_io_result_sha256(
        &repository::IoObservedValue::Present { value: Value::Null }).unwrap();
    assert_eq!(witness.result_sha256.as_deref(), Some(expected_hash.as_str()));
}

#[test]
fn blocked_manual_output_keeps_accepted_null_and_rejects_another_acknowledgment() {
    let fixture = Fixture::new();
    let (instance_id, _, _) =
        super::manual_tests::start_manual(&fixture, &configured_manual("1 / 0"));
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let task = before.user_tasks.iter().find(|task| task.node_id == "Manual").unwrap();
    let command = stamp("block configured Manual output");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let blocked = repository::acknowledge_manual_task(
        &fixture.db, &fixture.owner, &command, &instance_id, &task.user_task_id,
        before.instance.revision, ProcessPlanInput::Canonical, at_ms,
    ).unwrap();
    let retained = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    let open = retained.user_tasks.iter().find(|row|
        row.user_task_id == task.user_task_id).unwrap();
    assert_eq!(open.status, ProcessUserTaskStatus::Open);
    assert_eq!(open.outputs, Value::Null);
    assert!(!open.can_complete);
    assert!(retained.tokens.iter().any(|token|
        open.token_id.as_deref() == Some(token.token_id.as_str())
            && token.status == "waiting"));
    let witness = retained.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "manual" && row.phase == "output_blocked"
            && row.resource_id.as_deref() == Some(task.user_task_id.as_str())).unwrap();
    assert_eq!(witness.result_presence.as_deref(), Some("present"));
    assert_eq!(witness.result.as_ref(), Some(&Value::Null));
    let expected_hash = repository::activity_io_result_sha256(
        &repository::IoObservedValue::Present { value: Value::Null }).unwrap();
    assert_eq!(witness.result_sha256.as_deref(), Some(expected_hash.as_str()));
    let events = repository::list_events(&fixture.db, &fixture.owner, &instance_id, 0, 100)
        .unwrap().0;
    assert_eq!(events.iter().filter(|event|
        event.kind == "manual_result_accepted").count(), 1);
    assert_eq!(events.iter().filter(|event|
        event.kind == "manual_task_acknowledged").count(), 0);
    assert_eq!(events.iter().filter(|event|
        event.kind == "incident" && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    let replay = repository::acknowledge_manual_task(
        &fixture.db, &fixture.owner, &command, &instance_id, &task.user_task_id,
        before.instance.revision, ProcessPlanInput::Canonical, at_ms,
    ).unwrap();
    assert_eq!(replay.instance.revision, blocked.instance.revision);
    let rows = super::call_tests::transition_rows(&fixture);
    let changed = repository::acknowledge_manual_task(
        &fixture.db, &fixture.owner, &stamp("replace blocked Manual acknowledgment"),
        &instance_id, &task.user_task_id, blocked.instance.revision,
        ProcessPlanInput::Canonical, at_ms + 1,
    ).unwrap_err();
    assert!(format!("{changed:#}").contains("result was accepted and its output is blocked"));
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert!(persisted.activity_io_witnesses.iter().any(|row|
        row.witness_id == witness.witness_id && row.phase == "output_blocked"
            && row.result_presence.as_deref() == Some("present")
            && row.result.as_ref() == Some(&Value::Null)));
}

fn configured_receive(expression: &str) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = super::send_receive_tests::receive_model();
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![ProcessDataObject {
            id: "Object_Receive".into(),
            name: None,
        }],
        data_object_references: vec![ProcessDataObjectReference {
            id: "Ref_Receive".into(),
            name: None,
            data_object_ref: "Object_Receive".into(),
            variable_binding_key: Some("mapped".into()),
        }],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: vec![ProcessIoDataOutput {
            id: "Output_Receive".into(),
            name: None,
            value_expression: expression.into(),
        }],
        input_set_id: "InputSet_Receive".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_Receive".into(),
        output_set: vec!["Output_Receive".into()],
        input_associations: Vec::new(),
        output_associations: vec![ProcessOutputAssociation {
            id: "Association_Receive".into(),
            source_output_id: "Output_Receive".into(),
            target_object_ref_id: "Ref_Receive".into(),
        }],
        coordinator_output: None,
    });
    model
}

#[test]
fn receive_activity_io_binds_authored_input_into_correlation_after_reopen() {
    use super::messages::{self, test_support::{catch_target, envelope, send, start_version}};
    use super::repository::MessageSelection;

    let fixture = Fixture::new();
    let mut model = configured_receive("outputs.value");
    model.variables.insert("case_key".into(), json!("raw-case"));
    let receive = model.nodes.iter_mut().find(|node| node.id == "Catch_1")
        .expect("configured Receive node");
    let ProcessNodeKind::ReceiveTask { correlation_expression, .. } = &mut receive.kind else {
        unreachable!("configured Receive node has the ReceiveTask kind");
    };
    *correlation_expression = "inputs.Input_Receive".into();
    let io = receive.activity_io.as_mut().expect("configured Receive IO");
    io.data_inputs.push(ProcessIoDataInput {
        id: "Input_Receive".into(),
        name: Some("Receive correlation input".into()),
    });
    io.input_set.push("Input_Receive".into());
    io.input_associations.push(ProcessInputAssociation::CelAssignment {
        id: "Association_Receive_Input".into(),
        from_expression: "'case-1'".into(),
        target_input_id: "Input_Receive".into(),
    });

    let version = runtime::test_support::publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    assert_eq!(receiver.subscriptions[0].correlation_key.as_deref(), Some("case-1"));
    let message = envelope(
        catch_target(&version, Some(&receiver.instance_id),
            Some(&receiver.subscriptions[0].subscription_id)),
        json!({"value": 42}),
    );
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|item| item.key.message_id == message.message_id)
        .expect("configured Receive message candidate");
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db, &candidate)
        .unwrap() else { panic!("authored Receive correlation must select its message") };
    let plan = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    let delivered = repository::deliver_message(
        &fixture.db, &prepared, ProcessPlanInput::Supplied(&plan), at_ms,
    )
    .unwrap()
    .expect("configured Receive should complete exactly once");
    assert_eq!(delivered.transition.instance.variables["mapped"], json!(42));

    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &receiver.instance_id)
        .unwrap();
    assert_eq!(persisted.instance.variables["case_key"], json!("raw-case"));
    assert_eq!(persisted.instance.variables["mapped"], json!(42));
    assert!(persisted.activity_io_witnesses.iter().any(|witness|
        witness.activity_kind == "receive"
            && witness.phase == "output_applied"
            && witness.result == Some(json!({"value": 42}))));
}

#[test]
fn receive_output_uses_one_accepted_message_before_completion() {
    use super::messages::{self, test_support::{catch_target, envelope, send, start_version}};
    use super::repository::{MessageSelection, ProcessPlanInput};
    let fixture = Fixture::new();
    let version = runtime::test_support::publish_model(&fixture,
        &configured_receive("outputs.value"));
    let receiver = start_version(&fixture, &version);
    let subscription = receiver.subscriptions.iter().find(|sub|
        sub.node_id == "Catch_1").unwrap();
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&subscription.subscription_id)), json!({"value":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("configured Receive lost its exact message") };
    let mut forged = messages::plan_message_delivery(&prepared, at_ms, None).unwrap();
    let accepted_index = forged.events.iter().position(|event|
        event.kind == "message_delivered").unwrap();
    forged.events[accepted_index].data["payload"] = json!({"value":"forged"});
    let rows = super::call_tests::transition_rows(&fixture);
    assert!(repository::deliver_message(&fixture.db, &prepared,
        ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    let delivered = repository::deliver_message(&fixture.db, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().unwrap();
    assert_eq!(delivered.transition.instance.variables["mapped"], 42);
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &receiver.instance_id, 0, 100).unwrap().0;
    let accepted = events.iter().position(|event| event.kind == "message_delivered").unwrap();
    let applied = events.iter().position(|event|
        event.kind == "activity_io_output_applied").unwrap();
    let completed = events.iter().position(|event|
        event.kind == "receive_task_completed").unwrap();
    assert!(accepted < applied && applied < completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &receiver.instance_id).unwrap();
    let witness = snapshot.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "receive" && row.phase == "output_applied").unwrap();
    assert_eq!(witness.result.as_ref(), Some(&json!({"value":42})));
    assert_eq!(witness.resource_id.as_deref(), Some(subscription.subscription_id.as_str()));
    assert!(repository::deliver_message(&reopened, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().is_none());
}

#[test]
fn receive_mapping_failure_retains_one_delivered_receipt_without_body_completion() {
    use super::messages::{test_support::{catch_target, envelope, send, start_version}};
    use super::repository::{MessageSelection, ProcessPlanInput};
    use tentaflow_protocol::processes::{ProcessMessageStatus, ProcessSubscriptionStatus};
    let fixture = Fixture::new();
    let version = runtime::test_support::publish_model(&fixture,
        &configured_receive("1 / 0"));
    let receiver = start_version(&fixture, &version);
    let subscription = receiver.subscriptions.iter().find(|sub|
        sub.node_id == "Catch_1").unwrap();
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&subscription.subscription_id)), json!({"value":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("configured Receive lost its exact message") };
    repository::deliver_message(&fixture.db, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().unwrap();
    let retained = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &receiver.instance_id).unwrap();
    assert!(retained.subscriptions.iter().any(|sub|
        sub.subscription_id == subscription.subscription_id
            && sub.status == ProcessSubscriptionStatus::Consumed));
    assert!(retained.tokens.iter().any(|token|
        token.token_id == subscription.token_id && token.status == "waiting"));
    let witness = retained.activity_io_witnesses.iter().find(|row|
        row.activity_kind == "receive" && row.phase == "output_blocked").unwrap();
    assert_eq!(witness.result.as_ref(), Some(&json!({"value":42})));
    assert_eq!(witness.result_presence.as_deref(), Some("present"));
    let expected_hash = repository::activity_io_result_sha256(
        &repository::IoObservedValue::Present { value: json!({"value":42}) }).unwrap();
    assert_eq!(witness.result_sha256.as_deref(), Some(expected_hash.as_str()));
    assert_eq!(witness.resource_id.as_deref(), Some(subscription.subscription_id.as_str()));
    let events = repository::list_events(&fixture.db, &fixture.owner,
        &receiver.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "message_delivered").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 0);
    assert_eq!(events.iter().filter(|event| event.kind == "incident"
        && event.data["code"] == "ACTIVITY_IO_OUTPUT_FAILED").count(), 1);
    assert_eq!(repository::get_message(&fixture.db, &fixture.owner,
        &fixture.owner.user_id, &message.message_id).unwrap().message.status,
        ProcessMessageStatus::Delivered);
    let rows = super::call_tests::transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert!(repository::deliver_message(&reopened, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().is_none());
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
}

#[test]
fn blocked_receive_wins_event_race_and_closes_its_timer_sibling_once() {
    use super::messages::test_support::{catch_target, envelope, send, start_version};
    use super::repository::{MessageSelection, ProcessPlanInput};
    use tentaflow_protocol::processes::{
        ProcessEventRaceStatus, ProcessSubscriptionStatus, ProcessTimerStatus,
    };
    let fixture = Fixture::new();
    let configured = configured_receive("1 / 0");
    let mut model = super::send_receive_tests::receive_race_model();
    model.variables.insert("mapped".into(), Value::Null);
    model.modeling = configured.modeling;
    model.nodes.iter_mut().find(|node| node.id == "Catch_1").unwrap().activity_io =
        configured.nodes.iter().find(|node| node.id == "Catch_1").unwrap()
            .activity_io.clone();
    let version = runtime::test_support::publish_model(&fixture, &model);
    let receiver = start_version(&fixture, &version);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &receiver.instance_id).unwrap();
    let subscription = before.subscriptions.iter().find(|sub|
        sub.node_id == "Catch_1").unwrap();
    let timer = before.timers.iter().find(|timer|
        timer.node_id == "Timer_1").unwrap();
    let race_id = subscription.race_id.as_ref().unwrap();
    assert_eq!(timer.race_id.as_ref(), Some(race_id));
    let message = envelope(catch_target(&version, Some(&receiver.instance_id),
        Some(&subscription.subscription_id)), json!({"value":42}));
    send(&fixture, &message);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32).unwrap()
        .into_iter().find(|row| row.key.message_id == message.message_id).unwrap();
    let MessageSelection::Ready(prepared) = repository::message_snapshot(&fixture.db,
        &candidate).unwrap() else { panic!("event race lost its exact Receive arm") };
    repository::deliver_message(&fixture.db, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let after = repository::runtime_snapshot(&reopened, &fixture.owner,
        &receiver.instance_id).unwrap();
    assert_eq!(after.event_races.iter().find(|row| row.race_id == *race_id)
        .unwrap().status, ProcessEventRaceStatus::Won);
    assert_eq!(after.subscriptions.iter().find(|row|
        row.subscription_id == subscription.subscription_id).unwrap().status,
        ProcessSubscriptionStatus::Consumed);
    assert_eq!(after.timers.iter().find(|row| row.timer_id == timer.timer_id)
        .unwrap().status, ProcessTimerStatus::Cancelled);
    assert_eq!(after.tokens.iter().filter(|token| token.status == "waiting").count(), 1);
    assert!(after.tokens.iter().any(|token|
        token.token_id == subscription.token_id && token.status == "waiting"));
    assert!(after.activity_io_witnesses.iter().any(|row|
        row.activity_kind == "receive" && row.phase == "output_blocked"
            && row.token_id == subscription.token_id));
    let events = repository::list_events(&reopened, &fixture.owner,
        &receiver.instance_id, 0, 100).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "event_race_won").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "receive_task_completed").count(), 0);
    let late = super::timers::drain_due(&reopened, timer.due_at_ms.unwrap() + 1);
    late.completion.unwrap();
    assert_eq!(late.fired, 0);
    assert!(repository::deliver_message(&reopened, &prepared,
        ProcessPlanInput::Canonical, at_ms).unwrap().is_none());
}
