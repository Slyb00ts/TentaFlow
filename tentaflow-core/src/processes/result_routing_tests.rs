// ===== File: result_routing_tests.rs — the designer's routing fixture driven through the real engine =====
//
// tests/fixtures/process_result_routing.json states, case by case, where an
// outcome of a Service task goes. The designer's table reads the same file
// (www/js/modules/flows-builder/bpmn.test.js), so a rule that changes in the
// engine fails here and one that changes in the table fails there.
//
// Every case runs as an addon tool task: routing is decided from the observed
// result (plan_job_result) whatever the source, and the scripted tool host is
// the one source that can produce every stimulus, including a denied permission
// and a timeout. Rows that depend on the source itself are listed in the JS
// test; the engine's own tests cover the Flow and Block sources.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessErrorDeclaration, ProcessEscalationDeclaration,
    ProcessInstanceStatus, ProcessModel, ProcessMultiInstanceInput, ProcessMultiInstanceMode,
    ProcessNode, ProcessNodeKind, ProcessRepeatSpec, ProcessUserTaskKind, ProcessUserTaskStatus,
};

use super::repository;
use super::runtime::{self, test_support::*};
use super::tool_host::ToolAccessError;
use super::tool_task_tests::{fixture_with, now_ms, run, tool_model};
use super::tool_host::test_support::ScriptedToolHost;

const FIXTURE: &str = include_str!("../../tests/fixtures/process_result_routing.json");

fn cases() -> Vec<Value> {
    let document: Value = serde_json::from_str(FIXTURE).expect("the fixture is valid JSON");
    document["cases"].as_array().expect("the fixture lists cases").clone()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

/// What the engine did, read back from durable state.
#[derive(Debug, Default)]
struct Facts {
    status: Option<ProcessInstanceStatus>,
    incidents: Vec<String>,
    caught: Vec<String>,
    propagated: bool,
    verification_open: bool,
    service_results: usize,
    timeout_reason: Option<String>,
    /// `kind@node` of every event, so a drifting case reads as a story.
    trail: Vec<String>,
    jobs: Vec<String>,
    invocations: Vec<String>,
}

impl Facts {
    /// Everything the engine did, for the message of a case that drifted.
    fn story(&self) -> String {
        format!(
            "status {:?}, incidents {:?}, caught {:?}, climbed {}, verification open {}, events [{}], jobs [{}], invocations {:?}",
            self.status, self.incidents, self.caught, self.propagated, self.verification_open,
            self.trail.join(", "), self.jobs.join(", "), self.invocations,
        )
    }
}

fn facts(fixture: &Fixture, instance_id: &str) -> Facts {
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    let history = repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
        .unwrap()
        .0;
    Facts {
        status: Some(snapshot.instance.status.clone()),
        incidents: snapshot.incidents.iter().map(|incident| incident.code.clone()).collect(),
        caught: history
            .iter()
            .filter(|event| matches!(event.kind.as_str(), "business_error_caught" | "escalation_caught"))
            .filter_map(|event| event.node_id.clone())
            .collect(),
        propagated: history.iter().any(|event| event.kind == "scope_error_propagated"),
        verification_open: snapshot.user_tasks.iter().any(|task| {
            task.kind == ProcessUserTaskKind::Verification
                && task.status == ProcessUserTaskStatus::Open
        }),
        service_results: history.iter().filter(|event| event.kind == "service_result").count(),
        timeout_reason: history
            .iter()
            .filter(|event| event.kind == "incident")
            .find_map(|event| event.data["reason"].as_str().map(str::to_owned)),
        trail: history
            .iter()
            .map(|event| format!("{}@{}", event.kind, event.node_id.as_deref().unwrap_or("-")))
            .collect(),
        jobs: snapshot
            .jobs
            .iter()
            .map(|job| format!("{}:{}:{:?}", job.node_id, job.status, job.result_origin))
            .collect(),
        invocations: {
            let connection = fixture.db.read().unwrap();
            let mut statement = connection
                .prepare("SELECT phase FROM bpmn_service_invocations WHERE instance_id=?1")
                .unwrap();
            statement
                .query_map([instance_id], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|phase| phase.unwrap())
                .collect()
        },
    }
}

fn boundary(id: &str, kind: ProcessNodeKind) -> ProcessNode {
    ProcessNode { id: id.into(), name: format!("Handler {id}"), kind, repeat: None, activity_io: None }
}

/// The case's task with its boundary events, each leading straight to the end.
fn model_of(case: &Value) -> ProcessModel {
    let task = &case["task"];
    let timeout = task["timeoutSeconds"].as_u64().unwrap_or(60) as u32;
    let result_rule = match case["stimulus"]["type"].as_str().unwrap() {
        "bad_result_rule" => Some("1"),
        _ if task["resultExpression"].as_bool().unwrap_or(false) => Some("outputs.result"),
        _ => None,
    };
    let mut model = tool_model(timeout, result_rule);
    let repeated = task["repeat"].as_bool().unwrap_or(false);
    if repeated {
        model.variables.insert("items".into(), json!([{"case": "A"}]));
        model.variables.insert("results".into(), json!([]));
    }
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    let ProcessNodeKind::ServiceTask { verification, output_mapping, .. } = &mut service.kind else {
        panic!("the tool fixture is a Service task");
    };
    // The stimulus is not shaped like the default tool output, so nothing is mapped.
    output_mapping.clear();
    *verification = match task["verification"].as_str().unwrap() {
        "Human" => ActivityVerification::Human,
        _ => ActivityVerification::Condition {
            expression: if task["conditionAccepts"].as_bool().unwrap_or(true) { "true" } else { "false" }.into(),
        },
    };
    if repeated {
        service.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::CollectionExpression { expression: "vars.items".into() },
            output_collection_variable: "results".into(),
        });
    }
    let nested = case["nested"].as_bool().unwrap_or(false);
    let end = if nested { "RootEnd_Scope" } else { "End_1" };
    if nested {
        model = embedded_model(model, "Scope");
    }
    let wire = |model: &mut ProcessModel, node: ProcessNode| {
        let id = node.id.clone();
        model.nodes.push(node);
        model.sequence_flows.push(edge(&format!("Path_{id}"), &id, end));
    };
    for handler in case["errors"].as_array().into_iter().flatten() {
        let id = handler["id"].as_str().unwrap();
        let code = text(handler, "code");
        if let Some(code) = &code {
            model.errors.push(ProcessErrorDeclaration {
                error_id: format!("Decl_{id}"), name: id.into(), error_code: code.clone(),
            });
        }
        wire(&mut model, boundary(id, ProcessNodeKind::BoundaryError {
            attached_to_id: "Service".into(),
            error_ref: code.map(|_| format!("Decl_{id}")),
            output_mapping: BTreeMap::new(),
        }));
    }
    for handler in case["escalations"].as_array().into_iter().flatten() {
        let id = handler["id"].as_str().unwrap();
        let code = text(handler, "code");
        if let Some(code) = &code {
            model.escalations.push(ProcessEscalationDeclaration {
                escalation_id: format!("Decl_{id}"), name: id.into(), escalation_code: code.clone(),
            });
        }
        // An escalation path may not run straight into the end: it waits for a person.
        let work = format!("Work_{id}");
        wire(&mut model, boundary(&work, ProcessNodeKind::UserTask {
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        }));
        model.sequence_flows.push(edge(&format!("Escalate_{id}"), id, &work));
        model.nodes.push(boundary(id, ProcessNodeKind::BoundaryEscalation {
            attached_to_id: "Service".into(),
            escalation_ref: code.map(|_| format!("Decl_{id}")),
            cancel_activity: handler["interrupting"].as_bool().unwrap_or(true),
            output_mapping: BTreeMap::new(),
        }));
    }
    if nested && case["errors"].as_array().is_none_or(|errors| errors.is_empty()) {
        wire(&mut model, boundary("ScopeCatch", ProcessNodeKind::BoundaryError {
            attached_to_id: "Scope".into(),
            error_ref: None,
            output_mapping: BTreeMap::new(),
        }));
    }
    for timer in case["timers"].as_array().into_iter().flatten() {
        let id = timer["id"].as_str().unwrap();
        model.timer_timezone = Some("UTC".into());
        wire(&mut model, boundary(id, ProcessNodeKind::BoundaryTimer {
            attached_to_id: "Service".into(),
            cancel_activity: timer["interrupting"].as_bool().unwrap_or(true),
            timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration {
                seconds: timer["seconds"].as_u64().unwrap() as u32,
            },
        }));
    }
    model
}

/// The tool's answer for a stimulus; `None` makes the tool fail.
fn tool_reply(stimulus: &Value) -> Option<Value> {
    let result = |outcome: &str| {
        json!({"outcome": outcome, "code": stimulus["code"], "summary": "scripted outcome",
            "outputs": {}, "evidence": []})
    };
    match stimulus["type"].as_str().unwrap() {
        "error" => Some(result("Error")),
        "needs_human" => Some(result("NeedsHuman")),
        "cancelled" => Some(result("Cancelled")),
        "tool_error" => None,
        _ => Some(json!({"hits": 1})),
    }
}

async fn reject_verification(fixture: &Fixture, instance_id: &str) {
    let current = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    let task = current
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Verification)
        .expect("a verification task to reject");
    let at = now_ms();
    let command = stamp("reject the verification in a routing case");
    let plan = runtime::plan_user_completion(
        &current,
        &task.user_task_id,
        &json!("declined"),
        Some(false),
        at,
        human_input(&current, &task.user_task_id, &command),
        None,
    )
    .unwrap();
    repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        instance_id,
        &task.user_task_id,
        current.instance.revision,
        &json!("declined"),
        Some(false),
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
}

/// Fires the pending boundary timer the way the scheduler would, once its
/// deadline has passed, and returns the status the instance ends in.
fn fire_timer(fixture: &Fixture, instance_id: &str, node_id: &str) -> ProcessInstanceStatus {
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    let timer = snapshot
        .timers
        .iter()
        .find(|timer| {
            timer.node_id == node_id
                && timer.status == tentaflow_protocol::processes::ProcessTimerStatus::Pending
        })
        .expect("the boundary timer is armed");
    let due = timer.due_at_ms.unwrap() + 1;
    let candidate = repository::due_timers(&fixture.db, due, 32)
        .unwrap()
        .into_iter()
        .find(|row| row.timer_id == timer.timer_id)
        .expect("the timer is due");
    let timer_snapshot = repository::timer_snapshot(&fixture.db, &candidate).unwrap();
    let plan = super::timers::plan_timer_fire(&timer_snapshot, due, None, None).unwrap();
    assert_eq!(
        plan.closed_service_dispatches.len(),
        1,
        "the timer closes the dispatch that is still uncertain"
    );
    assert_eq!(plan.closed_service_dispatches[0].source.phase, "uncertain");
    repository::fire_timer(
        &fixture.db,
        &candidate,
        &fixture.owner,
        Some(snapshot.instance.revision),
        repository::ProcessPlanInput::Supplied(&plan),
        due,
    )
    .unwrap()
    .unwrap()
    .instance
    .status
}

async fn drive(case: &Value) -> (Fixture, String) {
    let stimulus = &case["stimulus"];
    let host = ScriptedToolHost::new(tool_reply(stimulus).unwrap_or(json!(null)));
    if tool_reply(stimulus).is_none() {
        *host.reply.lock().unwrap() = Err("the addon tool failed".into());
    }
    if stimulus["type"] == "timeout" {
        *host.delay.lock().unwrap() = std::time::Duration::from_millis(2500);
    }
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &model_of(case));
    if stimulus["type"] == "denied" {
        *host.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
    }
    run(&fixture, "routing-case-worker").await;
    (fixture, started.instance_id)
}

fn check(case: &Value, fixture: &Fixture, instance_id: &str, facts: &Facts) {
    let name = case["name"].as_str().unwrap();
    let engine = &case["engine"];
    let wanted = engine["fact"].as_str().unwrap();
    let caught_only = |node: &str| facts.caught == [node.to_owned()];
    let none_of_the_incidents = facts.incidents.is_empty();
    match wanted {
        "verification" => assert!(
            facts.verification_open && none_of_the_incidents && facts.caught.is_empty(),
            "{name}: a verification task and nothing else, got {}", facts.story()
        ),
        "advanced" => assert!(
            facts.status == Some(ProcessInstanceStatus::Completed) && none_of_the_incidents,
            "{name}: the process runs on, got {}", facts.story()
        ),
        "incident" => {
            let code = engine["code"].as_str().unwrap();
            assert!(
                facts.incidents.iter().any(|incident| incident == code) && facts.caught.is_empty(),
                "{name}: incident {code} that no boundary event caught, got {}", facts.story()
            );
        }
        "boundary" => assert!(
            caught_only(engine["node"].as_str().unwrap())
                && none_of_the_incidents
                && !facts.verification_open,
            "{name}: caught by the boundary event alone, got {}", facts.story()
        ),
        "boundary_and_verification" => assert!(
            caught_only(engine["node"].as_str().unwrap())
                && facts.verification_open
                && none_of_the_incidents,
            "{name}: the boundary event runs and the verification task stays, got {}", facts.story()
        ),
        "propagated" => assert!(
            facts.propagated && caught_only(engine["node"].as_str().unwrap()) && none_of_the_incidents,
            "{name}: climbed to the enclosing boundary event, got {}", facts.story()
        ),
        "uncertain_incident" => {
            assert!(
                facts.incidents.iter().any(|incident| incident == "EXTERNAL_OUTCOME_UNCERTAIN"),
                "{name}: the uncertain-outcome incident, got {}", facts.story()
            );
            assert_eq!(
                facts.timeout_reason.as_deref(),
                engine["reason"].as_str(),
                "{name}: the reason the incident carries"
            );
            assert_eq!(facts.service_results, 0, "{name}: no result was accepted");
        }
        "timer_resolves" => {
            assert!(
                facts.incidents.iter().any(|incident| incident == "EXTERNAL_OUTCOME_UNCERTAIN"),
                "{name}: the timeout raised the uncertain incident first, got {}", facts.story()
            );
            let timer = case["timers"][0]["id"].as_str().unwrap();
            assert_eq!(
                fire_timer(fixture, instance_id, timer),
                ProcessInstanceStatus::Completed,
                "{name}: the interrupting timer ends the task"
            );
            let history =
                repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200).unwrap().0;
            assert!(
                history.iter().any(|event| event.kind == "incident_resolved"
                    && event.data["resolution_reason"] == "activity_cancelled_after_dispatch"),
                "{name}: the uncertain incident is resolved"
            );
        }
        other => panic!("{name}: unknown engine fact {other}"),
    }
}

async fn run_case(case: Value) {
    let name = case["name"].as_str().unwrap().to_owned();
    if case["engine"].is_null() {
        assert!(
            case["engineReason"].as_str().is_some_and(|reason| !reason.is_empty()),
            "{name}: a row without an engine probe says why"
        );
        return;
    }
    let (fixture, instance_id) = drive(&case).await;
    if case["stimulus"]["type"] == "rejected" && case["task"]["verification"] == "Human" {
        reject_verification(&fixture, &instance_id).await;
    }
    let observed = facts(&fixture, &instance_id);
    assert!(
        observed.invocations != ["observed"],
        "{name}: the result was observed but never applied to the process, got {}", observed.story()
    );
    check(&case, &fixture, &instance_id, &observed);
}

/// Every case runs, and every failure is reported at once, so one run shows the
/// whole drift between the fixture and the engine.
#[tokio::test]
async fn every_routing_case_goes_where_the_designer_says() {
    let cases = cases();
    assert!(cases.len() >= 25, "the fixture is the contract; do not thin it out");
    let mut failures = Vec::new();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for case in &cases {
                let name = case["name"].as_str().unwrap().to_owned();
                if let Err(error) = tokio::task::spawn_local(run_case(case.clone())).await {
                    let reason = error.into_panic();
                    let text = reason
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| reason.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                        .unwrap_or_else(|| "panicked".into());
                    failures.push(format!("{name}: {text}"));
                }
            }
        })
        .await;
    assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

/// A caller's error boundary is a property of the Call activity, which a
/// single-process case cannot build; the engine test below proves it, and the
/// fixture names it so the claim "unless a Call activity catches it" cannot
/// outlive its proof.
#[test]
fn the_caller_catch_named_by_the_fixture_exists() {
    let calls = include_str!("call_tests.rs");
    let mut named = 0;
    for case in cases() {
        let Some(test) = case["callerCatch"].as_str() else { continue };
        named += 1;
        let (module, function) = test.split_once("::").unwrap();
        assert_eq!(module, "call_tests");
        assert!(
            calls.contains(&format!("async fn {function}()")) || calls.contains(&format!("fn {function}()")),
            "the engine test {test} named by the fixture is gone"
        );
    }
    assert!(named >= 1);
}
