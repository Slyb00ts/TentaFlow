// ============ File: simulation_repetition_tests.rs — multi-instance and loop groups in a private simulation ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessInstanceStatus, ProcessModel, ProcessMultiInstanceInput, ProcessMultiInstanceMode,
    ProcessNodeKind, ProcessRepeatSpec, ProcessRepetitionGroupMode, ProcessRepetitionGroupStatus,
    ProcessRepetitionOccurrenceStatus,
};

use super::model::starter_model;
use super::runtime::test_support::edge;
use super::simulation::{
    SimulationStore, SimulationView, MAX_SIMULATION_OCCURRENCES, MAX_SIMULATION_REPETITION_GROUPS,
    MAX_SIMULATION_SCOPES, SIMULATION_REPETITION_LIMIT, SIMULATION_SCOPE_LIMIT,
};
use super::simulation_scope_tests::{
    advance_until, body, complete_open_work, node, open_work, refusal_code, started, sub_process,
    user_task,
};
use super::simulation_service_tests::normalize_events;
use super::simulation_tests::test_authorization;

fn multi_instance(mode: ProcessMultiInstanceMode, count: u8) -> ProcessRepeatSpec {
    ProcessRepeatSpec::MultiInstance {
        mode,
        input: ProcessMultiInstanceInput::Cardinality { count },
        output_collection_variable: "results".into(),
    }
}

/// Start, one repeated user task, End.
pub(crate) fn repeated_task_model(
    assignee: Option<&str>,
    repeat: ProcessRepeatSpec,
) -> ProcessModel {
    let mut model = starter_model();
    model.variables = BTreeMap::from([("results".into(), json!([]))]);
    let mut review = node("Review_1", user_task(assignee));
    review.repeat = Some(repeat);
    model.nodes = vec![
        model.nodes[0].clone(),
        review,
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToReview", "Start_1", "Review_1"),
        edge("ReviewEnd", "Review_1", "End_1"),
    ];
    model
}

fn occurrence_task(view: &SimulationView, ordinal: u32) -> String {
    view.repetition_occurrences
        .iter()
        .find(|occurrence| occurrence.ordinal == ordinal)
        .and_then(|occurrence| occurrence.user_task_id.clone())
        .unwrap_or_else(|| panic!("occurrence {ordinal} has no task"))
}

fn complete(store: &mut SimulationStore, task_id: &str, answer: Value) -> SimulationView {
    store
        .complete_user_task(
            test_authorization(store),
            task_id,
            json!({ "answer": answer }),
            None,
        )
        .expect("complete the occurrence task")
}

#[test]
fn a_parallel_multi_instance_opens_every_occurrence_and_aggregates_the_answers() {
    let mut store = started(repeated_task_model(
        Some("user-simulation"),
        multi_instance(ProcessMultiInstanceMode::Parallel, 3),
    ));
    let view = advance_until(&mut store, "three open tasks", |view| {
        open_work(view).len() == 3
    });
    let group = &view.repetition_groups[0];
    assert_eq!(group.node_id, "Review_1");
    assert_eq!(group.node_name, "Step Review_1");
    assert_eq!(
        group.mode,
        ProcessRepetitionGroupMode::MultiInstanceParallel
    );
    assert_eq!(
        (group.total, group.created_count, group.completed),
        (Some(3), 3, 0)
    );
    assert_eq!(group.status, ProcessRepetitionGroupStatus::Open);
    assert_eq!(view.repetition_occurrences.len(), 3);
    assert!(view
        .repetition_occurrences
        .iter()
        .all(|occurrence| occurrence.status == ProcessRepetitionOccurrenceStatus::Active));

    // Answered out of order: the aggregate keeps the ordinals.
    let mut view = view;
    for ordinal in [2, 0, 1] {
        let task = occurrence_task(&view, ordinal);
        view = complete(&mut store, &task, json!(format!("answer {ordinal}")));
    }
    let instance = view.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        instance.variables["results"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(view.repetition_groups[0].completed, 3);
    assert_eq!(
        view.repetition_groups[0].status,
        ProcessRepetitionGroupStatus::Completed
    );
}

#[test]
fn a_sequential_multi_instance_opens_one_occurrence_at_a_time() {
    let mut store = started(repeated_task_model(
        Some("user-simulation"),
        multi_instance(ProcessMultiInstanceMode::Sequential, 2),
    ));
    let view = advance_until(&mut store, "the first task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(open_work(&view).len(), 1);
    assert_eq!(view.repetition_groups[0].created_count, 1);
    let view = complete_open_work(&mut store, json!({"answer": "first"}));
    let view = if open_work(&view).is_empty() {
        advance_until(&mut store, "the second task", |view| {
            !open_work(view).is_empty()
        })
    } else {
        view
    };
    assert_eq!(open_work(&view).len(), 1);
    assert_eq!(view.repetition_groups[0].created_count, 2);
    let done = complete_open_work(&mut store, json!({"answer": "second"}));
    assert_eq!(
        done.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn a_repetition_run_repeats_with_the_same_ids_and_trace() {
    let run = || {
        let mut store = started(repeated_task_model(
            Some("user-simulation"),
            multi_instance(ProcessMultiInstanceMode::Parallel, 2),
        ));
        let mut view = advance_until(&mut store, "two open tasks", |view| {
            open_work(view).len() == 2
        });
        for ordinal in [1, 0] {
            let task = occurrence_task(&view, ordinal);
            view = complete(&mut store, &task, json!(ordinal));
        }
        (
            view.repetition_groups
                .iter()
                .map(|group| group.group_id.clone())
                .collect::<Vec<_>>(),
            view.repetition_occurrences
                .iter()
                .map(|occurrence| occurrence.occurrence_id.clone())
                .collect::<Vec<_>>(),
            view.events
                .iter()
                .map(|event| {
                    (
                        event.event_id.clone(),
                        event.kind.clone(),
                        event.data.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            view.trace_steps
                .iter()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone()))
                .collect::<Vec<_>>(),
        )
    };
    let (first, second) = (run(), run());
    assert_eq!((first.0.len(), first.1.len()), (1, 2));
    assert_eq!(first, second);
}

#[tokio::test]
async fn the_simulated_repetition_trace_equals_the_production_execution() {
    use super::repository;
    use super::runtime::test_support::{stamp, start_model, Fixture};

    let model = repeated_task_model(None, multi_instance(ProcessMultiInstanceMode::Parallel, 2));
    let production = Fixture::new();
    let instance = start_model(&production, &model);
    for ordinal in [1_u32, 0] {
        let snapshot =
            repository::runtime_snapshot(&production.db, &production.owner, &instance.instance_id)
                .expect("production snapshot");
        let task_id = snapshot
            .repetition_occurrences
            .iter()
            .find(|occurrence| occurrence.ordinal == ordinal)
            .and_then(|occurrence| occurrence.user_task_id.clone())
            .expect("production opens a task per occurrence");
        repository::complete_user_task(
            &production.db,
            &production.owner,
            &stamp("complete"),
            &instance.instance_id,
            &task_id,
            snapshot.instance.revision,
            &json!({ "answer": ordinal }),
            None,
            repository::ProcessPlanInput::Canonical,
            chrono::Utc::now().timestamp_millis(),
        )
        .expect("complete the production occurrence");
    }
    let production_events = normalize_events(
        repository::list_events(
            &production.db,
            &production.owner,
            &instance.instance_id,
            0,
            200,
        )
        .expect("production history")
        .0
        .into_iter()
        .map(|event| (event.kind, event.node_id, event.data))
        .collect(),
    );
    let production_variables = repository::get_instance(
        &production.db,
        &production.owner,
        &instance.instance_id,
        None,
    )
    .expect("production instance")
    .variables;

    let mut model = model;
    if let ProcessNodeKind::UserTask {
        assignee_user_id, ..
    } = &mut model.nodes[1].kind
    {
        *assignee_user_id = Some("user-simulation".into());
    }
    let mut store = started(model);
    let mut view = advance_until(&mut store, "two open tasks", |view| {
        open_work(view).len() == 2
    });
    for ordinal in [1_u32, 0] {
        let task = occurrence_task(&view, ordinal);
        view = complete(&mut store, &task, json!(ordinal));
    }
    let simulated_events = normalize_events(
        view.events
            .iter()
            .map(|event| {
                (
                    event.kind.clone(),
                    event.node_id.clone(),
                    event.data.clone(),
                )
            })
            .collect(),
    );
    assert_eq!(simulated_events, production_events);
    assert_eq!(
        view.instance.as_ref().expect("instance").variables,
        production_variables
    );
}

#[test]
fn a_structured_loop_runs_until_its_condition_ends_it() {
    let mut model = repeated_task_model(
        Some("user-simulation"),
        ProcessRepeatSpec::StructuredLoop {
            condition: "vars.answer != \"done\"".into(),
            test_before: false,
            max_iterations: 5,
            output_collection_variable: "results".into(),
        },
    );
    model.variables.insert("answer".into(), json!("again"));
    let mut store = started(model);
    let mut view = advance_until(&mut store, "the first pass", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(
        view.repetition_groups[0].mode,
        ProcessRepetitionGroupMode::StructuredLoop
    );
    assert_eq!(view.repetition_groups[0].max_iterations, Some(5));
    for pass in 0..2 {
        view = complete_open_work(
            &mut store,
            json!({"answer": if pass == 1 { "done" } else { "again" }}),
        );
        if pass == 0 && open_work(&view).is_empty() {
            view = advance_until(&mut store, "the second pass", |view| {
                !open_work(view).is_empty()
            });
        }
    }
    let instance = view.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        view.repetition_groups[0].completed, 2,
        "the loop stopped after the answer 'done'"
    );
    assert_eq!(view.repetition_occurrences.len(), 2);
}

/// Start, then `count` repeated sub-processes in a row, each around one person's task.
fn repeated_scopes_model(count: usize, occurrences: u8) -> ProcessModel {
    let mut model = starter_model();
    let mut nodes = vec![model.nodes[0].clone()];
    let mut flows = Vec::new();
    let mut previous = "Start_1".to_owned();
    for index in 0..count {
        model.variables.insert(format!("out_{index}"), json!([]));
        let (scope, start, work, end) = (
            format!("Scope_{index}"),
            format!("InnerStart_{index}"),
            format!("InnerWork_{index}"),
            format!("InnerEnd_{index}"),
        );
        let inner = body(
            vec![
                node(&start, ProcessNodeKind::Start),
                node(&work, user_task(Some("user-simulation"))),
                node(&end, ProcessNodeKind::End),
            ],
            vec![
                edge(&format!("InnerFlowA_{index}"), &start, &work),
                edge(&format!("InnerFlowB_{index}"), &work, &end),
            ],
        );
        let mut repeated = node(&scope, sub_process(inner, &[], &[]));
        repeated.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: occurrences },
            output_collection_variable: format!("out_{index}"),
        });
        nodes.push(repeated);
        flows.push(edge(&format!("To_{index}"), &previous, &scope));
        previous = scope;
    }
    nodes.push(model.nodes.last().expect("starter End").clone());
    flows.push(edge("ToEnd", &previous, "End_1"));
    model.nodes = nodes;
    model.sequence_flows = flows;
    model
}

#[test]
fn repeated_sub_processes_hit_the_scope_limit_with_a_stable_code_and_leave_the_run_intact() {
    // Two rounds of sixteen scopes fill the limit exactly; a third would pass it.
    let rounds = MAX_SIMULATION_SCOPES / 16 + 1;
    let mut store = started(repeated_scopes_model(rounds, 16));
    let mut refusal = None;
    'run: for _ in 0..=(rounds * 16 + 8) {
        let view = store
            .view(test_authorization(&store))
            .expect("read the run");
        let tasks = open_work(&view)
            .iter()
            .map(|task| task.user_task_id.clone())
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            if let Err(error) = store.advance(test_authorization(&store)) {
                refusal = Some(error);
                break;
            }
            continue;
        }
        for task in tasks {
            if let Err(error) = store.complete_user_task(
                test_authorization(&store),
                &task,
                json!({"answer": true}),
                None,
            ) {
                refusal = Some(error);
                break 'run;
            }
        }
    }
    let error = refusal.expect("the third round must pass the scope limit");
    assert_eq!(refusal_code(&error), SIMULATION_SCOPE_LIMIT);
    let view = store
        .view(test_authorization(&store))
        .expect("read the run");
    assert_eq!(
        view.scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .count(),
        MAX_SIMULATION_SCOPES,
        "the refused transition opened no scope"
    );
    assert_ne!(
        view.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Error
    );
}

fn plan_with_repetition(groups: usize, occurrences: usize) -> super::repository::RuntimePlan {
    use super::repository::{RepetitionGroup, RepetitionOccurrence};
    use tentaflow_protocol::processes::{
        ProcessRepetitionGroupStatus as G, ProcessRepetitionOccurrenceStatus as O,
    };
    let id =
        |kind: u128, index: usize| uuid::Uuid::from_u128((kind << 64) | index as u128).to_string();
    let mut plan = super::repository::RuntimePlan::initial(json!({}));
    plan.repetition_groups = (0..groups)
        .map(|index| RepetitionGroup {
            group_id: id(1, index),
            instance_id: id(9, 0),
            scope_id: id(9, 0),
            definition_id: "definition".into(),
            version: 1,
            node_id: "Review_1".into(),
            mode: ProcessRepetitionGroupMode::MultiInstanceParallel,
            status: G::Open,
            source_token_id: id(8, 0),
            parent_token_id: id(8, 1),
            entry_source_event_id: None,
            output_collection_variable: "results".into(),
            entry_variables: json!({}),
            frozen_collection: None,
            total_count: None,
            created_count: 0,
            completed_count: 0,
            max_iterations: None,
            loop_state: None,
            loop_state_revision: 0,
            next_ordinal: 0,
            retained_bytes: 0,
            terminal_capacity: false,
            terminal_incident_id: None,
            terminal_event_id: None,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
        })
        .collect();
    plan.repetition_occurrences = (0..occurrences)
        .map(|index| RepetitionOccurrence {
            occurrence_id: id(2, index),
            instance_id: id(9, 0),
            scope_id: id(9, 0),
            group_id: id(1, 0),
            ordinal: index as u32,
            status: O::Pending,
            token_id: id(8, 2),
            user_task_id: None,
            job_id: None,
            verification_user_task_id: None,
            item: Value::Null,
            input_variables: json!({}),
            accepted_source_event_id: None,
            approval_event_id: None,
            aggregate_item: None,
            state_patch: None,
            state_after: None,
            accepted_origin: None,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
        })
        .collect();
    plan
}

#[test]
fn a_transition_cannot_reach_more_repetition_groups_or_occurrences_than_the_limit() {
    use super::simulation::{check_simulation_capacity, SimulationUsage};

    check_simulation_capacity(
        &plan_with_repetition(MAX_SIMULATION_REPETITION_GROUPS, MAX_SIMULATION_OCCURRENCES),
        &SimulationUsage::default(),
    )
    .expect("a run may reach exactly the limits");
    for (groups, occurrences) in [
        (MAX_SIMULATION_REPETITION_GROUPS + 1, 0),
        (1, MAX_SIMULATION_OCCURRENCES + 1),
    ] {
        let error = check_simulation_capacity(
            &plan_with_repetition(groups, occurrences),
            &SimulationUsage::default(),
        )
        .expect_err("past a limit");
        assert_eq!(refusal_code(&error), SIMULATION_REPETITION_LIMIT);
    }

    // Rows an earlier step wrote count once: rewriting them is not new work.
    let rewritten =
        plan_with_repetition(MAX_SIMULATION_REPETITION_GROUPS, MAX_SIMULATION_OCCURRENCES);
    let usage = SimulationUsage {
        group_ids: rewritten
            .repetition_groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect(),
        occurrence_ids: rewritten
            .repetition_occurrences
            .iter()
            .map(|occurrence| occurrence.occurrence_id.clone())
            .collect(),
        ..SimulationUsage::default()
    };
    check_simulation_capacity(&rewritten, &usage).expect("a rewrite of persisted rows fits");
    let error = check_simulation_capacity(
        &plan_with_repetition(MAX_SIMULATION_REPETITION_GROUPS + 1, 0),
        &usage,
    )
    .expect_err("one more new group than the persisted ones allow");
    assert_eq!(refusal_code(&error), SIMULATION_REPETITION_LIMIT);
}

#[test]
fn a_plan_with_a_repetition_row_the_allocator_never_issued_is_refused() {
    use super::simulation::{validate_plan_ids, SimulationIdSource};
    use std::collections::HashSet;

    let mut ids = SimulationIdSource::new(&"b".repeat(64)).expect("allocator");
    let mut plan = plan_with_repetition(1, 1);
    // The group and the occurrence were minted by an allocator of the same run.
    plan.repetition_groups[0].group_id =
        super::runtime::RuntimeIdSource::next_id(&mut ids, "runtime");
    plan.repetition_occurrences[0].occurrence_id =
        super::runtime::RuntimeIdSource::next_id(&mut ids, "runtime");
    plan.repetition_occurrences[0].group_id = plan.repetition_groups[0].group_id.clone();
    validate_plan_ids(&plan, &ids, &HashSet::new()).expect("issued repetition rows");

    let known = HashSet::from([
        plan.repetition_groups[0].group_id.clone(),
        plan.repetition_occurrences[0].occurrence_id.clone(),
    ]);
    let fresh = SimulationIdSource::new(&"b".repeat(64)).expect("a later request's allocator");
    validate_plan_ids(&plan, &fresh, &known).expect("rows an earlier request persisted");

    let mut forged_group = plan.clone();
    forged_group.repetition_groups[0].group_id = uuid::Uuid::new_v4().to_string();
    let error = validate_plan_ids(&forged_group, &ids, &HashSet::new()).expect_err("forged group");
    assert!(
        format!("{error:#}").contains("repetition group identifier not issued"),
        "{error:#}"
    );
    let mut forged_occurrence = plan;
    forged_occurrence.repetition_occurrences[0].occurrence_id = uuid::Uuid::new_v4().to_string();
    let error = validate_plan_ids(&forged_occurrence, &ids, &HashSet::new())
        .expect_err("forged occurrence");
    assert!(
        format!("{error:#}").contains("repetition occurrence identifier not issued"),
        "{error:#}"
    );
}

#[test]
fn a_repetition_run_continues_after_the_store_is_reopened_between_steps() {
    // Every request of a live run reopens the store with a fresh allocator, so the
    // rows a plan rewrites are known only through the persisted run.
    let mut store = started(repeated_task_model(
        Some("user-simulation"),
        multi_instance(ProcessMultiInstanceMode::Parallel, 2),
    ));
    let mut view = advance_until(&mut store, "two open tasks", |view| {
        open_work(view).len() == 2
    });
    for ordinal in [1_u32, 0] {
        let simulation_id = store.simulation_id().to_owned();
        let actor = super::repository::ProcessActor {
            org_id: view.source.org_id.clone(),
            user_id: view.source.actor_user_id.clone(),
        };
        store = super::simulation_tests::reopen_store(store.into_database(), &simulation_id, actor)
            .expect("reopen the private run");
        let task = occurrence_task(&view, ordinal);
        view = complete(&mut store, &task, json!(ordinal));
    }
    assert_eq!(
        view.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(view.repetition_groups[0].completed, 2);
}
