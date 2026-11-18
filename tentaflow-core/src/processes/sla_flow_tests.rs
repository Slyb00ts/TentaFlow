// ============ File: sla_flow_tests.rs — a flow task under an SLA policy is put off, not failed ============

use tentaflow_protocol::processes::ActivityVerification;
use tokio_util::sync::CancellationToken;

use super::repository;
use super::runtime::test_support::{flow, graph, service_model, start_model, Fixture};
use crate::sla::{repository as sla_repository, AdmitOrigin, SlaPolicySpec, SlaRegistry};

const WINDOW_MS: u64 = 3_600_000;
/// Half an hour into a one-hour window, with the clock frozen there.
const CLOCK_MS: u64 = 1_700_000_000_000 - (1_700_000_000_000 % WINDOW_MS) + WINDOW_MS / 2;

fn job_row(fixture: &Fixture, instance_id: &str) -> (String, i64, Option<String>, Option<i64>) {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT status,defer_count,last_defer_code,not_before_ms FROM bpmn_jobs WHERE instance_id=?1",
            [instance_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flow_task_refused_by_its_policy_is_deferred_for_the_refusals_retry_after_and_runs_later()
{
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let fixture = Fixture::new();
    let clock = Arc::new(AtomicU64::new(CLOCK_MS));
    let handle = clock.clone();
    let sla: &'static SlaRegistry =
        Box::leak(Box::new(SlaRegistry::with_clock(Arc::new(move || {
            handle.load(Ordering::SeqCst)
        }))));
    fixture.dispatcher().use_sla_registry(sla);

    let flow_id = flow(&fixture.db, &fixture.owner, &graph("deferred", None));
    let policy = sla_repository::create_policy(
        &fixture.db,
        crate::services::org::DEFAULT_ORG_ID,
        &fixture.owner.user_id,
        &SlaPolicySpec {
            name: "one call an hour".into(),
            call_limit: Some(1),
            call_window_secs: Some(3_600),
            ..Default::default()
        },
    )
    .unwrap();
    sla_repository::assign(
        &fixture.db,
        crate::services::org::DEFAULT_ORG_ID,
        &fixture.owner.user_id,
        "flow",
        &flow_id,
        &policy.id,
    )
    .unwrap();
    sla.refresh_if_stale(&fixture.db).unwrap();
    let gate = sla.gate(&flow_id).expect("the flow is governed");
    // Somebody else spends the one call of this window.
    gate.admit(AdmitOrigin::Request)
        .await
        .unwrap()
        .finish(crate::sla::Outcome::Ok);

    let started = start_model(
        &fixture,
        &service_model(&flow_id, ActivityVerification::Human),
    );
    let worker = "sla-worker";
    let claimed = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap()
        .expect("the queued flow task");
    let before_ms = chrono::Utc::now().timestamp_millis();
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        worker,
        claimed,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    // Nothing ran, nothing failed: the task is back in the queue and will not
    // be claimed again before the window that refused it is over.
    let (status, defers, code, not_before) = job_row(&fixture, &started.instance_id);
    let not_before = not_before.expect("a deferred job has a not-before time");
    assert_eq!(status, "queued");
    assert_eq!((defers, code.as_deref()), (1, Some("FLOW_OVERLOADED")));
    assert!(
        not_before >= before_ms + (WINDOW_MS / 2) as i64 - 1_000,
        "deferred for the refusal's retry_after, not just the usual backoff: {}",
        not_before - before_ms
    );
    let stats = gate.stats();
    assert_eq!(
        (stats.rejected_calls, stats.in_flight, stats.window_calls),
        (1, 0, 1)
    );
    let after =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(after.incidents.is_empty(), "a refusal is not an incident");

    // The window rolls over and the task runs.
    clock.fetch_add(WINDOW_MS, Ordering::SeqCst);
    assert!(
        repository::claim_job(&fixture.db, worker, before_ms)
            .unwrap()
            .is_none(),
        "not before its time"
    );
    let claimed = repository::claim_job(&fixture.db, worker, not_before + 1)
        .unwrap()
        .expect("claimable once its time has come");
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        worker,
        claimed,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (status, ..) = job_row(&fixture, &started.instance_id);
    assert_ne!(status, "queued", "the task ran");
    assert_eq!(gate.stats().accepted, 2, "the first call and this run");
}

fn govern(fixture: &Fixture, sla: &'static SlaRegistry, flow_id: &str, spec: SlaPolicySpec) {
    let policy = sla_repository::create_policy(
        &fixture.db,
        crate::services::org::DEFAULT_ORG_ID,
        &fixture.owner.user_id,
        &spec,
    )
    .unwrap();
    sla_repository::assign(
        &fixture.db,
        crate::services::org::DEFAULT_ORG_ID,
        &fixture.owner.user_id,
        "flow",
        flow_id,
        &policy.id,
    )
    .unwrap();
    sla.refresh_if_stale(&fixture.db).unwrap();
}

fn own_registry(fixture: &Fixture) -> &'static SlaRegistry {
    let sla: &'static SlaRegistry = Box::leak(Box::new(SlaRegistry::default()));
    fixture.dispatcher().use_sla_registry(sla);
    sla
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flow_task_that_hits_its_policy_timeout_fails_like_any_dispatched_flow_error_and_is_not_interrupted(
) {
    let fixture = Fixture::new();
    let sla = own_registry(&fixture);
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("slow", Some(60_000)));
    govern(
        &fixture,
        sla,
        &flow_id,
        SlaPolicySpec {
            name: "short".into(),
            max_concurrency: Some(1),
            timeout_ms: Some(200),
            ..Default::default()
        },
    );
    let gate = sla.gate(&flow_id).expect("governed");
    let started = start_model(
        &fixture,
        &service_model(&flow_id, ActivityVerification::Human),
    );
    let worker = "timeout-worker";
    let claimed = repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
        .unwrap()
        .expect("the queued flow task");
    // The job's own token: the policy's timeout must not cancel it, or the job
    // would be reported as interrupted instead of failed.
    let job_token = CancellationToken::new();
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        worker,
        claimed,
        job_token.clone(),
    )
    .await
    .unwrap();

    assert!(!job_token.is_cancelled(), "the job's token is not touched");
    let instance =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(instance.incidents.len(), 1, "{:?}", instance.incidents);
    // The flow had been dispatched when it failed, so its effect is reported as
    // uncertain, the way any failed dispatched flow is. It is not an interruption
    // of the worker: the job's own token was never cancelled.
    assert_eq!(instance.incidents[0].code, "EXTERNAL_OUTCOME_UNCERTAIN");
    let stats = gate.stats();
    assert_eq!(
        (stats.timeouts, stats.cancelled, stats.in_flight),
        (1, 0, 0)
    );
    let running: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM flow_executions WHERE status = 'running'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(running, 0, "no audit row is left running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_addon_call_into_a_governed_flow_takes_no_slot() {
    use crate::compliance::ai_gateway::AiGatewayContext;
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};
    use crate::flow_engine::dispatchers::ParentRun;

    let fixture = Fixture::new();
    let sla = own_registry(&fixture);
    let flow_id = crate::db::repository::create_flow(
        &fixture.db,
        &crate::db::models::FlowParams {
            name: "Nested model",
            description: None,
            is_default: false,
            service_type: Some("chat"),
            flow_json: &graph("nested", None),
            status: "active",
            published_model_name: Some("nested-model"),
            actor_user_id: Some(&fixture.owner.user_id),
        },
    )
    .unwrap();
    fixture
        .router
        .catalog_provider()
        .rebuild(
            &crate::services::mesh_registry::MeshServicesRegistry::new(),
            &fixture.db,
        )
        .unwrap();
    govern(
        &fixture,
        sla,
        &flow_id,
        SlaPolicySpec {
            name: "one at a time".into(),
            max_concurrency: Some(1),
            max_queue: Some(0),
            ..Default::default()
        },
    );
    let gate = sla.gate(&flow_id).expect("governed");
    let _held = gate.admit(AdmitOrigin::Request).await.unwrap();

    let request = || -> crate::api::openai::types::ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "nested-model",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap()
    };
    let call = |parent: Option<ParentRun>| {
        let router = fixture.router.clone();
        let request = request();
        async move {
            router
                .route_chat_completion(
                    request,
                    None,
                    FlowOrigin::Addon,
                    FlowActor::system(),
                    Some(AiGatewayContext {
                        parent_run: parent,
                        ..Default::default()
                    }),
                )
                .await
        }
    };
    let overloaded = |result: &anyhow::Result<_>| {
        matches!(
            result
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<crate::error::CoreError>()),
            Some(crate::error::CoreError::Overloaded { .. })
        )
    };

    // An addon acting on its own behalf is a top-level call and is refused...
    assert!(overloaded(&call(None).await), "a top-level call is refused");
    // ...one made on behalf of a running flow is part of that run.
    let nested = call(Some(ParentRun::unrecorded(FlowOrigin::Chat))).await;
    assert!(!overloaded(&nested), "a nested call is not refused");
    assert_eq!(gate.stats().accepted, 1, "only the held call was counted");
}
