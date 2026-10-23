// ===== File: agent_sink_tests.rs — agent steps: publication, binding and the core.task_complete sink =====

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessModel, ProcessNodeKind,
    ServiceImplementation,
};
use tokio_util::sync::CancellationToken;

use super::agent_sink::{
    agent_run_id, binding_from_meta, complete_task, ProcessActivityBinding,
    PROCESS_ACTIVITY_META_KEY,
};
use super::repository::{self, ClaimedProcessJob};
use super::runtime::test_support::*;
use super::tool_task_tests::{invocation, now_ms};
use crate::agents::run_manager::AgentFlowOutcome;
use crate::agents::{AgentPrincipal, AgentRunManager, BackgroundFlowRunner};
use crate::db::models::AgentParams;
use crate::flow_engine::dispatcher::FlowRef;
use crate::flow_engine::dispatchers::ProgressSink;
use crate::flow_engine::envelope::FlowEnvelope;
use crate::flow_engine::progress_broker::ProgressBroker;

pub(crate) const AGENT_ID: &str = "11111111-2222-4333-8444-555555555555";

pub(crate) fn seed_agent(
    fixture: &Fixture,
    id: &str,
    tools_json: &str,
    max_iterations: i64,
    enabled: bool,
) {
    crate::db::repository::upsert_agent(
        &fixture.db,
        &AgentParams {
            runtime_json: crate::agents::LLM_RUNTIME_JSON,
            id,
            name: id,
            display_name: None,
            description: "process step agent",
            system_prompt: Some("Work carefully."),
            model: None,
            tools_json,
            skills_json: "{}",
            params_json: "{}",
            max_iterations,
            timeout_secs: 600,
            max_subagents: 0,
            max_spawn_depth: 1,
            flow_id: None,
            routable: false,
            is_enabled: enabled,
            on_child_complete: "notify",
            allowed_agents_json: None,
            actor_user_id: None,
        },
    )
    .expect("seed agent");
}

pub(crate) fn agent_implementation(
    agent_id: &str,
    allowed_tools: &[&str],
    output_schema: Option<Value>,
) -> ServiceImplementation {
    ServiceImplementation::Agent {
        agent_id: agent_id.into(),
        goal_expression: "\"Summarise the case\"".into(),
        allowed_tools: allowed_tools
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect(),
        max_iterations: 5,
        output_schema,
    }
}

pub(crate) fn agent_model(implementation: ServiceImplementation) -> ProcessModel {
    let mut model = service_model(
        "",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    node.kind = ProcessNodeKind::ServiceTask {
        flow_id: String::new(),
        input_mapping: BTreeMap::new(),
        output_mapping: BTreeMap::new(),
        verification: ActivityVerification::Condition {
            expression: "true".into(),
        },
        timeout_seconds: 120,
        result_expression: None,
        implementation,
    };
    model
}

/// Writes an `agent_runs` row the way the run manager does and moves it to
/// `status`, so the sink has a run row to check.
pub(crate) fn insert_run(
    fixture: &Fixture,
    run_id: &str,
    agent_id: &str,
    parent_run_id: Option<&str>,
    correlation_id: Option<&str>,
    status: &str,
) {
    let principal = AgentPrincipal::user(&fixture.owner.user_id);
    crate::db::repository::create_agent_run(
        &fixture.db,
        &crate::db::models::NewAgentRun {
            id: run_id,
            agent_id,
            parent_run_id,
            flow_execution_id: None,
            user_id: principal.user_id(),
            org_id: principal.org_id.as_deref(),
            prompt: "Summarise the case",
            origin: principal.origin.as_str(),
            actor_kind: principal.actor.kind().as_str(),
            actor_id: principal.actor.id(),
            actor_user_id: principal.actor.user_id(),
            correlation_id,
        },
    )
    .expect("insert the run row");
    crate::db::repository::update_agent_run_status(
        &fixture.db,
        run_id,
        &crate::db::models::AgentRunStatusUpdate {
            status,
            ..Default::default()
        },
    )
    .expect("set the run status");
}

/// A started process whose agent step is claimed and past the dispatch
/// boundary, with the running top-level run the executor would have spawned for
/// it: the only state in which the sink may record a report.
pub(crate) fn running_step(
    fixture: &Fixture,
    implementation: ServiceImplementation,
) -> (ClaimedProcessJob, ProcessActivityBinding) {
    let ServiceImplementation::Agent { agent_id, .. } = &implementation else {
        panic!("an agent step is needed");
    };
    let agent_id = agent_id.clone();
    let (claimed, binding) = step_past_the_boundary(fixture, implementation);
    insert_run(
        fixture,
        &binding.agent_run_id(),
        &agent_id,
        None,
        Some(&binding.invocation_id),
        "running",
    );
    (claimed, binding)
}

/// The same step before any run exists for it: the state in which the executor
/// spawns the run.
pub(crate) fn step_past_the_boundary(
    fixture: &Fixture,
    implementation: ServiceImplementation,
) -> (ClaimedProcessJob, ProcessActivityBinding) {
    start_model(fixture, &agent_model(implementation.clone()));
    let claimed = repository::claim_job(&fixture.db, "agent-worker", now_ms())
        .unwrap()
        .expect("claim the queued agent step");
    assert!(repository::commit_job_dispatch_boundary(
        &fixture.db,
        &claimed,
        "agent-worker",
        now_ms()
    )
    .unwrap());
    let ServiceImplementation::Agent {
        allowed_tools,
        output_schema,
        ..
    } = implementation
    else {
        panic!("an agent step is needed");
    };
    let binding = ProcessActivityBinding {
        invocation_id: claimed.invocation_id.clone(),
        instance_id: claimed.job.instance_id.clone(),
        node_id: claimed.job.node_id.clone(),
        attempt: claimed.job.attempt,
        fence: claimed.job.fence,
        allowed_tools,
        output_schema,
    };
    (claimed, binding)
}

fn completions(fixture: &Fixture) -> Vec<(String, String, ActivityResult)> {
    let conn = fixture.db.read().unwrap();
    let mut statement = conn
        .prepare("SELECT invocation_id,run_id,result_json FROM bpmn_agent_completions ORDER BY created_at_ms")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                serde_json::from_str::<ActivityResult>(&row.get::<_, String>(2)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn report(
    fixture: &Fixture,
    binding: &ProcessActivityBinding,
    arguments: Value,
) -> Result<Value, String> {
    complete_task(
        &fixture.db,
        binding,
        &binding.agent_run_id(),
        &arguments,
        now_ms(),
    )
}

fn finished(summary: &str, outputs: Value) -> Value {
    json!({"outcome": "completed", "summary": summary, "outputs": outputs})
}

#[test]
fn a_report_is_stored_once_as_a_contract_shaped_result() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, r#"["core.skill_view"]"#, 10, true);
    let (claimed, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));

    let stored = report(
        &fixture,
        &binding,
        json!({"outcome": "completed", "code": "DONE.1", "summary": "all good",
               "outputs": {"verdict": "ok"}, "evidence": ["doc:1"]}),
    )
    .expect("the first report is recorded");
    assert_eq!(stored, json!({"recorded": true}));

    let rows = completions(&fixture);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, claimed.invocation_id);
    assert_eq!(rows[0].1, agent_run_id(&claimed.invocation_id));
    assert_eq!(
        rows[0].2,
        ActivityResult {
            outcome: ActivityOutcome::Completed,
            code: Some("DONE.1".into()),
            summary: "all good".into(),
            outputs: json!({"verdict": "ok"}),
            evidence: vec!["doc:1".into()],
        }
    );
}

#[test]
fn error_and_needs_human_reports_keep_their_outcome() {
    for (word, outcome) in [
        ("error", ActivityOutcome::Error),
        ("needs_human", ActivityOutcome::NeedsHuman),
    ] {
        let fixture = Fixture::new();
        seed_agent(&fixture, AGENT_ID, "[]", 10, true);
        let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
        report(
            &fixture,
            &binding,
            json!({"outcome": word, "code": "NO_DATA", "summary": "cannot decide"}),
        )
        .unwrap();
        let rows = completions(&fixture);
        assert_eq!(rows[0].2.outcome, outcome, "{word}");
        assert_eq!(rows[0].2.outputs, json!({}));
    }
}

#[test]
fn the_second_report_is_refused_and_the_first_stays() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    report(&fixture, &binding, finished("first", json!({"n": 1}))).unwrap();

    let refusal = report(&fixture, &binding, finished("second", json!({"n": 2}))).unwrap_err();
    assert!(refusal.contains("already completed"), "{refusal}");
    let rows = completions(&fixture);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2.summary, "first");
}

#[test]
fn the_table_itself_refuses_a_second_row_and_any_edit() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (claimed, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    report(&fixture, &binding, finished("first", json!({}))).unwrap();

    let conn = fixture.db.write().unwrap();
    assert!(conn
        .execute(
            "INSERT INTO bpmn_agent_completions(invocation_id,run_id,result_json,created_at_ms) \
             VALUES(?1,'r','{}',1)",
            [&claimed.invocation_id],
        )
        .is_err());
    let edit = conn
        .execute("UPDATE bpmn_agent_completions SET result_json='{}'", [])
        .unwrap_err();
    assert!(
        edit.to_string().contains("agent completion is immutable"),
        "{edit}"
    );
}

#[test]
fn a_report_for_another_invocation_is_not_recorded() {
    // The model cannot choose the target; the binding is the whole identity. A
    // binding that names an invocation of another step finds no running step of
    // that identity, and the run id check refuses a run that was not spawned
    // for the invocation at all.
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));

    let foreign = ProcessActivityBinding {
        invocation_id: "another-invocation".into(),
        ..binding.clone()
    };
    let refusal = complete_task(
        &fixture.db,
        &foreign,
        &binding.agent_run_id(),
        &finished("x", json!({})),
        now_ms(),
    )
    .unwrap_err();
    assert!(
        refusal.contains("was not started for the step"),
        "{refusal}"
    );
    let refusal = complete_task(
        &fixture.db,
        &foreign,
        &foreign.agent_run_id(),
        &finished("x", json!({})),
        now_ms(),
    )
    .unwrap_err();
    assert!(refusal.contains("no longer running"), "{refusal}");
    assert!(completions(&fixture).is_empty());
}

#[test]
fn arguments_cannot_name_a_target() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    for key in ["invocation_id", "instance_id", "node_id", "run_id", "fence"] {
        let mut arguments = finished("x", json!({}));
        arguments[key] = json!("someone-else");
        let refusal = report(&fixture, &binding, arguments).unwrap_err();
        assert!(refusal.contains("unknown report field"), "{key}: {refusal}");
    }
    assert!(completions(&fixture).is_empty());
}

#[test]
fn a_stale_attempt_or_fence_is_not_live() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    for stale in [
        ProcessActivityBinding {
            attempt: binding.attempt + 1,
            ..binding.clone()
        },
        ProcessActivityBinding {
            fence: binding.fence + 1,
            ..binding.clone()
        },
        ProcessActivityBinding {
            node_id: "Other".into(),
            ..binding.clone()
        },
        ProcessActivityBinding {
            instance_id: "other-instance".into(),
            ..binding.clone()
        },
    ] {
        let refusal = report(&fixture, &stale, finished("x", json!({}))).unwrap_err();
        assert!(refusal.contains("no longer running"), "{refusal}");
    }
    assert!(completions(&fixture).is_empty());
}

#[test]
fn nothing_is_recorded_before_the_dispatch_boundary() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let started = start_model(
        &fixture,
        &agent_model(agent_implementation(AGENT_ID, &[], None)),
    );
    let claimed = repository::claim_job(&fixture.db, "agent-worker", now_ms())
        .unwrap()
        .unwrap();
    let binding = ProcessActivityBinding {
        invocation_id: claimed.invocation_id.clone(),
        instance_id: started.instance_id,
        node_id: claimed.job.node_id.clone(),
        attempt: claimed.job.attempt,
        fence: claimed.job.fence,
        allowed_tools: Vec::new(),
        output_schema: None,
    };
    let refusal = report(&fixture, &binding, finished("early", json!({}))).unwrap_err();
    assert!(refusal.contains("no longer running"), "{refusal}");
    assert!(completions(&fixture).is_empty());
}

#[test]
fn a_report_after_the_instance_was_cancelled_is_not_recorded() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (claimed, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE bpmn_instances SET status='cancelled' WHERE instance_id=?1",
            [&claimed.job.instance_id],
        )
        .unwrap();
    let refusal = report(&fixture, &binding, finished("late", json!({}))).unwrap_err();
    assert!(refusal.contains("no longer running"), "{refusal}");
    assert!(completions(&fixture).is_empty());
}

#[test]
fn a_report_after_the_result_was_observed_is_not_recorded() {
    // The attempt and fence still match once the invocation has moved on, so
    // only its phase says the step no longer waits for a report.
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (claimed, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE bpmn_service_invocations SET phase='observed',phase_revision=phase_revision+1,\
             updated_at_ms=updated_at_ms+1,observed_result_json='{}',observed_result_origin='platform',\
             observed_result_sha256=?2,reserved_result_event_id='event-1',observed_at_ms=1 \
             WHERE invocation_id=?1",
            rusqlite::params![claimed.invocation_id, "b".repeat(64)],
        )
        .unwrap();
    let refusal = report(&fixture, &binding, finished("late", json!({}))).unwrap_err();
    assert!(refusal.contains("no longer running"), "{refusal}");
    assert!(completions(&fixture).is_empty());
}

#[test]
fn an_invalid_report_is_repairable_and_leaves_no_row() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    let oversized = "x".repeat(33 * 1024);
    for (arguments, expected) in [
        (json!("done"), "must be a JSON object"),
        (json!({"summary": "s"}), "'outcome' must be one of"),
        (
            json!({"outcome": "great", "summary": "s"}),
            "'outcome' must be one of",
        ),
        (json!({"outcome": "completed"}), "'summary' is required"),
        (
            json!({"outcome": "completed", "summary": "s", "code": "bad code!"}),
            "error code is invalid",
        ),
        (
            json!({"outcome": "completed", "summary": oversized}),
            "exceeds its budget",
        ),
        (
            json!({"outcome": "completed", "summary": "s", "evidence": "doc"}),
            "invalid report",
        ),
    ] {
        let refusal = report(&fixture, &binding, arguments).unwrap_err();
        assert!(refusal.contains(expected), "{expected}: {refusal}");
    }
    assert!(completions(&fixture).is_empty());
    // The step is still open, so the repaired report is accepted.
    report(&fixture, &binding, finished("fixed", json!({}))).unwrap();
    assert_eq!(completions(&fixture).len(), 1);
}

#[test]
fn completed_outputs_must_match_the_output_schema_but_failures_need_not() {
    let schema = json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string", "enum": ["ok", "bad"]}},
        "additionalProperties": false
    });
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], Some(schema)));

    for outputs in [
        json!({}),
        json!({"verdict": "maybe"}),
        json!({"verdict": "ok", "extra": 1}),
    ] {
        let refusal = report(&fixture, &binding, finished("s", outputs)).unwrap_err();
        assert!(
            refusal.contains("do not match the output schema"),
            "{refusal}"
        );
    }
    assert!(completions(&fixture).is_empty());
    // A failure report carries whatever explains the failure.
    report(
        &fixture,
        &binding,
        json!({"outcome": "error", "code": "BROKEN", "summary": "no", "outputs": {"why": 1}}),
    )
    .unwrap();
    assert_eq!(completions(&fixture)[0].2.outcome, ActivityOutcome::Error);
}

#[test]
fn a_valid_output_passes_the_schema() {
    let schema =
        json!({"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}});
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], Some(schema)));
    report(&fixture, &binding, finished("s", json!({"n": 3}))).unwrap();
    assert_eq!(completions(&fixture)[0].2.outputs, json!({"n": 3}));
}

#[test]
fn the_binding_round_trips_through_run_meta_and_a_forged_one_is_refused() {
    let binding = ProcessActivityBinding {
        invocation_id: "inv".into(),
        instance_id: "ins".into(),
        node_id: "n".into(),
        attempt: 1,
        fence: 7,
        allowed_tools: vec!["core.skill_view".into()],
        output_schema: Some(json!({"type": "object"})),
    };
    let (key, value) = binding.meta_entry();
    assert_eq!(key, PROCESS_ACTIVITY_META_KEY);
    let mut meta = BTreeMap::new();
    meta.insert(key.to_owned(), value);
    assert_eq!(binding_from_meta(&meta), Some(binding));

    assert_eq!(binding_from_meta(&BTreeMap::new()), None);
    meta.insert(
        PROCESS_ACTIVITY_META_KEY.to_owned(),
        json!({"invocation_id": "inv"}),
    );
    assert_eq!(binding_from_meta(&meta), None);
}

#[test]
fn the_run_id_is_a_stable_function_of_the_invocation() {
    assert_eq!(agent_run_id("a"), agent_run_id("a"));
    assert_ne!(agent_run_id("a"), agent_run_id("b"));
    assert!(uuid::Uuid::parse_str(&agent_run_id("a")).is_ok());
}

/// Runs the agent the way the process executor will: through the manager, with
/// the binding handed to `spawn_with_run_id` as extra meta, and a scripted
/// runner standing in for the model that reports through the sink. The runner
/// reports under `scope`, the id the engine is executing (what the real
/// `tool_exec` reads from `ExecutionContext::request_id`), never under an id it
/// finds in meta.
struct ReportingRunner {
    db: crate::db::DbPool,
    reports: Vec<Value>,
    seen: tokio::sync::mpsc::UnboundedSender<Vec<Result<Value, String>>>,
}

#[async_trait]
impl BackgroundFlowRunner for ReportingRunner {
    async fn run_agent_flow(
        &self,
        _flow: FlowRef,
        initial: FlowEnvelope,
        _principal: AgentPrincipal,
        _deadline: Option<std::time::Instant>,
        _cancel: CancellationToken,
        _progress: Arc<dyn ProgressSink>,
        scope: String,
    ) -> anyhow::Result<AgentFlowOutcome> {
        let binding = binding_from_meta(&initial.meta).expect("the spawn carried the binding");
        let outcomes = self
            .reports
            .iter()
            .map(|arguments| complete_task(&self.db, &binding, &scope, arguments, now_ms()))
            .collect();
        self.seen.send(outcomes).unwrap();
        Ok(AgentFlowOutcome {
            text: "done".into(),
            usage: Default::default(),
            model: None,
            iterations: None,
        })
    }
}

#[tokio::test]
async fn a_run_spawned_with_the_derived_id_and_the_binding_reports_through_the_sink() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (claimed, binding) =
        step_past_the_boundary(&fixture, agent_implementation(AGENT_ID, &[], None));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = Arc::new(AgentRunManager::new(
        fixture.db.clone(),
        Arc::new(ReportingRunner {
            db: fixture.db.clone(),
            reports: vec![
                finished("first", json!({"n": 1})),
                finished("again", json!({"n": 2})),
            ],
            seen: sender,
        }),
        Arc::new(ProgressBroker::new()),
        2,
    ));
    manager.attach_self();
    let principal = AgentPrincipal::user(&fixture.owner.user_id)
        .with_correlation_id(Some(binding.invocation_id.clone()));

    let run_id = manager
        .spawn_with_run_id(
            &binding.agent_run_id(),
            AGENT_ID,
            "Summarise the case",
            None,
            &principal,
            &[],
            &[binding.meta_entry()],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(run_id, agent_run_id(&claimed.invocation_id));
    manager
        .await_run(&run_id, Duration::from_secs(10))
        .await
        .unwrap();

    let outcomes = receiver.recv().await.unwrap();
    assert_eq!(outcomes[0], Ok(json!({"recorded": true})));
    assert!(outcomes[1]
        .as_ref()
        .unwrap_err()
        .contains("already completed"));
    assert_eq!(completions(&fixture).len(), 1);
    assert_eq!(completions(&fixture)[0].1, run_id);
}

/// Spawns a run through the manager with a runner that tries to report the
/// given arguments, and returns what the sink answered.
async fn reports_from_a_spawned_run(
    fixture: &Fixture,
    spawn: impl FnOnce(
        Arc<AgentRunManager>,
        AgentPrincipal,
    ) -> futures::future::BoxFuture<'static, String>,
) -> Vec<Result<Value, String>> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = Arc::new(AgentRunManager::new(
        fixture.db.clone(),
        Arc::new(ReportingRunner {
            db: fixture.db.clone(),
            reports: vec![finished("hijack", json!({}))],
            seen: sender,
        }),
        Arc::new(ProgressBroker::new()),
        2,
    ));
    manager.attach_self();
    let run_id = spawn(
        manager.clone(),
        AgentPrincipal::user(&fixture.owner.user_id),
    )
    .await;
    manager
        .await_run(&run_id, Duration::from_secs(10))
        .await
        .unwrap();
    receiver.recv().await.unwrap()
}

#[tokio::test]
async fn a_run_with_a_forged_binding_and_an_id_of_its_own_cannot_report_for_the_step() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    // `spawn` mints its own id: a run nobody derived from the invocation, whose
    // meta nevertheless carries the genuine binding.
    let outcomes = reports_from_a_spawned_run(&fixture, |manager, principal| {
        let entry = binding.meta_entry();
        Box::pin(async move {
            manager
                .spawn(
                    AGENT_ID,
                    "hijack",
                    None,
                    &principal,
                    &[],
                    &[entry],
                    None,
                    None,
                )
                .await
                .unwrap()
        })
    })
    .await;
    assert!(outcomes[0]
        .as_ref()
        .unwrap_err()
        .contains("was not started for the step"));
    assert!(completions(&fixture).is_empty());
}

#[tokio::test]
async fn naming_the_derived_run_id_in_meta_does_not_make_a_run_the_step_run() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let (_, binding) = running_step(&fixture, agent_implementation(AGENT_ID, &[], None));
    // The forged run copies both meta entries a flow node can write, but the id
    // the engine executes it under is its own.
    let outcomes = reports_from_a_spawned_run(&fixture, |manager, principal| {
        let entries = [
            binding.meta_entry(),
            ("agent_run_id", json!(binding.agent_run_id())),
        ];
        Box::pin(async move {
            manager
                .spawn(
                    AGENT_ID,
                    "hijack",
                    None,
                    &principal,
                    &[],
                    &entries,
                    None,
                    None,
                )
                .await
                .unwrap()
        })
    })
    .await;
    assert!(outcomes[0]
        .as_ref()
        .unwrap_err()
        .contains("was not started for the step"));
    assert!(completions(&fixture).is_empty());
}

#[test]
fn only_the_running_top_level_run_of_the_pinned_agent_for_this_invocation_may_report() {
    const OTHER_AGENT: &str = "99999999-2222-4333-8444-555555555555";
    let cases: [(&str, &dyn Fn(&Fixture, &ProcessActivityBinding)); 6] = [
        ("another agent", &|fixture, binding| {
            seed_agent(fixture, OTHER_AGENT, "[]", 10, true);
            insert_run(
                fixture,
                &binding.agent_run_id(),
                OTHER_AGENT,
                None,
                Some(&binding.invocation_id),
                "running",
            );
        }),
        ("a sub-agent", &|fixture, binding| {
            insert_run(fixture, "parent-run", AGENT_ID, None, None, "running");
            insert_run(
                fixture,
                &binding.agent_run_id(),
                AGENT_ID,
                Some("parent-run"),
                Some(&binding.invocation_id),
                "running",
            );
        }),
        ("another invocation's run", &|fixture, binding| {
            insert_run(
                fixture,
                &binding.agent_run_id(),
                AGENT_ID,
                None,
                Some("another-invocation"),
                "running",
            );
        }),
        ("a run without a correlation", &|fixture, binding| {
            insert_run(
                fixture,
                &binding.agent_run_id(),
                AGENT_ID,
                None,
                None,
                "running",
            );
        }),
        ("a run that is not running", &|fixture, binding| {
            insert_run(
                fixture,
                &binding.agent_run_id(),
                AGENT_ID,
                None,
                Some(&binding.invocation_id),
                "completed",
            );
        }),
        ("no run row at all", &|_, _| {}),
    ];
    for (name, make_run) in cases {
        let fixture = Fixture::new();
        seed_agent(&fixture, AGENT_ID, "[]", 10, true);
        let (_, binding) =
            step_past_the_boundary(&fixture, agent_implementation(AGENT_ID, &[], None));
        make_run(&fixture, &binding);
        let refusal = report(&fixture, &binding, finished("x", json!({}))).unwrap_err();
        assert!(
            refusal.contains("was not started for the step"),
            "{name}: {refusal}"
        );
        assert!(completions(&fixture).is_empty(), "{name}");
    }
}

// --- Publication ---------------------------------------------------------------

fn pin_result(
    fixture: &Fixture,
    implementation: ServiceImplementation,
) -> Result<Vec<repository::PinnedServiceSnapshot>, tentaflow_protocol::ProtocolError> {
    crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &fixture.owner,
        &agent_model(implementation),
    )
}

#[test]
fn a_valid_agent_step_publishes_with_a_pin_and_records_an_agent_invocation() {
    let fixture = Fixture::new();
    seed_agent(
        &fixture,
        AGENT_ID,
        r#"["core.skill_view","memory.*"]"#,
        10,
        true,
    );
    let implementation = agent_implementation(AGENT_ID, &["core.skill_view"], None);

    let pins = pin_result(&fixture, implementation.clone()).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].info.flow_id, format!("agent:{AGENT_ID}"));
    assert_eq!(
        pins[0].implementation_kind,
        tentaflow_protocol::processes::ServiceImplementationKind::Agent
    );
    let pin: Value = serde_json::from_str(&pins[0].graph_json).unwrap();
    assert_eq!(pin["agent_id"], AGENT_ID);
    assert_eq!(pin["allowed_tools"], json!(["core.skill_view"]));
    assert_eq!(pin["digest"].as_str().unwrap().len(), 64);

    let (claimed, _) = running_step(&fixture, implementation);
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id),
        (
            "agent".into(),
            format!("agent:{AGENT_ID}"),
            "may_have_executed".into()
        )
    );
}

#[test]
fn publication_refuses_an_agent_that_cannot_run_a_process_step() {
    let fixture = Fixture::new();
    seed_agent(
        &fixture,
        AGENT_ID,
        r#"["core.skill_view","memory.*"]"#,
        4,
        true,
    );
    let refused = |implementation: ServiceImplementation| -> String {
        let error = pin_result(&fixture, implementation).unwrap_err();
        assert_eq!(
            error.code,
            tentaflow_protocol::ProtocolErrorCode::BadRequest
        );
        error.message
    };

    assert!(refused(agent_implementation("missing-agent", &[], None)).contains("does not exist"));
    assert!(
        refused(agent_implementation(AGENT_ID, &["core.fs_read"], None))
            .contains("which the agent itself does not have")
    );
    assert!(refused(agent_implementation(
        AGENT_ID,
        &["memory.memory_store", "notes.*"],
        None
    ))
    .contains("notes.*"));
    let ServiceImplementation::Agent {
        agent_id,
        goal_expression,
        output_schema,
        ..
    } = agent_implementation(AGENT_ID, &[], None)
    else {
        unreachable!()
    };
    let greedy = ServiceImplementation::Agent {
        agent_id,
        goal_expression,
        allowed_tools: Vec::new(),
        max_iterations: 5,
        output_schema,
    };
    assert!(refused(greedy).contains("only 4"));

    seed_agent(&fixture, "disabled-agent", "[]", 10, false);
    assert!(refused(agent_implementation("disabled-agent", &[], None)).contains("disabled"));

    seed_agent(&fixture, "cli-agent", "[]", 10, true);
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE agents SET runtime_json=?1 WHERE id='cli-agent'",
            [r#"{"kind":"cli","engine":"codex","account":{"mode":"user"}}"#],
        )
        .unwrap();
    assert!(refused(agent_implementation("cli-agent", &[], None)).contains("CLI engine"));
}

#[test]
fn a_wildcard_step_entry_needs_the_same_wildcard_on_the_agent() {
    let fixture = Fixture::new();
    seed_agent(
        &fixture,
        AGENT_ID,
        r#"["memory.memory_store","memory.*"]"#,
        10,
        true,
    );
    pin_result(
        &fixture,
        agent_implementation(AGENT_ID, &["memory.*"], None),
    )
    .unwrap();
    seed_agent(
        &fixture,
        "exact-agent",
        r#"["memory.memory_store"]"#,
        10,
        true,
    );
    let error = pin_result(
        &fixture,
        agent_implementation("exact-agent", &["memory.*"], None),
    )
    .unwrap_err();
    assert!(error.message.contains("memory.*"), "{}", error.message);
}

#[test]
fn publication_refuses_a_pin_that_is_not_this_steps_agent() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    seed_agent(&fixture, "other-agent", "[]", 10, true);
    let model = agent_model(agent_implementation(AGENT_ID, &[], None));
    let definition = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("create"),
        None,
        0,
        "Agent process",
        "",
        &model,
    )
    .unwrap();
    // The pin of ANOTHER agent, under this step's reference, is not accepted.
    let mut pins = pin_result(&fixture, agent_implementation(AGENT_ID, &[], None)).unwrap();
    let other = pin_result(&fixture, agent_implementation("other-agent", &[], None)).unwrap();
    pins[0].graph_json = other[0].graph_json.clone();
    pins[0].info.graph_sha256 = other[0].info.graph_sha256.clone();
    let error = repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("publish"),
        &definition.definition_id,
        definition.draft_revision,
        &pins,
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("agent"), "{error:#}");
}

#[test]
fn model_validation_bounds_an_agent_step() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let invalid = |implementation: ServiceImplementation, timeout: u32, result: Option<&str>| {
        let mut model = agent_model(implementation);
        let node = model
            .nodes
            .iter_mut()
            .find(|node| node.id == "Service")
            .unwrap();
        let ProcessNodeKind::ServiceTask {
            timeout_seconds,
            result_expression,
            ..
        } = &mut node.kind
        else {
            unreachable!()
        };
        *timeout_seconds = timeout;
        *result_expression = result.map(str::to_owned);
        format!("{:#}", super::model::validate_model(&model).unwrap_err())
    };
    let agent = |tools: &[&str], iterations: u32, goal: &str, schema: Option<Value>| {
        ServiceImplementation::Agent {
            agent_id: AGENT_ID.into(),
            goal_expression: goal.into(),
            allowed_tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            max_iterations: iterations,
            output_schema: schema,
        }
    };

    super::model::validate_model(&agent_model(agent(
        &["core.skill_view"],
        5,
        "\"goal\"",
        None,
    )))
    .unwrap();
    // An agent may think for an hour, which no other implementation may.
    let mut long = agent_model(agent(&[], 5, "\"goal\"", None));
    let ProcessNodeKind::ServiceTask {
        timeout_seconds, ..
    } = &mut long
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    *timeout_seconds = 3600;
    super::model::validate_model(&long).unwrap();
    assert!(invalid(agent(&[], 5, "\"goal\"", None), 3601, None).contains("1..=3600"));
    assert!(invalid(agent(&[], 0, "\"goal\"", None), 60, None).contains("iterations"));
    assert!(invalid(agent(&[], 101, "\"goal\"", None), 60, None).contains("iterations"));
    assert!(invalid(agent(&[], 5, "", None), 60, None).contains("goal expression"));
    assert!(invalid(agent(&["nodot"], 5, "\"g\"", None), 60, None).contains("unknown or malformed"));
    assert!(invalid(agent(&["core.bogus"], 5, "\"g\"", None), 60, None)
        .contains("unknown or malformed"));
    assert!(
        invalid(agent(&["core.task_complete"], 5, "\"g\"", None), 60, None)
            .contains("completion tool")
    );
    assert!(invalid(
        agent(&["core.skill_view", "core.skill_view"], 5, "\"g\"", None),
        60,
        None
    )
    .contains("twice"));
    assert!(
        invalid(agent(&[], 5, "\"g\"", Some(json!({"if": {}}))), 60, None)
            .contains("output schema")
    );
    assert!(invalid(agent(&[], 5, "\"g\"", Some(json!([1]))), 60, None).contains("JSON object"));
    assert!(
        invalid(agent(&[], 5, "\"g\"", None), 60, Some("outputs")).contains("completion report")
    );
    let mut named = agent_model(agent(&[], 5, "\"g\"", None));
    let ProcessNodeKind::ServiceTask { flow_id, .. } = &mut named
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    *flow_id = "some-flow".into();
    assert!(super::model::validate_model(&named)
        .unwrap_err()
        .to_string()
        .contains("must not name a flow"));
    // A flow cannot masquerade as an agent through the reserved prefix.
    let reserved =
        super::model::validate_model(&service_model("agent:someone", ActivityVerification::Human))
            .unwrap_err();
    assert!(
        reserved.to_string().contains("reserved agent: prefix"),
        "{reserved}"
    );
}

#[tokio::test]
async fn running_an_agent_step_before_its_executor_exists_fails_before_the_boundary() {
    let fixture = Fixture::new();
    seed_agent(&fixture, AGENT_ID, "[]", 10, true);
    let started = start_model(
        &fixture,
        &agent_model(agent_implementation(AGENT_ID, &[], None)),
    );
    let claimed = repository::claim_job(&fixture.db, "agent-worker", now_ms())
        .unwrap()
        .unwrap();
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "agent-worker",
        claimed.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(
        current.status,
        tentaflow_protocol::processes::ProcessInstanceStatus::Incident
    );
    assert_eq!(current.incidents[0].code, "AGENT_EXECUTOR_UNAVAILABLE");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id),
        (
            "agent".into(),
            format!("agent:{AGENT_ID}"),
            "proved_no_effect".into()
        )
    );
    assert!(completions(&fixture).is_empty());
}

#[test]
fn an_agent_step_survives_the_xml_round_trip() {
    let implementation = agent_implementation(
        AGENT_ID,
        &["core.skill_view", "memory.*"],
        Some(
            json!({"type": "object", "properties": {"b": {"type": "string"}, "a": {"type": "integer"}}}),
        ),
    );
    let model = agent_model(implementation);
    let xml = super::bpmn::export_xml(&model).unwrap();
    assert!(xml.contains(&format!(
        "implementation=\"agent\" agentId=\"{AGENT_ID}\" maxIterations=\"5\""
    )));
    let import = |xml: &str| super::bpmn::import_xml(xml).0;
    let imported = import(&xml).expect("the exported agent step imports");
    let service = |model: &ProcessModel| {
        model
            .nodes
            .iter()
            .find(|node| node.id == "Service")
            .unwrap()
            .kind
            .clone()
    };
    assert_eq!(service(&imported), service(&model));
    // Exported twice, the bytes do not change.
    assert_eq!(super::bpmn::export_xml(&imported).unwrap(), xml);

    let mixed = xml.replace(
        "implementation=\"agent\"",
        "implementation=\"agent\" nodeType=\"llm\"",
    );
    assert!(import(&mixed).is_none());
    let missing_config = xml.replace("agentConfig", "blockConfig");
    assert!(import(&missing_config).is_none());
}
