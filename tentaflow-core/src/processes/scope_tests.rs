// ============ File: scope_tests.rs — Persisted embedded process scope behavior ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessDiagram, ProcessInstanceStatus, ProcessMessageDeclaration, ProcessMessageStatus,
    ProcessMessageTargetSpec, ProcessNode, ProcessNodeKind, ProcessSubProcess,
};
use uuid::Uuid;

use super::messages::{self, test_support as message_support};
use super::model::starter_model;
use super::repository::{self, PlannedEvent, PlannedScope, ProcessToken, RuntimePlan, ScopeUpdate};
use super::runtime::{
    self,
    test_support::{edge, embedded_model, publish_model, stamp, start_model, Fixture},
    StartCause,
};

fn fast_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    let body = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "LocalStart".into(),
                name: "Start locally".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "LocalEnd".into(),
                name: "End locally".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![edge("LocalFlow", "LocalStart", "LocalEnd")],
        variables: BTreeMap::from([("local_ID".into(), json!("kept"))]),
        diagram: ProcessDiagram::default(),
    };
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Scope".into(),
            name: "Fast embedded scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body,
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    model.sequence_flows = vec![
        edge("IntoScope", "Start_1", "Scope"),
        edge("AfterScope", "Scope", "End_1"),
    ];
    model
}

fn nested_model(
    owner: &str,
    outer_large: &str,
    inner_large: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.variables = BTreeMap::from([
        ("source_ID".into(), json!("origin")),
        ("shadow".into(), json!("root")),
    ]);
    let inner = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "InnerStart".into(),
                name: "Start inner".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "InnerWork".into(),
                name: "Approve inner".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(owner.into()),
                    output_mapping: BTreeMap::from([("answer".into(), "outputs.answer".into())]),
                },
            },
            ProcessNode {
                id: "InnerEnd".into(),
                name: "End inner".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![
            edge("InnerFlow1", "InnerStart", "InnerWork"),
            edge("InnerFlow2", "InnerWork", "InnerEnd"),
        ],
        variables: BTreeMap::from([
            ("innerBig".into(), json!(inner_large)),
            ("inner_ID".into(), json!("original")),
        ]),
        diagram: ProcessDiagram::default(),
    };
    let outer = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "OuterStart".into(),
                name: "Start outer".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "Inner".into(),
                name: "Inner scope".into(),
                kind: ProcessNodeKind::SubProcess {
                    body: inner,
                    input_mapping: BTreeMap::from([(
                        "inner_from_outer".into(),
                        "vars.outer_from_root".into(),
                    )]),
                    output_mapping: BTreeMap::from([(
                        "inner_answer".into(),
                        "outputs.answer".into(),
                    )]),
                },
            },
            ProcessNode {
                id: "OuterEnd".into(),
                name: "End outer".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![
            edge("OuterFlow1", "OuterStart", "Inner"),
            edge("OuterFlow2", "Inner", "OuterEnd"),
        ],
        variables: BTreeMap::from([
            ("shadow".into(), json!("local")),
            ("outerBig".into(), json!(outer_large)),
        ]),
        diagram: ProcessDiagram::default(),
    };
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Outer".into(),
            name: "Outer scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: outer,
                input_mapping: BTreeMap::from([(
                    "outer_from_root".into(),
                    "vars.source_ID".into(),
                )]),
                output_mapping: BTreeMap::from([(
                    "result_ID".into(),
                    "outputs.inner_answer".into(),
                )]),
            },
        },
    );
    model.sequence_flows = vec![
        edge("RootFlow1", "Start_1", "Outer"),
        edge("RootFlow2", "Outer", "End_1"),
    ];
    model
}

fn event_count(fixture: &Fixture, instance_id: &str) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn command_count(fixture: &Fixture) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM bpmn_commands", [], |row| row.get(0))
        .unwrap()
}

fn outbox_model(
    receiver_id: &str,
    catch_instance_id: Option<&str>,
    pending_receiver_id: Option<&str>,
    owner: &str,
    interrupt: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let first_target = match catch_instance_id {
        Some(instance_id) => ProcessMessageTargetSpec::Catch {
            definition_id: receiver_id.into(),
            instance_id_expression: Some(serde_json::to_string(instance_id).unwrap()),
            subscription_id_expression: None,
        },
        None => ProcessMessageTargetSpec::Start {
            definition_id: receiver_id.into(),
        },
    };
    let mut inner = starter_model();
    inner.messages.push(ProcessMessageDeclaration {
        message_id: "EvidenceDeclaration".into(),
        name: "EvidenceReady".into(),
    });
    inner.nodes.insert(
        1,
        ProcessNode {
            id: "ThrowEvidence".into(),
            name: "Send evidence".into(),
            kind: ProcessNodeKind::MessageThrow {
                message_ref: "EvidenceDeclaration".into(),
                target: first_target,
                correlation_expression: "'case-1'".into(),
                payload_expression: "{'customer_ID': 23}".into(),
                ttl_seconds: 120,
            },
        },
    );
    inner.sequence_flows = vec![
        edge("InnerToThrow", "Start_1", "ThrowEvidence"),
        edge("InnerThrowEnd", "ThrowEvidence", "End_1"),
    ];
    if let Some(pending_receiver_id) = pending_receiver_id {
        inner.messages.push(ProcessMessageDeclaration {
            message_id: "PendingDeclaration".into(),
            name: "EvidencePending".into(),
        });
        inner.nodes.insert(
            2,
            ProcessNode {
                id: "ThrowPending".into(),
                name: "Queue later evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "PendingDeclaration".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: pending_receiver_id.into(),
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'customer_ID': 24}".into(),
                    ttl_seconds: 120,
                },
            },
        );
        inner.sequence_flows = vec![
            edge("InnerToThrow", "Start_1", "ThrowEvidence"),
            edge("ToPending", "ThrowEvidence", "ThrowPending"),
            edge("PendingEnd", "ThrowPending", "End_1"),
        ];
    }
    let mut outer = embedded_model(inner, "Inner");
    outer.nodes.insert(
        2,
        ProcessNode {
            id: "OuterWork".into(),
            name: "Finish outer work".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(owner.into()),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    outer.sequence_flows = vec![
        edge("OuterToInner", "RootStart_Inner", "Inner"),
        edge("InnerToWork", "Inner", "OuterWork"),
        edge("WorkToEnd", "OuterWork", "RootEnd_Inner"),
    ];
    let mut model = embedded_model(outer, "Outer");
    if interrupt {
        model.messages.push(ProcessMessageDeclaration {
            message_id: "StopDeclaration".into(),
            name: "StopWork".into(),
        });
        model.nodes.push(ProcessNode {
            id: "StopOuter".into(),
            name: "Interrupt outer scope".into(),
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "Outer".into(),
                cancel_activity: true,
                message_ref: "StopDeclaration".into(),
                correlation_expression: "'case-1'".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        model
            .sequence_flows
            .push(edge("StopToEnd", "StopOuter", "RootEnd_Outer"));
    }
    model
}

#[test]
fn nested_fast_scope_persists_root_child_and_replays_one_start() {
    let fixture = Fixture::new();
    let model = fast_model();
    let version = publish_model(&fixture, &model);
    let first_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let actor = &fixture.owner;
    let first_plan = runtime::plan_start(
        &version.model,
        &first_id,
        actor,
        &version.definition_id,
        version.version,
        variables.clone(),
        StartCause::Manual,
        1_000,
    )
    .unwrap();
    let command = stamp("embedded-fast-start");
    let completed = repository::start_instance(
        &fixture.db,
        actor,
        &command,
        &first_id,
        &version.definition_id,
        version.version,
        &variables,
        &first_plan,
        1_000,
    )
    .unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.scopes.len(), 2);
    assert_eq!(
        completed
            .scopes
            .iter()
            .find(|scope| scope.parent_scope_id.is_none())
            .unwrap()
            .scope_id,
        first_id
    );
    let child = completed
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.as_deref() == Some(first_id.as_str()))
        .unwrap();
    assert_eq!(child.subprocess_node_id.as_deref(), Some("Scope"));
    assert_eq!(child.status, ProcessInstanceStatus::Completed);
    let persisted: (i64, i64) = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*),COUNT(CASE WHEN parent_scope_id IS NULL AND local_variables_json IS NULL THEN 1 END) FROM bpmn_scopes WHERE instance_id=?1",
        [&first_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(persisted, (2, 1));
    let events_before = event_count(&fixture, &first_id);
    assert!(
        events_before >= 4,
        "start, entry, child completion and instance completion are persisted"
    );

    let replacement_id = Uuid::new_v4().to_string();
    let replacement_plan = runtime::plan_start(
        &version.model,
        &replacement_id,
        actor,
        &version.definition_id,
        version.version,
        variables.clone(),
        StartCause::Manual,
        2_000,
    )
    .unwrap();
    let replay = repository::start_instance(
        &fixture.db,
        actor,
        &command,
        &replacement_id,
        &version.definition_id,
        version.version,
        &variables,
        &replacement_plan,
        2_000,
    )
    .unwrap();
    assert_eq!(replay.instance_id, first_id);
    assert_eq!(replay.scopes, completed.scopes);
    assert_eq!(event_count(&fixture, &first_id), events_before);
    assert!(repository::get_instance(&fixture.db, actor, &replacement_id, None).is_err());
}

#[test]
fn scoped_work_uses_lexical_inputs_outputs_and_authorized_scope_detail() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let waiting = start_model(&fixture, &model);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    assert_eq!(waiting.scopes.len(), 3);
    let root = waiting
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.is_none())
        .unwrap();
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap();
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    assert_eq!(root.scope_id, waiting.instance_id);
    assert_eq!(
        outer.parent_scope_id.as_deref(),
        Some(root.scope_id.as_str())
    );
    assert_eq!(
        inner.parent_scope_id.as_deref(),
        Some(outer.scope_id.as_str())
    );
    let (detail, local, active) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &inner.scope_id,
    )
    .unwrap();
    assert_eq!(detail.subprocess_node_name.as_deref(), Some("Inner scope"));
    assert_eq!(local["inner_from_outer"], "origin");
    assert_eq!(active, vec!["InnerWork"]);
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.participant,
        &waiting.instance_id,
        &inner.scope_id
    )
    .is_err());
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        "forged-scope"
    )
    .is_err());
    let other_instance = start_model(&fixture, &model);
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &other_instance.instance_id,
        &inner.scope_id
    )
    .is_err());

    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    let task_id = snapshot
        .user_tasks
        .iter()
        .find(|task| task.scope_id == inner.scope_id)
        .unwrap()
        .user_task_id
        .clone();
    let outputs = json!({"answer":"approved"});
    let plan = runtime::plan_user_completion(&snapshot, &task_id, &outputs, None, 2_000).unwrap();
    let completed = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &stamp("scoped-complete"),
        &waiting.instance_id,
        &task_id,
        waiting.revision,
        &outputs,
        None,
        &plan,
        2_000,
    )
    .unwrap()
    .instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["result_ID"], "approved");
    assert_eq!(completed.scopes.len(), 3);
    assert!(completed
        .scopes
        .iter()
        .all(|scope| scope.status == ProcessInstanceStatus::Completed));
}

#[test]
fn forged_scope_parent_or_cross_body_node_cannot_publish_a_partial_transition() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let waiting = start_model(&fixture, &model);
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap();
    let events_before = event_count(&fixture, &waiting.instance_id);
    for (parent_scope_id, node_id) in [
        (Uuid::new_v4().to_string(), "Outer"),
        (outer.scope_id.clone(), "Outer"),
    ] {
        let scope_id = Uuid::new_v4().to_string();
        let parent_token_id = Uuid::new_v4().to_string();
        let mut forged = RuntimePlan::initial(waiting.variables.clone());
        forged.status = waiting.status.clone();
        forged.create_tokens.push(ProcessToken {
            token_id: parent_token_id.clone(),
            scope_id: parent_scope_id.clone(),
            node_id: node_id.into(),
            arrival_edge_id: None,
            fork_stack: Vec::new(),
            status: "waiting".into(),
        });
        forged.create_scopes.push(PlannedScope {
            scope_id: scope_id.clone(),
            parent_scope_id: parent_scope_id.clone(),
            parent_token_id: parent_token_id.clone(),
            subprocess_node_id: node_id.into(),
            variables: json!({}),
        });
        forged.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_entered".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":parent_scope_id,
                "parent_token_id":parent_token_id,"subprocess_node_id":node_id}),
        });
        assert!(repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            waiting.revision,
            &forged,
            2_000
        )
        .is_err());
        assert!(repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &scope_id
        )
        .is_err());
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None)
                .unwrap()
                .revision,
            waiting.revision
        );
        assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    }
}

#[test]
fn ancestor_variable_change_revalidates_all_active_descendants_atomically() {
    let fixture = Fixture::new();
    let model = nested_model(
        &fixture.owner.user_id,
        &"o".repeat(70_000),
        &"i".repeat(20_000),
    );
    let waiting = start_model(&fixture, &model);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap()
        .clone();
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap()
        .clone();
    let events_before = event_count(&fixture, &waiting.instance_id);
    let commands_before = command_count(&fixture);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    let task_before = snapshot
        .user_tasks
        .iter()
        .find(|task| task.scope_id == inner.scope_id)
        .unwrap()
        .clone();

    let mut invalid = RuntimePlan::initial(waiting.variables.clone());
    invalid.status = waiting.status.clone();
    invalid.variables["newLarge"] = Value::String("x".repeat(180_000));
    assert!(
        repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            waiting.revision,
            &invalid,
            2_000
        )
        .is_err(),
        "unchanged grandchild effective variables exceed 256 KiB"
    );
    let after_rejection =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    assert_eq!(after_rejection.revision, waiting.revision);
    assert_eq!(after_rejection.variables, waiting.variables);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(command_count(&fixture), commands_before);
    let snapshot_after_rejection =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    assert_eq!(
        snapshot_after_rejection.scope_variables,
        snapshot.scope_variables
    );
    assert_eq!(
        serde_json::to_value(&snapshot_after_rejection.tokens).unwrap(),
        serde_json::to_value(&snapshot.tokens).unwrap()
    );
    assert_eq!(
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap()
            .user_tasks
            .iter()
            .find(|task| task.scope_id == inner.scope_id)
            .unwrap()
            .status,
        task_before.status
    );

    let mut shadowed = RuntimePlan::initial(waiting.variables.clone());
    shadowed.status = waiting.status.clone();
    shadowed.variables["shadow"] = Value::String("s".repeat(180_000));
    let legal = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        waiting.revision,
        &shadowed,
        3_000,
    )
    .unwrap()
    .instance;
    assert_eq!(legal.variables["shadow"].as_str().unwrap().len(), 180_000);
    assert_eq!(legal.status, ProcessInstanceStatus::Waiting);
    let (outer_after, local, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &outer.scope_id,
    )
    .unwrap();
    assert_eq!(local["shadow"], "local");

    let mut removed_shadow = RuntimePlan::initial(legal.variables.clone());
    removed_shadow.status = legal.status.clone();
    let mut exposed = local.as_object().unwrap().clone();
    exposed.remove("shadow");
    removed_shadow.scope_updates.push(ScopeUpdate {
        scope_id: outer.scope_id.clone(),
        expected_revision: outer_after.revision,
        status: outer_after.status.clone(),
        variables: Some(Value::Object(exposed)),
    });
    assert!(
        repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            legal.revision,
            &removed_shadow,
            4_000
        )
        .is_err(),
        "removing a shadow exposes oversized ancestor value to unchanged descendants"
    );
    let unchanged =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    assert_eq!(unchanged.revision, legal.revision);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(command_count(&fixture), commands_before);
    let (_, local_after, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &outer.scope_id,
    )
    .unwrap();
    assert_eq!(local_after["shadow"], "local");
    assert_eq!(
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap()
            .user_tasks
            .iter()
            .find(|task| task.scope_id == inner.scope_id)
            .unwrap()
            .status,
        task_before.status
    );

    let (inner_after, inner_local, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &inner.scope_id,
    )
    .unwrap();
    let mut legal_outer_local = local.as_object().unwrap().clone();
    legal_outer_local.remove("shadow");
    let mut legal_inner_local = inner_local.as_object().unwrap().clone();
    legal_inner_local.insert("shadow".into(), json!("nested shadow"));
    let mut simultaneous = RuntimePlan::initial(legal.variables.clone());
    simultaneous.status = legal.status.clone();
    simultaneous.scope_updates.extend([
        ScopeUpdate {
            scope_id: outer.scope_id.clone(),
            expected_revision: outer_after.revision,
            status: outer_after.status,
            variables: Some(Value::Object(legal_outer_local)),
        },
        ScopeUpdate {
            scope_id: inner.scope_id.clone(),
            expected_revision: inner_after.revision,
            status: inner_after.status,
            variables: Some(Value::Object(legal_inner_local)),
        },
    ]);
    let accepted = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        legal.revision,
        &simultaneous,
        5_000,
    )
    .unwrap()
    .instance;
    assert_eq!(accepted.revision, legal.revision + 1);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &outer.scope_id
        )
        .unwrap()
        .1
        .get("shadow"),
        None
    );
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &inner.scope_id
        )
        .unwrap()
        .1["shadow"],
        "nested shadow"
    );

    let mut terminal_model = nested_model(&fixture.owner.user_id, "", &"i".repeat(20_000));
    let ProcessNodeKind::SubProcess {
        body: outer_body, ..
    } = &mut terminal_model.nodes[1].kind
    else {
        panic!("the fixture must retain its outer embedded body");
    };
    let ProcessNodeKind::SubProcess {
        body: inner_body,
        output_mapping: inner_output,
        ..
    } = &mut outer_body.nodes[1].kind
    else {
        panic!("the fixture must retain its inner embedded body");
    };
    inner_output.clear();
    inner_body.nodes.retain(|node| node.id != "InnerWork");
    inner_body.sequence_flows = vec![edge("InnerDirectEnd", "InnerStart", "InnerEnd")];
    outer_body.nodes.insert(
        2,
        ProcessNode {
            id: "OuterWait".into(),
            name: "Keep the ancestor active".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    outer_body.sequence_flows = vec![
        edge("OuterToInner", "OuterStart", "Inner"),
        edge("InnerToWait", "Inner", "OuterWait"),
        edge("WaitToEnd", "OuterWait", "OuterEnd"),
    ];
    let terminal_waiting = start_model(&fixture, &terminal_model);
    let finished_inner = terminal_waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    assert_eq!(finished_inner.status, ProcessInstanceStatus::Completed);
    let terminal_inner_id = finished_inner.scope_id.clone();
    let terminal_local = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        &terminal_inner_id,
    )
    .unwrap()
    .1;
    let terminal_events = repository::list_events(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        0,
        100,
    )
    .unwrap()
    .0;
    let mut root_change = RuntimePlan::initial(terminal_waiting.variables.clone());
    root_change.status = terminal_waiting.status.clone();
    root_change.variables["terminal_only_large"] = Value::String("x".repeat(250_000));
    let terminal_after = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        terminal_waiting.revision,
        &root_change,
        6_000,
    )
    .unwrap()
    .instance;
    assert_eq!(terminal_after.revision, terminal_waiting.revision + 1);
    assert_eq!(terminal_after.status, ProcessInstanceStatus::Waiting);
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &terminal_waiting.instance_id,
            &terminal_inner_id
        )
        .unwrap()
        .1,
        terminal_local
    );
    assert_eq!(
        terminal_after
            .scopes
            .iter()
            .find(|scope| scope.scope_id == terminal_inner_id)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::list_events(
            &fixture.db,
            &fixture.owner,
            &terminal_waiting.instance_id,
            0,
            100
        )
        .unwrap()
        .0,
        terminal_events
    );
    assert!(terminal_after
        .user_tasks
        .iter()
        .any(|task| task.node_id == "OuterWait"
            && task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open));
}

#[test]
fn scope_lifetime_limit_records_an_incident_and_retains_the_waiting_parent_activation() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let initial = start_model(&fixture, &model);
    assert_eq!(initial.scopes.len(), 3);
    let original =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &initial.instance_id).unwrap();
    let outer_scope_id = original
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap()
        .scope_id
        .clone();
    let mut outer_variables = original.scope_variables[&outer_scope_id].clone();
    outer_variables["inner_answer"] = json!("completed");
    let mut expansion = RuntimePlan::initial(initial.variables.clone());
    expansion.status = initial.status.clone();
    for _ in 0..126 {
        let scope_id = Uuid::new_v4().to_string();
        let parent_token_id = Uuid::new_v4().to_string();
        let end_token_id = Uuid::new_v4().to_string();
        expansion.create_tokens.push(ProcessToken {
            token_id: parent_token_id.clone(),
            scope_id: initial.instance_id.clone(),
            node_id: "Outer".into(),
            arrival_edge_id: None,
            fork_stack: Vec::new(),
            status: "waiting".into(),
        });
        expansion.create_tokens.push(ProcessToken {
            token_id: end_token_id.clone(),
            scope_id: scope_id.clone(),
            node_id: "OuterEnd".into(),
            arrival_edge_id: Some("OuterFlow2".into()),
            fork_stack: Vec::new(),
            status: "ready".into(),
        });
        expansion.create_scopes.push(PlannedScope {
            scope_id: scope_id.clone(),
            parent_scope_id: initial.instance_id.clone(),
            parent_token_id: parent_token_id.clone(),
            subprocess_node_id: "Outer".into(),
            variables: outer_variables.clone(),
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_entered".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":initial.instance_id.clone(),
                "parent_token_id":parent_token_id,"subprocess_node_id":"Outer"}),
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "end_reached".into(),
            node_id: Some("OuterEnd".into()),
            data: Value::Null,
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_completed".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":initial.instance_id.clone(),
                "parent_token_id":parent_token_id,"subprocess_node_id":"Outer"}),
        });
        expansion.consume_token_ids.push(parent_token_id);
        expansion.consume_token_ids.push(end_token_id);
        expansion.scope_updates.push(ScopeUpdate {
            scope_id,
            expected_revision: 1,
            status: ProcessInstanceStatus::Completed,
            variables: None,
        });
    }
    expansion.create_tokens.push(ProcessToken {
        token_id: Uuid::new_v4().to_string(),
        scope_id: initial.instance_id.clone(),
        node_id: "Outer".into(),
        arrival_edge_id: None,
        fork_stack: Vec::new(),
        status: "ready".into(),
    });
    let expanded = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &initial.instance_id,
        initial.revision,
        &expansion,
        2_000,
    )
    .unwrap()
    .instance;
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &initial.instance_id).unwrap();
    assert_eq!(snapshot.scopes.len(), 129);
    let plan = runtime::plan_advance(&snapshot, 3_000).unwrap();
    assert!(plan
        .add_incidents
        .iter()
        .any(|incident| incident.code == "SCOPE_LIMIT"));
    let failed = plan
        .events
        .iter()
        .find(|event| event.kind == "scope_entry_failed")
        .unwrap();
    let retained_token = failed.data["parent_token_id"].as_str().unwrap();
    assert!(plan
        .create_tokens
        .iter()
        .any(|token| token.token_id == retained_token
            && token.scope_id == initial.instance_id
            && token.node_id == "Outer"
            && token.status == "waiting"));
    let result = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &initial.instance_id,
        expanded.revision,
        &plan,
        3_000,
    )
    .unwrap()
    .instance;
    assert!(result
        .incidents
        .iter()
        .any(|incident| incident.code == "SCOPE_LIMIT"));
    let persisted: (String, String) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT scope_id,status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
            rusqlite::params![initial.instance_id, retained_token],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(persisted, (initial.instance_id, "waiting".into()));
    assert!(event_count(&fixture, &result.instance_id) > 0);
}

#[test]
fn interrupted_ancestor_retracts_completed_descendant_outbox_without_erasing_facts() {
    let fixture = Fixture::new();
    let receiver = publish_model(&fixture, &message_support::receiving_model(true, false));
    let mut pending_receiver_model = message_support::receiving_model(true, false);
    pending_receiver_model.messages[0].name = "EvidencePending".into();
    let pending_receiver = publish_model(&fixture, &pending_receiver_model);
    let model = outbox_model(
        &receiver.definition_id,
        None,
        Some(&pending_receiver.definition_id),
        &fixture.owner.user_id,
        true,
    );
    let source_version = publish_model(&fixture, &model);
    let waiting = message_support::start_version(&fixture, &source_version);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    let inner_id = inner.scope_id.clone();
    assert_eq!(inner.status, ProcessInstanceStatus::Completed);
    let before_detail =
        repository::get_scope(&fixture.db, &fixture.owner, &waiting.instance_id, &inner_id)
            .unwrap();
    assert_eq!(waiting.outgoing_messages.len(), 2);
    let deliver_id = waiting
        .outgoing_messages
        .iter()
        .find(|message| message.message_name == "EvidenceReady")
        .unwrap()
        .message_id
        .clone();
    let pending = waiting
        .outgoing_messages
        .iter()
        .find(|message| message.message_name == "EvidencePending")
        .unwrap();
    assert_eq!(pending.status, ProcessMessageStatus::Pending);
    assert_eq!(pending.source_scope_id.as_deref(), Some(inner_id.as_str()));
    let pending_id = pending.message_id.clone();
    let first_at_ms = chrono::Utc::now().timestamp_millis();
    let deliver_candidate = repository::due_messages(&fixture.db, first_at_ms, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.key.message_id == deliver_id)
        .unwrap();
    let repository::MessageSelection::Ready(deliver_prepared) =
        repository::message_snapshot(&fixture.db, &deliver_candidate).unwrap()
    else {
        panic!("the completed descendant's first message must have a real matching start");
    };
    let deliver_plan = messages::plan_message_delivery(&deliver_prepared, first_at_ms).unwrap();
    let first_delivery =
        repository::deliver_message(&fixture.db, &deliver_prepared, &deliver_plan, first_at_ms)
            .unwrap()
            .unwrap();
    assert_eq!(
        first_delivery.message.status,
        ProcessMessageStatus::Delivered
    );
    let before_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;

    let mut stop = message_support::envelope(
        message_support::catch_target(&source_version, Some(&waiting.instance_id), None),
        Value::Null,
    );
    stop.message_name = "StopWork".into();
    let stop_receipt = message_support::send(&fixture, &stop);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.key.message_id == stop_receipt.message_id)
        .unwrap();
    let repository::MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
    else {
        panic!("the actual boundary subscription must accept the stop message");
    };
    let plan = messages::plan_message_delivery(&prepared, at_ms).unwrap();
    let delivered = repository::deliver_message(&fixture.db, &prepared, &plan, at_ms)
        .unwrap()
        .unwrap();
    assert_eq!(delivered.message.status, ProcessMessageStatus::Delivered);

    let after =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    let inner_after = after
        .scopes
        .iter()
        .find(|scope| scope.scope_id == inner_id)
        .unwrap();
    assert_eq!(inner_after.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        repository::get_scope(&fixture.db, &fixture.owner, &waiting.instance_id, &inner_id)
            .unwrap()
            .1,
        before_detail.1
    );
    let cancelled = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        &pending_id,
    )
    .unwrap();
    assert_eq!(cancelled.message.status, ProcessMessageStatus::Cancelled);
    assert_eq!(
        cancelled.message.last_reason.as_deref(),
        Some("scope_cancelled")
    );
    assert_eq!(
        cancelled.message.source_scope_id.as_deref(),
        Some(inner_id.as_str())
    );
    let after_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;
    assert!(before_events.iter().all(|event| after_events
        .iter()
        .any(|kept| kept.seq == event.seq && kept.kind == event.kind && kept.data == event.data)));
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_queued")
            .count(),
        2
    );
    assert_eq!(
        repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            &deliver_id
        )
        .unwrap()
        .message
        .status,
        ProcessMessageStatus::Delivered
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_delivered" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_cancelled" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "scope_completed" && event.scope_id == inner_id)
            .count(),
        1
    );
}

#[test]
fn normal_scope_completion_keeps_pending_outbox_until_real_catch_delivery_once() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &message_support::receiving_model(false, true));
    let receiver = message_support::start_version(&fixture, &receiver_version);
    let source_version = publish_model(
        &fixture,
        &outbox_model(
            &receiver_version.definition_id,
            Some(&receiver.instance_id),
            None,
            &fixture.owner.user_id,
            false,
        ),
    );
    let waiting = message_support::start_version(&fixture, &source_version);
    let inner_id = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap()
        .scope_id
        .clone();
    let work_id = waiting
        .user_tasks
        .iter()
        .find(|task| task.node_id == "OuterWork")
        .unwrap()
        .user_task_id
        .clone();
    let message_id = waiting.outgoing_messages[0].message_id.clone();
    assert_eq!(
        waiting.outgoing_messages[0].status,
        ProcessMessageStatus::Pending
    );
    let completed = message_support::complete(&fixture, &waiting.instance_id, &work_id);
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert!(completed
        .scopes
        .iter()
        .all(|scope| scope.status == ProcessInstanceStatus::Completed));
    assert_eq!(
        repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            &message_id
        )
        .unwrap()
        .message
        .status,
        ProcessMessageStatus::Pending
    );

    let gate_id = receiver
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Gate_1")
        .unwrap()
        .user_task_id
        .clone();
    let armed = message_support::complete(&fixture, &receiver.instance_id, &gate_id);
    assert_eq!(
        armed
            .subscriptions
            .iter()
            .filter(|subscription| subscription.node_id == "Catch_1"
                && subscription.status
                    == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .count(),
        1
    );
    let at_ms = chrono::Utc::now().timestamp_millis();
    let first = messages::drain_pending(&fixture.db, at_ms);
    first.completion.unwrap();
    assert_eq!(first.delivered, 1);
    let second = messages::drain_pending(&fixture.db, at_ms + 1);
    second.completion.unwrap();
    assert_eq!(second.delivered, 0);
    let receipt = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        &message_id,
    )
    .unwrap();
    assert_eq!(receipt.message.status, ProcessMessageStatus::Delivered);
    assert_eq!(
        receipt.message.source_scope_id.as_deref(),
        Some(inner_id.as_str())
    );
    assert_eq!(
        receipt.message.matched_instance_id.as_deref(),
        Some(receiver.instance_id.as_str())
    );
    let source_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "message_queued" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "message_delivered" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "scope_completed" && event.scope_id == inner_id)
            .count(),
        1
    );
}
