// ============ File: simulation_tests.rs — deterministic simulation store regressions ============

use super::model::starter_model;
use super::repository::{IoObservedValue, ProcessActor};
use super::runtime::RuntimeIdSource;
use super::simulation::{
    simulation_meta_status, AuthenticatedSimulationAction, AuthenticatedSimulationSource,
    set_simulation_transition_preflight, simulation_scenario_sha256, SimulationClock,
    SimulationDatabase, SimulationIdSource, SimulationRegistry, SimulationSourceInput,
    SimulationStore,
};
use super::simulation_schema;
use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessActivityIo, ProcessBodyModeling, ProcessDataObject,
    ProcessDataObjectReference, ProcessInputAssociation, ProcessIoDataInput, ProcessIoDataOutput,
    ProcessNode, ProcessNodeKind, ProcessOutputAssociation, ProcessSequenceFlow,
    ProcessTimerSpec, ProcessTimerStatus, ProcessUserTaskStatus,
};

fn linear_activity_model(kind: ProcessNodeKind) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Script_1".into(),
            name: "Compute answer".into(),
            kind,
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            id: "Flow_ToScript".into(),
            source_id: "Start_1".into(),
            target_id: "Script_1".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_ToEnd".into(),
            source_id: "Script_1".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        },
    ];
    model
}

fn script_model() -> tentaflow_protocol::processes::ProcessModel {
    linear_activity_model(ProcessNodeKind::ScriptTask {
        script: "vars.value + 1".into(),
        output_mapping: BTreeMap::from([("answer".into(), "outputs".into())]),
    })
}

fn configured_user_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = linear_activity_model(ProcessNodeKind::UserTask {
        assignee_user_id: Some("user-simulation".into()),
        output_mapping: BTreeMap::new(),
    });
    model.variables.insert("missing_value".into(), json!(null));
    model.variables.insert("null_value".into(), json!(null));
    model.modeling = Some(ProcessBodyModeling {
        data_objects: vec![
            ProcessDataObject {
                id: "Object_Missing".into(),
                name: None,
            },
            ProcessDataObject {
                id: "Object_Null".into(),
                name: None,
            },
        ],
        data_object_references: vec![
            ProcessDataObjectReference {
                id: "Ref_Missing".into(),
                name: None,
                data_object_ref: "Object_Missing".into(),
                variable_binding_key: Some("missing_value".into()),
            },
            ProcessDataObjectReference {
                id: "Ref_Null".into(),
                name: None,
                data_object_ref: "Object_Null".into(),
                variable_binding_key: Some("null_value".into()),
            },
        ],
        ..ProcessBodyModeling::default()
    });
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: vec![
            ProcessIoDataInput {
                id: "Input_Missing".into(),
                name: Some("Missing input".into()),
            },
            ProcessIoDataInput {
                id: "Input_Null".into(),
                name: Some("Null input".into()),
            },
        ],
        data_outputs: Vec::new(),
        input_set_id: "InputSet_User".into(),
        input_set: vec!["Input_Missing".into(), "Input_Null".into()],
        output_set_id: "OutputSet_User".into(),
        output_set: Vec::new(),
        input_associations: vec![
            ProcessInputAssociation::DirectRef {
                id: "Association_Missing".into(),
                source_object_ref_id: "Ref_Missing".into(),
                target_input_id: "Input_Missing".into(),
            },
            ProcessInputAssociation::DirectRef {
                id: "Association_Null".into(),
                source_object_ref_id: "Ref_Null".into(),
                target_input_id: "Input_Null".into(),
            },
        ],
        output_associations: Vec::new(),
        coordinator_output: None,
    });
    model
}

fn manual_model() -> tentaflow_protocol::processes::ProcessModel {
    linear_activity_model(ProcessNodeKind::ManualTask {
        assignee_user_id: Some("user-simulation".into()),
        instructions: "Approve the simulation".into(),
    })
}

fn service_model() -> tentaflow_protocol::processes::ProcessModel {
    linear_activity_model(ProcessNodeKind::ServiceTask {
        flow_id: "flow-simulation-service".into(),
        input_mapping: BTreeMap::new(),
        output_mapping: BTreeMap::new(),
        verification: ActivityVerification::Human,
        timeout_seconds: 30,
        result_expression: None,
    })
}

fn exclusive_gateway_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Gateway_1".into(),
            name: "Choose path".into(),
            kind: ProcessNodeKind::ExclusiveGateway {
                default_flow_id: Some("Flow_ToEnd".into()),
            },
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            id: "Flow_ToGateway".into(),
            source_id: "Start_1".into(),
            target_id: "Gateway_1".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_ToEnd".into(),
            source_id: "Gateway_1".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        },
    ];
    model
}

fn timer_catch_model(timer: ProcessTimerSpec) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.timer_timezone = Some("UTC".into());
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Wait".into(),
            name: "Wait for the timer".into(),
            kind: ProcessNodeKind::TimerCatch { timer },
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            id: "Flow_ToWait".into(),
            source_id: "Start_1".into(),
            target_id: "Wait".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_ToEnd".into(),
            source_id: "Wait".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        },
    ];
    model
}

fn parallel_timer_model(duration_seconds: u32) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = timer_catch_model(ProcessTimerSpec::Duration {
        seconds: duration_seconds,
    });
    model.nodes.extend([
        ProcessNode {
            id: "Split".into(),
            name: "Split normal work and timer".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "Work".into(),
            name: "Review while timer waits".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            },
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "Join".into(),
            name: "Join normal work and timer".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
            activity_io: None,
        },
    ]);
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            id: "Flow_ToSplit".into(),
            source_id: "Start_1".into(),
            target_id: "Split".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_TimerBranch".into(),
            source_id: "Split".into(),
            target_id: "Wait".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_WorkBranch".into(),
            source_id: "Split".into(),
            target_id: "Work".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_TimerJoin".into(),
            source_id: "Wait".into(),
            target_id: "Join".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_WorkJoin".into(),
            source_id: "Work".into(),
            target_id: "Join".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_JoinEnd".into(),
            source_id: "Join".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        },
    ];
    model
}

fn parallel_two_timer_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = parallel_timer_model(1);
    model.nodes.push(ProcessNode {
        id: "WaitLater".into(),
        name: "Wait for the later timer".into(),
        kind: ProcessNodeKind::TimerCatch {
            timer: ProcessTimerSpec::Duration { seconds: 2 },
        },
        repeat: None,
        activity_io: None,
    });
    model.sequence_flows.extend([
        ProcessSequenceFlow {
            id: "Flow_LaterTimerBranch".into(),
            source_id: "Split".into(),
            target_id: "WaitLater".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_LaterTimerJoin".into(),
            source_id: "WaitLater".into(),
            target_id: "Join".into(),
            condition: None,
            call_start_node_id: None,
        },
    ]);
    model
}

fn structured_gateway_model(inclusive: bool) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    let gateway_kind = |default_flow_id: Option<&str>| {
        if inclusive {
            ProcessNodeKind::InclusiveGateway {
                default_flow_id: default_flow_id.map(str::to_owned),
            }
        } else {
            ProcessNodeKind::ParallelGateway
        }
    };
    model.nodes = vec![
        ProcessNode {
            id: "Start_1".into(),
            name: "Start".into(),
            kind: ProcessNodeKind::Start,
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "Split_1".into(),
            name: "Split".into(),
            kind: gateway_kind(None),
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "Join_1".into(),
            name: "Join".into(),
            kind: gateway_kind(None),
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "End_1".into(),
            name: "End".into(),
            kind: ProcessNodeKind::End,
            repeat: None,
            activity_io: None,
        },
    ];
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            id: "Flow_ToSplit".into(),
            source_id: "Start_1".into(),
            target_id: "Split_1".into(),
            condition: None,
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_BranchA".into(),
            source_id: "Split_1".into(),
            target_id: "Join_1".into(),
            condition: inclusive.then(|| "true".into()),
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_BranchB".into(),
            source_id: "Split_1".into(),
            target_id: "Join_1".into(),
            condition: inclusive.then(|| "false".into()),
            call_start_node_id: None,
        },
        ProcessSequenceFlow {
            id: "Flow_ToEnd".into(),
            source_id: "Join_1".into(),
            target_id: "End_1".into(),
            condition: None,
            call_start_node_id: None,
        },
    ];
    model
}

fn configured_script_input_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = script_model();
    model.variables.insert("seed".into(), json!(4));
    model.variables.insert("answer".into(), json!(null));
    model.variables.insert("mapped".into(), json!(null));
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
    let script = model.nodes.get_mut(1).expect("configured Script node");
    script.kind = ProcessNodeKind::ScriptTask {
        script: "inputs.Input_Script".into(),
        output_mapping: BTreeMap::from([(String::from("answer"), String::from("outputs"))]),
    };
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
    model
}

fn source_input_for(model: tentaflow_protocol::processes::ProcessModel) -> SimulationSourceInput {
    let model_json = serde_json::to_string(&model).expect("simulation model serializes");
    let model_sha256 = hex::encode(Sha256::digest(model_json.as_bytes()));
    SimulationSourceInput {
        org_id: "org-simulation".into(),
        owner_user_id: "user-simulation".into(),
        actor_user_id: "user-simulation".into(),
        definition_id: "definition-simulation".into(),
        version: 1,
        model_json,
        model_sha256,
        selected_process_id: "Process_1".into(),
        start_node_id: "Start_1".into(),
        acl_snapshot_json: json!({"permitted_user_ids":["user-simulation"]}).to_string(),
        scenario_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        start_ms: 1_000,
        horizon_ms: 10_000,
        tick_duration_ms: 100,
    }
}

fn source_input() -> SimulationSourceInput {
    source_input_for(script_model())
}

fn create_store(input: SimulationSourceInput) -> anyhow::Result<SimulationStore> {
    SimulationStore::create(AuthenticatedSimulationSource::from_test(input))
}

fn test_authorization(store: &SimulationStore) -> AuthenticatedSimulationAction {
    let source = store.source();
    AuthenticatedSimulationAction::for_test(
        source,
        &ProcessActor {
            org_id: source.org_id.clone(),
            user_id: source.actor_user_id.clone(),
        },
    )
}

fn reopen_store(
    database: SimulationDatabase,
    simulation_id: &str,
    actor: ProcessActor,
) -> anyhow::Result<SimulationStore> {
    let source = SimulationStore::source_pin(&database, simulation_id)?;
    let authorization = AuthenticatedSimulationAction::for_test(&source, &actor);
    SimulationStore::open(database, simulation_id, actor, authorization)
        .map_err(|error| error.error)
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SimulationSqliteFootprint {
    page_count: i64,
    page_size: i64,
    freelist_count: i64,
    trace_bytes: i64,
    resident_bytes: u64,
}

fn measure_simulation_database(
    database: SimulationDatabase,
) -> anyhow::Result<(SimulationDatabase, SimulationSqliteFootprint)> {
    let connection = database.into_connection_for_test();
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let freelist_count: i64 =
        connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let trace_bytes: i64 = connection.query_row(
        "SELECT COALESCE(SUM(length(CAST(data_json AS BLOB))), 0) FROM simulation_trace_steps",
        [],
        |row| row.get(0),
    )?;
    let resident_bytes = u64::try_from(page_count)?
        .checked_mul(u64::try_from(page_size)?)
        .expect("simulation resident bytes do not overflow");
    Ok((
        SimulationDatabase::from_connection_for_test(connection),
        SimulationSqliteFootprint {
            page_count,
            page_size,
            freelist_count,
            trace_bytes,
            resident_bytes,
        },
    ))
}

fn footprint_source_case(
    index: usize,
    owner_count: usize,
) -> (SimulationSourceInput, serde_json::Value) {
    assert!(owner_count > 0 && owner_count <= 4);
    let model = match index % 4 {
        0 => script_model(),
        1 => configured_user_model(),
        2 => manual_model(),
        _ => timer_catch_model(ProcessTimerSpec::Duration { seconds: 2 }),
    };
    let payload_unit = "simulation-footprint-payload";
    let payload = payload_unit.repeat(192 * 1024 / payload_unit.len());
    let variables = json!({
        "value": 1,
        "seed": 4,
        "payload": payload,
    });
    let mut input = source_input_for(model);
    let owner_user_id = format!(
        "simulation-footprint-owner-{}",
        index % owner_count
    );
    input.owner_user_id = owner_user_id.clone();
    let mut permitted_user_ids = vec![input.actor_user_id.clone()];
    if owner_user_id != input.actor_user_id {
        permitted_user_ids.push(owner_user_id);
    }
    input.acl_snapshot_json = json!({
        "permitted_user_ids": permitted_user_ids,
    })
    .to_string();
    if index % 4 == 3 {
        input.tick_duration_ms = 1_000;
        input.horizon_ms = 5_000;
    }
    input.scenario_sha256 = simulation_scenario_sha256(
        &input.definition_id,
        input.version,
        &input.model_sha256,
        &input.selected_process_id,
        &input.start_node_id,
        &variables,
        input.start_ms,
        input.horizon_ms,
        input.tick_duration_ms,
    )
    .expect("derive canonical footprint scenario digest");
    (input, variables)
}

fn retained_registry_footprints(
    run_count: usize,
) -> anyhow::Result<Vec<SimulationSqliteFootprint>> {
    assert!(run_count > 0 && run_count <= 16);
    let owner_count = if run_count == 16 { 4 } else { 1 };
    let registry = SimulationRegistry::new();
    let mut footprints = Vec::with_capacity(run_count);
    for index in 0..run_count {
        let (input, variables) = footprint_source_case(index, owner_count);
        let mut store = create_store(input)?;
        let simulation_id = store.simulation_id().to_owned();
        let started = store.start(test_authorization(&store), variables)?;
        match index % 4 {
            0 => assert!(started.events.iter().any(|event| event.kind == "script_completed")),
            1 => {
                let task_id = started
                    .user_tasks
                    .iter()
                    .find(|task| task.status == ProcessUserTaskStatus::Open)
                    .map(|task| task.user_task_id.clone())
                    .expect("UserTask footprint run opens a real task");
                store.complete_user_task(
                    test_authorization(&store),
                    &task_id,
                    json!({"decision": "approved"}),
                )?;
            }
            2 => {
                let task_id = started
                    .user_tasks
                    .iter()
                    .find(|task| task.status == ProcessUserTaskStatus::Open)
                    .map(|task| task.user_task_id.clone())
                    .expect("ManualTask footprint run opens a real task");
                store.acknowledge_manual_task(test_authorization(&store), &task_id)?;
            }
            _ => {
                let mut advanced = started.clone();
                let mut fired = false;
                for _ in 0..8 {
                    if advanced.events.iter().any(|event| event.kind == "timer_fired") {
                        fired = true;
                        break;
                    }
                    advanced = store.advance(test_authorization(&store))?;
                }
                assert!(fired, "TimerCatch footprint run must reach a due tick");
                assert!(advanced
                    .events
                    .iter()
                    .any(|event| event.kind == "timer_fired"));
                assert!(advanced
                    .instance
                    .as_ref()
                    .is_some_and(|instance| {
                        instance.status
                            == tentaflow_protocol::processes::ProcessInstanceStatus::Completed
                    }),
                    "TimerCatch footprint run must complete after firing");
            }
        }
        let (database, footprint) = measure_simulation_database(store.into_database())?;
        registry.insert(simulation_id, database)?;
        assert!(footprint.page_count > 0);
        assert!(footprint.page_size > 0);
        assert!(footprint.trace_bytes > 0);
        assert!(footprint.resident_bytes > 0);
        footprints.push(footprint);
    }
    let total_bytes = footprints
        .iter()
        .map(|footprint| footprint.resident_bytes)
        .sum::<u64>();
    let page_size = footprints[0].page_size;
    assert!(footprints
        .iter()
        .all(|footprint| footprint.page_size == page_size));
    eprintln!(
        "simulation_registry_footprint run_count={run_count} page_size={page_size} total_resident_bytes={total_bytes} runs={footprints:?}"
    );
    Ok(footprints)
}

#[test]
fn simulation_registry_footprint_baseline() -> anyhow::Result<()> {
    let (database, footprint) = measure_simulation_database(SimulationDatabase::open()?)?;
    drop(database);
    assert!(footprint.page_count > 0);
    assert!(footprint.page_size > 0);
    assert_eq!(
        footprint.resident_bytes,
        u64::try_from(footprint.page_count)?
            .checked_mul(u64::try_from(footprint.page_size)?)
            .expect("baseline resident bytes do not overflow")
    );
    eprintln!("simulation_registry_footprint baseline={footprint:?}");
    Ok(())
}

#[test]
fn simulation_registry_footprint_one_retained_run() -> anyhow::Result<()> {
    let footprints = retained_registry_footprints(1)?;
    assert_eq!(footprints.len(), 1);
    Ok(())
}

#[test]
fn simulation_registry_footprint_four_retained_runs() -> anyhow::Result<()> {
    let footprints = retained_registry_footprints(4)?;
    assert_eq!(footprints.len(), 4);
    Ok(())
}

#[test]
fn simulation_registry_footprint_sixteen_retained_runs() -> anyhow::Result<()> {
    let footprints = retained_registry_footprints(16)?;
    assert_eq!(footprints.len(), 16);
    Ok(())
}

#[test]
fn simulation_private_schema_initializes_without_global_pool_or_scheduler() {
    let connection = Connection::open_in_memory().expect("open private connection");
    crate::db::migrations::run(&connection).expect("initialize production schema");
    simulation_schema::initialize(&connection).expect("initialize private schema");
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'simulation_%'",
            [],
            |row| row.get(0),
        )
        .expect("read private schema table count");
    assert_eq!(table_count, 6);
    simulation_schema::initialize(&connection).expect("reopen private schema");
}

#[test]
fn simulation_private_schema_rejects_orphan_rows_and_rolls_back_full_transaction() {
    let mut connection = Connection::open_in_memory().expect("open private connection");
    crate::db::migrations::run(&connection).expect("initialize production schema");
    simulation_schema::initialize(&connection).expect("initialize private schema");
    let transaction = connection.transaction().expect("begin private transaction");
    transaction
        .execute(
            "INSERT INTO simulation_meta(simulation_id,scenario_sha256,profile,org_id,actor_user_id,definition_id,version,model_sha256,status,revision,instance_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'running',1,NULL,0,0)",
            params![
                "00000000-0000-5000-8000-000000000001",
                "a".repeat(64),
                "script_user_manual",
                "org-simulation",
                "user-simulation",
                "definition-simulation",
                1,
                "b".repeat(64),
            ],
        )
        .expect("insert temporary simulation identity");
    assert!(transaction
        .execute(
            "INSERT INTO simulation_trace_steps(trace_step_id,simulation_id,ordinal,action,at_ms,request_sha256,result_sha256,data_json) VALUES(?1,?2,0,'test',0,?3,?4,'{}')",
            params![
                "00000000-0000-5000-8000-000000000002",
                "00000000-0000-5000-8000-000000000001",
                "b".repeat(64),
                "c".repeat(64),
            ],
        )
        .is_err());
    transaction.rollback().expect("rollback orphan transaction");
    let identities: i64 = connection
        .query_row("SELECT COUNT(*) FROM simulation_meta", [], |row| row.get(0))
        .expect("count rolled back identities");
    assert_eq!(identities, 0);
}

#[test]
fn simulation_ids_are_uuid_v5_and_stable_for_same_scenario() {
    let mut first = SimulationIdSource::new(&"b".repeat(64)).expect("create first allocator");
    let mut second = SimulationIdSource::new(&"b".repeat(64)).expect("create second allocator");
    assert_eq!(first.next_id("event"), second.next_id("event"));
    first.set_step_index(7);
    second.set_step_index(7);
    let id = first.next_id("token");
    assert_eq!(id, second.next_id("token"));
    let uuid = uuid::Uuid::parse_str(&id).expect("allocator emits UUID");
    assert_eq!(uuid.get_version_num(), 5);
    assert!(first.issued().contains(&id));
    assert!(!first.accepts("00000000-0000-4000-8000-000000000001"));
}

#[test]
fn simulation_scenario_digest_is_canonical_and_source_bound() {
    let model = script_model();
    let model_json = serde_json::to_string(&model).expect("encode scenario model");
    let model_sha256 = hex::encode(Sha256::digest(model_json.as_bytes()));
    let first_variables = json!({"nested": {"right": 2, "left": 1}, "seed": 4});
    let reordered_variables = json!({"seed": 4, "nested": {"left": 1, "right": 2}});
    let first = simulation_scenario_sha256(
        "definition-simulation",
        1,
        &model_sha256,
        "Process_1",
        "Start_1",
        &first_variables,
        1_000,
        10_000,
        100,
    )
    .expect("compute canonical scenario digest");
    assert_eq!(
        first,
        simulation_scenario_sha256(
            "definition-simulation",
            1,
            &model_sha256,
            "Process_1",
            "Start_1",
            &reordered_variables,
            1_000,
            10_000,
            100,
        )
        .expect("compute reordered scenario digest")
    );
    assert_ne!(
        first,
        simulation_scenario_sha256(
            "definition-simulation",
            1,
            &model_sha256,
            "Process_1",
            "Start_1",
            &json!({"seed": 5}),
            1_000,
            10_000,
            100,
        )
        .expect("compute changed scenario digest")
    );
    assert_ne!(
        first,
        simulation_scenario_sha256(
            "definition-simulation",
            1,
            &"c".repeat(64),
            "Process_1",
            "Start_1",
            &first_variables,
            1_000,
            10_000,
            100,
        )
        .expect("compute changed source digest")
    );
}

#[test]
fn simulation_registry_keeps_identical_scenario_runs_distinct_and_reopenable() {
    let mut first = create_store(source_input()).expect("capture first source pin");
    let first_view = first
        .start(test_authorization(&first), json!({"value": 1}))
        .expect("start first sandbox");
    let first_id = first_view.simulation_id.clone();
    let first_database = first.into_database();

    let mut second = create_store(source_input()).expect("capture second source pin");
    let second_view = second
        .start(test_authorization(&second), json!({"value": 2}))
        .expect("start second sandbox");
    let second_id = second_view.simulation_id.clone();
    let second_database = second.into_database();
    assert_ne!(first_id, second_id);

    let registry = SimulationRegistry::new();
    registry
        .insert(first_id.clone(), first_database)
        .expect("register first sandbox");
    registry
        .insert(second_id.clone(), second_database)
        .expect("register second sandbox");

    let first_database = registry.take(&first_id).expect("take first sandbox");
    let first_reopened = reopen_store(
        first_database,
        &first_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen first sandbox after second start");
    let reopened = first_reopened
        .view(test_authorization(&first_reopened))
        .expect("read first sandbox after second start");
    assert_eq!(
        reopened.instance.expect("first sandbox instance").variables["answer"],
        2
    );

    let second_database = registry.take(&second_id).expect("take second sandbox");
    let second_reopened = reopen_store(
        second_database,
        &second_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen second sandbox");
    assert_eq!(
        second_reopened
            .view(test_authorization(&second_reopened))
            .expect("read second sandbox")
            .instance
            .expect("second sandbox instance")
            .variables["answer"],
        2
    );
}

#[test]
fn simulation_registry_rejects_a_fifth_run_without_evicting_the_existing_four() {
    let registry = SimulationRegistry::new();
    let mut first_id = None;
    for index in 0..4 {
        let store = create_store(source_input()).expect("capture bounded source pin");
        let simulation_id = store.simulation_id().to_owned();
        if index == 0 {
            first_id = Some(simulation_id.clone());
        }
        registry
            .insert(simulation_id, store.into_database())
            .expect("register run within owner limit");
    }

    let fifth = create_store(source_input()).expect("capture fifth source pin");
    let fifth_id = fifth.simulation_id().to_owned();
    let error = registry
        .insert(fifth_id, fifth.into_database())
        .expect_err("owner limit must reject the fifth retained run");
    assert!(format!("{error:#}").contains("owner has reached"));

    let first_id = first_id.expect("first run identity");
    let first_database = registry.take(&first_id).expect("first run remains retained");
    let first_reopened = reopen_store(
        first_database,
        &first_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen retained first run");
    assert!(
        first_reopened
            .view(test_authorization(&first_reopened))
            .expect("view retained first run")
            .instance
            .is_none()
    );
}

#[test]
fn simulation_registry_reserves_start_capacity_before_database_construction() {
    let registry = SimulationRegistry::new();
    let first = registry
        .reserve_start("owner-one")
        .expect("reserve first pending start");
    let second = registry
        .reserve_start("owner-one")
        .expect("reserve second pending start");
    let third = registry
        .reserve_start("owner-one")
        .expect("reserve third pending start");
    let fourth = registry
        .reserve_start("owner-one")
        .expect("reserve fourth pending start");
    let error = registry
        .reserve_start("owner-one")
        .expect_err("per-owner pending start limit must reject the fifth reservation");
    assert!(format!("{error:#}").contains("owner has reached"));
    drop(first);
    let replacement = registry
        .reserve_start("owner-one")
        .expect("dropping a failed start must release its owner slot");
    drop((second, third, fourth, replacement));

    let mut global_reservations = Vec::new();
    for index in 0..16 {
        global_reservations.push(
            registry
                .reserve_start(&format!("owner-{index}"))
                .expect("reserve start within global limit"),
        );
    }
    let error = registry
        .reserve_start("owner-overflow")
        .expect_err("global pending start limit must reject the seventeenth reservation");
    assert!(format!("{error:#}").contains("active run limit"));
    drop(global_reservations.pop());
    let replacement = registry
        .reserve_start("owner-overflow")
        .expect("dropping a failed global start must release its slot");
    global_reservations.push(replacement);
    drop(global_reservations);
}

#[test]
fn simulation_registry_pending_release_drops_database_after_in_flight_step() {
    let registry = SimulationRegistry::new();
    let store = create_store(source_input()).expect("capture source for in-flight step");
    let simulation_id = store.simulation_id().to_owned();
    registry
        .insert(simulation_id.clone(), store.into_database())
        .expect("retain simulation before step");
    let database = registry
        .take(&simulation_id)
        .expect("step owns the private database");
    registry
        .mark_release_requested(&simulation_id)
        .expect("close records a durable pending release");
    registry
        .put(simulation_id.clone(), database)
        .expect("step completion finalizes the pending release");
    assert!(registry.take(&simulation_id).is_err());
}

#[test]
fn simulation_clock_is_monotonic_and_horizon_bounded() {
    let mut clock = SimulationClock::new(10, 30, 10).expect("create virtual clock");
    assert_eq!(clock.next_time().expect("first tick"), 20);
    clock
        .accept_transition(10)
        .expect("accept initial transition");
    assert_eq!(clock.step_index, 1);
    assert!(clock.accept_transition(19).is_err());
    assert!(clock.accept_transition(25).is_err());
    clock
        .accept_transition(20)
        .expect("accept configured virtual tick");
    assert!(clock.accept_transition(31).is_err());
    clock
        .accept_transition(30)
        .expect("accept final virtual tick");
    assert!(clock.next_time().is_err());
}

#[test]
fn simulation_timer_catch_waits_for_due_tick_and_reopens_with_injected_event_ids() {
    let mut input = source_input_for(timer_catch_model(ProcessTimerSpec::Duration { seconds: 2 }));
    input.tick_duration_ms = 1_000;
    input.horizon_ms = 5_000;
    let mut store = create_store(input).expect("capture TimerCatch source pin");
    let started = store
        .start(test_authorization(&store), json!({}))
        .expect("start TimerCatch simulation");
    let timer = started.timers.first().expect("one private Catch timer");
    assert_eq!(timer.status, ProcessTimerStatus::Pending);
    assert_eq!(timer.due_at_ms, Some(3_000));
    assert!(started
        .events
        .iter()
        .all(|event| uuid::Uuid::parse_str(&event.event_id).is_ok()));

    let before_due = store
        .advance(test_authorization(&store))
        .expect("advance before the timer due tick");
    assert_eq!(before_due.clock.now_ms, 2_000);
    assert_eq!(before_due.events, started.events);
    assert_eq!(
        before_due.timers[0].status,
        ProcessTimerStatus::Pending,
        "a waiting-only tick must preserve the factual timer"
    );

    let fired = store
        .advance(test_authorization(&store))
        .expect("fire TimerCatch at its due tick");
    assert_eq!(fired.clock.now_ms, 3_000);
    assert_eq!(
        fired
            .events
            .iter()
            .filter(|event| event.kind == "timer_fired")
            .count(),
        1
    );
    assert_eq!(fired.timers[0].status, ProcessTimerStatus::Fired);
    assert!(fired
        .events
        .iter()
        .all(|event| uuid::Uuid::parse_str(&event.event_id).is_ok()));

    let simulation_id = fired.simulation_id.clone();
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen fired TimerCatch simulation");
    let reopened_view = reopened
        .view(test_authorization(&reopened))
        .expect("read reopened TimerCatch simulation");
    assert_eq!(reopened_view.clock, fired.clock);
    assert_eq!(reopened_view.events, fired.events);
    assert_eq!(reopened_view.timers, fired.timers);
}

#[test]
fn simulation_timer_catch_date_and_working_duration_fire_from_pinned_rules_and_reopen() {
    let mut date_input = source_input_for(timer_catch_model(ProcessTimerSpec::Date {
        at: "1970-01-01T00:00:03Z".into(),
    }));
    date_input.tick_duration_ms = 1_000;
    date_input.horizon_ms = 5_000;
    let mut date_store = create_store(date_input).expect("capture Date TimerCatch source pin");
    let date_started = date_store
        .start(test_authorization(&date_store), json!({}))
        .expect("start Date TimerCatch simulation");
    assert_eq!(date_started.timers.len(), 1);
    assert_eq!(date_started.timers[0].status, ProcessTimerStatus::Pending);
    assert_eq!(date_started.timers[0].due_at_ms, Some(3_000));

    let calendar = tentaflow_protocol::processes::ProcessWorkCalendar {
        name: "Simulation working hours".into(),
        weekly_windows: (1..=7)
            .map(|weekday| tentaflow_protocol::processes::WorkWindow {
                weekday,
                start_minute: 0,
                end_minute: 1_440,
            })
            .collect(),
        manual_days_off: Vec::new(),
        holiday_policy: tentaflow_protocol::processes::HolidayPolicy::None,
    };
    let mut working_model = timer_catch_model(ProcessTimerSpec::WorkingDuration { seconds: 1 });
    working_model.work_calendar = Some(calendar.clone());
    working_model.calendar_pin = Some(
        super::calendar::mint_calendar_pin(&calendar, "UTC")
            .expect("mint immutable simulation calendar pin"),
    );
    let mut working_input = source_input_for(working_model);
    working_input.tick_duration_ms = 1_000;
    working_input.horizon_ms = 5_000;
    let mut working_store =
        create_store(working_input).expect("capture WorkingDuration TimerCatch source pin");
    let working_started = working_store
        .start(test_authorization(&working_store), json!({}))
        .expect("start WorkingDuration TimerCatch simulation");
    assert_eq!(working_started.timers.len(), 1);
    assert_eq!(working_started.timers[0].status, ProcessTimerStatus::Pending);
    assert!(working_started.timers[0].due_at_ms.is_some());
    assert_eq!(
        working_started.timers[0]
            .working_time
            .as_ref()
            .expect("WorkingDuration summary")
            .calendar_name,
        "Simulation working hours"
    );

    let date_before_due = date_store
        .advance(test_authorization(&date_store))
        .expect("advance Date TimerCatch before its due tick");
    assert!(date_before_due
        .events
        .iter()
        .all(|event| event.kind != "timer_fired"));
    let date_fired = date_store
        .advance(test_authorization(&date_store))
        .expect("fire Date TimerCatch at its due tick");
    assert_eq!(date_fired.timers[0].status, ProcessTimerStatus::Fired);
    assert_eq!(
        date_fired
            .events
            .iter()
            .filter(|event| event.kind == "timer_fired")
            .count(),
        1
    );
    let date_simulation_id = date_fired.simulation_id.clone();
    let date_reopened = reopen_store(
        date_store.into_database(),
        &date_simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen fired Date TimerCatch simulation");
    let date_reopened_view = date_reopened
        .view(test_authorization(&date_reopened))
        .expect("read reopened Date TimerCatch simulation");
    assert_eq!(date_reopened_view.clock, date_fired.clock);
    assert_eq!(date_reopened_view.events, date_fired.events);
    assert_eq!(date_reopened_view.timers, date_fired.timers);

    let working_fired = working_store
        .advance(test_authorization(&working_store))
        .expect("fire WorkingDuration TimerCatch at its due tick");
    assert_eq!(working_fired.timers[0].status, ProcessTimerStatus::Fired);
    assert_eq!(
        working_fired
            .events
            .iter()
            .filter(|event| event.kind == "timer_fired")
            .count(),
        1
    );
    let working_simulation_id = working_fired.simulation_id.clone();
    let working_reopened = reopen_store(
        working_store.into_database(),
        &working_simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen fired WorkingDuration TimerCatch simulation");
    let working_reopened_view = working_reopened
        .view(test_authorization(&working_reopened))
        .expect("read reopened WorkingDuration TimerCatch simulation");
    assert_eq!(working_reopened_view.clock, working_fired.clock);
    assert_eq!(working_reopened_view.events, working_fired.events);
    assert_eq!(working_reopened_view.timers, working_fired.timers);
}

#[test]
fn simulation_timer_catch_due_order_is_deterministic_and_one_per_tick() {
    let mut input = source_input_for(parallel_two_timer_model());
    input.tick_duration_ms = 1_000;
    input.horizon_ms = 5_000;
    let mut store = create_store(input).expect("capture parallel TimerCatch source pin");
    let started = store
        .start(test_authorization(&store), json!({}))
        .expect("start parallel TimerCatch simulation");
    assert_eq!(started.timers.len(), 2);
    let mut due_times = started
        .timers
        .iter()
        .map(|timer| timer.due_at_ms)
        .collect::<Vec<_>>();
    due_times.sort_unstable();
    assert_eq!(due_times, vec![Some(2_000), Some(3_000)]);

    let first = store
        .advance(test_authorization(&store))
        .expect("fire the first due TimerCatch");
    let first_fired = first
        .events
        .iter()
        .filter(|event| event.kind == "timer_fired")
        .collect::<Vec<_>>();
    assert_eq!(first_fired.len(), 1);
    assert_eq!(first_fired[0].node_id.as_deref(), Some("Wait"));
    assert_eq!(first.clock.now_ms, 2_000);
    assert_eq!(
        first
            .timers
            .iter()
            .filter(|timer| timer.status == ProcessTimerStatus::Fired)
            .count(),
        1
    );

    let second = store
        .advance(test_authorization(&store))
        .expect("fire the second due TimerCatch on the next tick");
    let second_fired = second
        .events
        .iter()
        .filter(|event| event.kind == "timer_fired")
        .collect::<Vec<_>>();
    assert_eq!(second_fired.len(), 2);
    assert_eq!(second_fired[1].node_id.as_deref(), Some("WaitLater"));
    assert_eq!(second.clock.now_ms, 3_000);
    assert_eq!(
        second
            .timers
            .iter()
            .filter(|timer| timer.status == ProcessTimerStatus::Fired)
            .count(),
        2
    );
}

#[test]
fn simulation_timer_catch_does_not_block_parallel_work_before_its_due_time() {
    let mut input = source_input_for(parallel_timer_model(60));
    input.tick_duration_ms = 1_000;
    input.horizon_ms = 10_000;
    let mut store = create_store(input).expect("capture parallel TimerCatch source pin");
    let started = store
        .start(test_authorization(&store), json!({}))
        .expect("start parallel TimerCatch simulation");
    let task_id = started
        .user_tasks
        .iter()
        .find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("normal Work branch remains completable")
        .user_task_id
        .clone();
    assert_eq!(started.timers.len(), 1);
    assert_eq!(started.timers[0].status, ProcessTimerStatus::Pending);

    let completed = store
        .complete_user_task(
            test_authorization(&store),
            &task_id,
            json!({"answer":"approved"}),
        )
        .expect("complete normal Work branch while timer waits");
    assert_eq!(completed.timers[0].status, ProcessTimerStatus::Pending);
    assert!(completed
        .events
        .iter()
        .any(|event| event.kind == "user_task_completed"));
    assert!(completed
        .events
        .iter()
        .all(|event| event.kind != "timer_fired"));
    assert_eq!(
        completed
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .expect("completed Work task")
            .outputs,
        json!({"answer":"approved"})
    );
}

#[test]
fn simulation_timer_catch_refuses_only_the_tick_beyond_horizon() {
    let mut input = source_input_for(timer_catch_model(ProcessTimerSpec::Duration { seconds: 60 }));
    input.tick_duration_ms = 1_000;
    input.horizon_ms = 3_000;
    let mut store = create_store(input).expect("capture horizon TimerCatch source pin");
    store
        .start(test_authorization(&store), json!({}))
        .expect("start horizon TimerCatch simulation");
    store
        .advance(test_authorization(&store))
        .expect("advance to the first in-horizon tick");
    store
        .advance(test_authorization(&store))
        .expect("advance to the final in-horizon tick");
    assert_eq!(store.clock().now_ms, 3_000);
    let error = store
        .advance(test_authorization(&store))
        .expect_err("a tick beyond the configured horizon must be refused");
    assert!(format!("{error:#}").contains("horizon"));
    assert_eq!(store.clock().now_ms, 3_000);
}

#[test]
fn simulation_timer_catch_rolls_back_every_private_row_on_late_command_conflict() {
    let mut input = source_input_for(timer_catch_model(ProcessTimerSpec::Duration { seconds: 2 }));
    input.tick_duration_ms = 1_000;
    input.horizon_ms = 5_000;
    input.scenario_sha256 = simulation_scenario_sha256(
        &input.definition_id,
        input.version,
        &input.model_sha256,
        &input.selected_process_id,
        &input.start_node_id,
        &json!({}),
        input.start_ms,
        input.horizon_ms,
        input.tick_duration_ms,
    )
    .expect("derive the actual canonical TimerCatch scenario identity");
    let mut store = create_store(input).expect("capture TimerCatch source pin");
    store
        .start(test_authorization(&store), json!({}))
        .expect("start TimerCatch simulation");
    store
        .advance(test_authorization(&store))
        .expect("advance to the pre-due tick");
    let simulation_id = store.simulation_id().to_owned();
    let injected_command_id = Arc::new(Mutex::new(None));
    let injected_command_id_for_hook = Arc::clone(&injected_command_id);
    let simulation_id_for_hook = simulation_id.clone();
    set_simulation_transition_preflight(move |connection, command_id| {
        assert!(uuid::Uuid::parse_str(command_id).is_ok());
        *injected_command_id_for_hook
            .lock()
            .expect("lock injected command identity") = Some(command_id.to_owned());
        let (expected_revision, at_ms): (i64, i64) = connection.query_row(
            "SELECT revision,now_ms FROM simulation_clock WHERE simulation_id=?1",
            [&simulation_id_for_hook],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let request_hash = hex::encode(Sha256::digest(b"timer-catch-conflict"));
        connection.execute(
            "INSERT INTO simulation_commands(command_id,simulation_id,actor_user_id,request_hash,expected_revision,result_json,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                command_id,
                simulation_id_for_hook,
                "user-simulation",
                request_hash,
                expected_revision,
                json!({"injected": true}).to_string(),
                at_ms,
            ],
        )?;
        Ok(())
    })
    .expect("install the one-shot prewrite conflict at the real advance seam");
    let before = store
        .view(test_authorization(&store))
        .expect("read pre-conflict TimerCatch view");
    let before_rows = store
        .private_row_snapshot()
        .expect("snapshot every private SQL row before the conflicted fire");
    let error = store
        .advance(test_authorization(&store))
        .expect_err("late command identity conflict must reject TimerCatch fire");
    assert!(format!("{error:#}").contains("simulation_commands"));
    let after = store
        .view(test_authorization(&store))
        .expect("read rolled-back TimerCatch view");
    let after_rows = store
        .private_row_snapshot()
        .expect("snapshot every private SQL row after the rejected fire");
    for (table_name, before_table_rows) in &before_rows {
        let after_table_rows = after_rows
            .get(table_name)
            .expect("rollback snapshot retains every private table");
        if table_name == "simulation_commands" {
            let command_id = injected_command_id
                .lock()
                .expect("lock injected command identity")
                .clone()
                .expect("real advance allocated the colliding command identity");
            assert_eq!(after_table_rows.len(), before_table_rows.len() + 1);
            assert!(before_table_rows
                .iter()
                .all(|row| after_table_rows.contains(row)));
            assert_eq!(
                after_table_rows
                    .iter()
                    .filter(|row| !before_table_rows.contains(row))
                    .filter(|row| row.iter().any(|value| value.contains(&command_id)))
                    .count(),
                1
            );
        } else {
            assert_eq!(before_table_rows, after_table_rows, "table {table_name} changed");
        }
    }
    assert_eq!(before_rows.len(), after_rows.len());
    assert_eq!(after.clock, before.clock);
    assert_eq!(after.events, before.events);
    assert_eq!(after.timers, before.timers);
    assert_eq!(
        after
            .timers
            .first()
            .expect("retained TimerCatch row")
            .status,
        ProcessTimerStatus::Pending
    );
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen after the rejected TimerCatch fire");
    let reopened_view = reopened
        .view(test_authorization(&reopened))
        .expect("read reopened rolled-back TimerCatch view");
    assert_eq!(reopened_view.clock, before.clock);
    assert_eq!(reopened_view.events, before.events);
    assert_eq!(reopened_view.timers, before.timers);
}

#[test]
fn simulation_incident_keeps_the_private_run_open_for_peer_work() {
    assert_eq!(
        simulation_meta_status(&tentaflow_protocol::processes::ProcessInstanceStatus::Incident),
        "running"
    );
}

#[test]
fn simulation_script_reopens_with_same_body_result_and_trace() {
    let mut store = create_store(source_input()).expect("capture source pin");
    let first = store
        .start(test_authorization(&store), json!({"value": 1}))
        .expect("start sandbox");
    assert!(first.instance.is_some());
    assert_eq!(
        first.instance.as_ref().expect("started instance").variables["answer"],
        2
    );
    assert!(first
        .events
        .iter()
        .any(|event| event.kind == "script_completed"
            && event.node_id.as_deref() == Some("Script_1")));
    let simulation_id = first.simulation_id.clone();
    let event_bytes = serde_json::to_vec(&first.events).expect("encode first trace");
    let view_bytes = serde_json::to_vec(&first).expect("encode first view");
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen sandbox");
    let second = reopened
        .view(test_authorization(&reopened))
        .expect("read reopened sandbox");
    assert_eq!(
        event_bytes,
        serde_json::to_vec(&second.events).expect("encode reopened trace")
    );
    assert_eq!(
        view_bytes,
        serde_json::to_vec(&second).expect("encode reopened view")
    );
}

#[test]
fn simulation_script_activity_io_binds_authored_association_value_after_reopen() {
    let mut store = create_store(source_input_for(configured_script_input_model()))
        .expect("capture source pin");
    let started = store
        .start(
            test_authorization(&store),
            json!({"seed": 4, "answer": null, "mapped": null}),
        )
        .expect("start sandbox");
    assert_eq!(
        started
            .instance
            .as_ref()
            .expect("started instance")
            .variables["answer"],
        14
    );
    assert_eq!(
        started
            .instance
            .as_ref()
            .expect("started instance")
            .variables["mapped"],
        14
    );
    assert!(started.events.iter().any(|event| {
        event.kind == "script_result_evaluated" && event.data["outputs"] == json!(14)
    }));
    assert!(started.activity_io_witnesses.iter().any(|witness| {
        witness.activity_kind == "script"
            && witness.phase == "output_applied"
            && witness.result == Some(json!(14))
            && witness.input_values.as_ref().is_some_and(|inputs| {
                matches!(inputs.as_slice(), [input]
                    if matches!(&input.observed, IoObservedValue::Present { value } if value == &json!(14)))
            })
    }));
    let simulation_id = started.simulation_id.clone();
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen script IO sandbox");
    let view = reopened
        .view(test_authorization(&reopened))
        .expect("read reopened script IO sandbox");
    assert_eq!(
        view.instance.expect("reopened instance").variables["mapped"],
        14
    );
    assert!(view.activity_io_witnesses.iter().any(|witness| {
        witness.activity_kind == "script"
            && witness.phase == "output_applied"
            && witness.result == Some(json!(14))
    }));
}

#[test]
fn simulation_exclusive_gateway_reopens_with_deterministic_trace() {
    let mut store = create_store(source_input_for(exclusive_gateway_model()))
        .expect("capture gateway source pin");
    let started = store
        .start(test_authorization(&store), json!({}))
        .expect("run exclusive gateway");
    assert_eq!(
        started.instance.expect("completed instance").status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed
    );
    assert!(started.events.iter().any(|event| {
        event.kind == "exclusive_selected" && event.node_id.as_deref() == Some("Gateway_1")
    }));
    assert!(started
        .events
        .iter()
        .any(|event| event.kind == "instance_completed"));
}

#[test]
fn simulation_parallel_and_inclusive_gateways_use_private_receipts() {
    for inclusive in [false, true] {
        let mut store = create_store(source_input_for(structured_gateway_model(inclusive)))
            .expect("capture structured gateway source pin");
        let started = store
            .start(test_authorization(&store), json!({}))
            .expect("run structured gateway");
        assert_eq!(
            started.instance.expect("completed instance").status,
            tentaflow_protocol::processes::ProcessInstanceStatus::Completed
        );
        assert!(started.events.iter().any(|event| {
            event.kind
                == if inclusive {
                    "inclusive_joined"
                } else {
                    "parallel_joined"
                }
        }));
    }
}

#[test]
fn simulation_user_task_reopens_with_ordered_missing_present_null_inputs() {
    let mut store =
        create_store(source_input_for(configured_user_model())).expect("capture source pin");
    let started = store
        .start(test_authorization(&store), json!({"null_value": null}))
        .expect("start sandbox");
    let task = started
        .user_tasks
        .iter()
        .find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("user task is open");
    assert!(task.can_complete);
    assert_eq!(task.activity_inputs.len(), 2);
    assert!(matches!(
        &task.activity_inputs[0].value,
        &tentaflow_protocol::processes::ProcessUserTaskInputValue::Missing
    ));
    assert!(matches!(
        &task.activity_inputs[1].value,
        tentaflow_protocol::processes::ProcessUserTaskInputValue::Present(value)
            if value.is_null()
    ));
    let task_id = task.user_task_id.clone();
    let completed = store
        .complete_user_task(
            test_authorization(&store),
            &task_id,
            json!({"decision": "yes"}),
        )
        .expect("complete user task");
    assert!(completed
        .events
        .iter()
        .any(|event| event.kind == "user_task_completed"
            && event.data["outputs"] == json!({"decision": "yes"})));
    assert!(completed
        .user_tasks
        .iter()
        .any(|task| task.user_task_id == task_id
            && task.status == ProcessUserTaskStatus::Completed
            && !task.can_complete
            && task.outputs == json!({"decision": "yes"})));
    assert!(completed.activity_io_witnesses.iter().any(|witness| {
        witness.activity_kind == "user"
            && witness.phase == "output_applied"
            && witness
                .input_values
                .as_ref()
                .is_some_and(|inputs| inputs.len() == 2)
    }));
    assert_eq!(
        completed.instance.expect("completed instance").status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Completed
    );
    let simulation_id = completed.simulation_id.clone();
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen completed user task");
    let reopened_view = reopened
        .view(test_authorization(&reopened))
        .expect("read completed user task");
    assert_eq!(
        serde_json::to_vec(&completed.events).expect("encode completed trace"),
        serde_json::to_vec(&reopened_view.events).expect("encode reopened trace")
    );
    assert_eq!(
        reopened_view
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .expect("reopened completed task")
            .outputs,
        json!({"decision": "yes"})
    );
}

#[test]
fn simulation_writer_rolls_back_every_transition_row_on_late_command_conflict() {
    let mut store =
        create_store(source_input_for(configured_user_model())).expect("capture source pin");
    let started = store
        .start(test_authorization(&store), json!({"null_value": null}))
        .expect("start sandbox");
    let task_id = started
        .user_tasks
        .iter()
        .find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("user task is open")
        .user_task_id
        .clone();
    let simulation_id = started.simulation_id.clone();
    let database = store.into_database();
    let reopened = reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen sandbox before conflict injection");

    let mut command_ids =
        SimulationIdSource::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("create replay allocator");
    command_ids.set_step_index(reopened.clock().step_index);
    let conflicting_command_id = command_ids.next_id("command");
    let connection = reopened.into_database().into_connection_for_test();
    connection
        .execute(
            "INSERT INTO simulation_commands(command_id,simulation_id,actor_user_id,request_hash,expected_revision,result_json,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                conflicting_command_id,
                simulation_id,
                "user-simulation",
                "b".repeat(64),
                2,
                "{}",
                1_100,
            ],
        )
        .expect("insert deterministic late-conflict command");

    let mut reopened = reopen_store(
        SimulationDatabase::from_connection_for_test(connection),
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen sandbox after conflict injection");
    let before = reopened
        .view(test_authorization(&reopened))
        .expect("read pre-conflict view");
    let error = reopened
        .complete_user_task(
            test_authorization(&reopened),
            &task_id,
            json!({"decision": "yes"}),
        )
        .expect_err("duplicate command must roll back the transition");
    assert!(format!("{error:#}").contains("simulation_commands"));
    let after = reopened
        .view(test_authorization(&reopened))
        .expect("read rolled-back view");
    assert_eq!(
        serde_json::to_vec(&before).expect("encode pre-conflict view"),
        serde_json::to_vec(&after).expect("encode rolled-back view")
    );
}

#[test]
fn simulation_manual_task_reopens_with_pinned_instructions_and_acknowledgment() {
    let mut store = create_store(source_input_for(manual_model())).expect("capture source pin");
    let started = store
        .start(test_authorization(&store), json!({}))
        .expect("start sandbox");
    let task = started
        .user_tasks
        .iter()
        .find(|task| task.status == ProcessUserTaskStatus::Open)
        .expect("manual task is open");
    assert!(task.can_complete);
    assert_eq!(task.instructions.as_deref(), Some("Approve the simulation"));
    let task_id = task.user_task_id.clone();
    let acknowledged = store
        .acknowledge_manual_task(test_authorization(&store), &task_id)
        .expect("acknowledge manual task");
    assert!(acknowledged.events.iter().any(|event| {
        event.kind == "manual_task_acknowledged"
            && event.data["user_task_id"] == task_id
            && event.data["acknowledged_by_user_id"] == "user-simulation"
    }));
    assert_eq!(
        acknowledged
            .user_tasks
            .iter()
            .find(|task| task.user_task_id == task_id)
            .expect("acknowledged task")
            .status,
        ProcessUserTaskStatus::Completed
    );
}

#[test]
fn simulation_source_capture_requires_acl_and_exact_model_hash() {
    let mut input = source_input();
    input.model_sha256 = "c".repeat(64);
    assert!(create_store(input).is_err());
}

#[test]
fn simulation_source_pin_is_immutable_after_capture() {
    let store = create_store(source_input()).expect("capture source pin");
    let simulation_id = store.simulation_id().to_owned();
    let connection = store.into_database().into_connection_for_test();
    assert!(connection
        .execute(
            "UPDATE simulation_source_pins SET model_sha256=?1 WHERE simulation_id=?2",
            params!["c".repeat(64), simulation_id],
        )
        .is_err());
    assert!(connection
        .execute(
            "UPDATE simulation_meta SET org_id='other-org' WHERE simulation_id=?1",
            [&simulation_id],
        )
        .is_err());
    reopen_store(
        SimulationDatabase::from_connection_for_test(connection),
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-simulation".into(),
        },
    )
    .expect("reopen unchanged source pin");
}

#[test]
fn simulation_service_refuses_before_claim_dispatch_and_result() {
    let error = create_store(source_input_for(service_model()))
        .expect_err("ServiceTask must be rejected by the initial profile");
    let message = format!("{error:#}");
    assert!(
        message.contains("ServiceTask is unsupported in simulation"),
        "{message}"
    );
}

#[test]
fn simulation_reader_rejects_actor_outside_captured_acl() {
    let mut store = create_store(source_input()).expect("capture source pin");
    let simulation_id = store.simulation_id().to_owned();
    store
        .start(test_authorization(&store), json!({"value": 1}))
        .expect("start sandbox");
    let database = store.into_database();
    assert!(reopen_store(
        database,
        &simulation_id,
        ProcessActor {
            org_id: "org-simulation".into(),
            user_id: "user-outside-acl".into(),
        },
    )
    .is_err());
}
