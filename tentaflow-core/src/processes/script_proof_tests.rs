// ============ File: script_proof_tests.rs — Script action provenance and rollback controls ============

use super::call_tests::transition_rows;
use super::repository::{self, VariableEffect};
use super::runtime::{self, test_support::*};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{ProcessModel, ProcessNode, ProcessNodeKind};
use uuid::Uuid;

fn model() -> ProcessModel {
    let mut model = super::model::starter_model();
    model.variables.insert("seed".into(), json!(2));
    model.variables.insert("answer".into(), Value::Null);
    model.nodes.insert(1, ProcessNode {
        id: "Compute".into(), name: "Compute".into(),
        kind: ProcessNodeKind::ScriptTask {
            script: "{\"value\":vars.seed + 3}".into(),
            output_mapping: BTreeMap::from([("answer".into(), "outputs.value".into())]),
        },
        repeat: None,
    });
    model.sequence_flows = vec![edge("ToCompute", "Start_1", "Compute"),
        edge("FromCompute", "Compute", "End_1")];
    model
}

#[test]
fn forged_script_result_source_and_extra_history_roll_back_all_sixteen_tables() {
    let fixture = Fixture::new();
    let model = model();
    let version = publish_model(&fixture, &model);
    let variables = serde_json::to_value(&model.variables).unwrap();
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("script-proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let script_index = plan.events.iter().position(|event| event.kind == "script_completed").unwrap();
    let source_id = plan.event_sources.get(&script_index).unwrap().clone();
    let other_id = plan.create_tokens.iter().find(|token|
        token.node_id == "Start_1").unwrap().token_id.clone();
    for variant in 0..6 {
        let mut forged = plan.clone();
        match variant {
            0 => {
                forged.events[script_index].data = json!({"outputs":{"value":6}});
                for effect in &mut forged.variable_effects {
                    if let VariableEffect::Mapped { node_id, outputs, result, .. } = effect {
                        if node_id == "Compute" {
                            *outputs = json!({"value":6});
                            result["answer"] = json!(6);
                        }
                    }
                }
                forged.variables["answer"] = json!(6);
            }
            1 => { forged.event_sources.insert(script_index, other_id.clone()); }
            2 => {
                forged.events.push(forged.events[script_index].clone());
                forged.event_sources.insert(forged.events.len()-1, source_id.clone());
            }
            3 => {
                forged.create_tokens.retain(|token|
                    !(token.node_id == "End_1" && token.status == "ready"));
            }
            4 | 5 => {
                forged.events.remove(script_index);
                forged.event_ids = forged.event_ids.into_iter().filter_map(|(index, id)|
                    (index != script_index).then_some((index - usize::from(index > script_index), id)))
                    .collect();
                forged.event_sources = forged.event_sources.into_iter().filter_map(|(index, id)|
                    (index != script_index).then_some((index - usize::from(index > script_index), id)))
                    .collect();
                forged.variable_effects.retain(|effect| !matches!(effect,
                    VariableEffect::Mapped { node_id, source_token_id, .. }
                        if node_id == "Compute" && source_token_id == &source_id));
                assert!(forged.create_tokens.iter().any(|token|
                    token.node_id == "End_1" && token.status == "ready"));
                if variant == 5 {
                    forged.variables = variables.clone();
                } else {
                    assert_eq!(forged.variables["answer"], 5);
                }
                assert!(!forged.events.iter().any(|event| event.kind == "script_completed"));
            }
            _ => unreachable!(),
        }
        let before = transition_rows(&fixture);
        let error = repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&forged),
            at_ms).unwrap_err();
        if variant == 5 {
            assert!(format!("{error:#}").contains("Script consumed or continued"),
                "{error:#}");
        }
        assert_eq!(transition_rows(&fixture), before, "forgery {variant} changed durable rows");
    }
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.variables["answer"], 5);
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.variables, committed.variables);
    let before_replay = transition_rows(&fixture);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(transition_rows(&fixture), before_replay);
}

#[test]
fn forged_script_failure_code_and_parked_wait_roll_back_before_real_incident() {
    let fixture = Fixture::new();
    let mut model = model();
    let script = model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap();
    let ProcessNodeKind::ScriptTask { script, .. } = &mut script.kind else { unreachable!() };
    *script = "1 / 0".into();
    let version = publish_model(&fixture, &model);
    let variables = serde_json::to_value(&model.variables).unwrap();
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("script-failure-proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.add_incidents[0].code, "SCRIPT_EVALUATION_FAILED");
    for variant in 0..2 {
        let mut forged = plan.clone();
        if variant == 0 {
            forged.add_incidents[0].code = "SCRIPT_MAPPING_FAILED".into();
            let event = forged.events.iter_mut().find(|event|
                event.kind == "incident" && event.node_id.as_deref() == Some("Compute")).unwrap();
            event.data["code"] = json!("SCRIPT_MAPPING_FAILED");
        } else {
            forged.create_tokens.retain(|token|
                !(token.node_id == "Compute" && token.status == "waiting"));
        }
        let before = transition_rows(&fixture);
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&forged),
            at_ms).is_err());
        assert_eq!(transition_rows(&fixture), before);
    }
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.incidents.len(), 1);
    assert_eq!(committed.incidents[0].code, "SCRIPT_EVALUATION_FAILED");
    let reopened = repository::get_instance(&fixture.db, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(reopened.incidents[0].code, committed.incidents[0].code);
}

#[test]
fn unmapped_script_output_is_replayed_from_the_pinned_body() {
    let fixture = Fixture::new();
    let mut model = model();
    let node = model.nodes.iter_mut().find(|node| node.id == "Compute").unwrap();
    node.kind = ProcessNodeKind::ScriptTask {
        script: "[1, 2, 3]".into(), output_mapping: BTreeMap::new(),
    };
    let version = publish_model(&fixture, &model);
    let variables = serde_json::to_value(&model.variables).unwrap();
    let instance_id = Uuid::new_v4().to_string();
    let command = stamp("script-unmapped-proof");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    let mut forged = plan.clone();
    let fact = forged.events.iter_mut().find(|event| event.kind == "script_completed").unwrap();
    fact.data = json!({"outputs":[1,2,4]});
    let before = transition_rows(&fixture);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&forged),
        at_ms).is_err());
    assert_eq!(transition_rows(&fixture), before);
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let history = repository::list_events(&fixture.db, &fixture.owner,
        &instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().find(|event| event.kind == "script_completed").unwrap().data,
        json!({"outputs":[1,2,3]}));
}
