// ============ File: jobs.rs — fenced process service jobs using the existing flow executor ============

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{ActivityOutcome, ActivityResult, ProcessNodeKind};
use tokio_util::sync::CancellationToken;

use super::repository::{self, ClaimedProcessJob};
use super::runtime::{plan_job_result, validate_output};
use crate::db::DbPool;
use crate::flow_engine::dispatcher::{FlowDispatcher, PinnedFlowSnapshot};
use crate::flow_engine::envelope::{FlowEnvelope, FlowExecutionOutcome, FlowValue};
use crate::flow_engine::expr::flow_value_to_json;

pub const LEASE_RENEWAL: Duration = Duration::from_secs(10);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn failure(code: &str, message: impl Into<String>) -> ActivityResult {
    let message = message.into();
    let summary = repository::bounded_failure_message(&message);
    if summary != message {
        tracing::error!(code, reason = %message, "full process service failure exceeds its history summary budget");
    }
    ActivityResult {
        outcome: ActivityOutcome::Error,
        code: Some(code.into()),
        summary,
        outputs: Value::Null,
        evidence: Vec::new(),
    }
}

fn normalize_outcome(outcome: FlowExecutionOutcome) -> Result<ActivityResult> {
    if let Some(error) = outcome.error {
        return Ok(failure("FLOW_ERROR", error));
    }
    let envelope = outcome.final_envelope;
    let variables = envelope
        .variables
        .iter()
        .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
        .collect::<BTreeMap<_, _>>();
    let artifacts = envelope
        .artifacts
        .iter()
        .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
        .collect::<BTreeMap<_, _>>();
    let outputs = json!({
        "payload": flow_value_to_json(&envelope.payload),
        "variables": variables,
        "artifacts": artifacts,
    });
    validate_output(&outputs)?;
    Ok(ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "The pinned flow returned an activity result".into(),
        outputs,
        evidence: vec![format!(
            "flow_execution_latency_ms:{}",
            outcome.total_latency_ms
        )],
    })
}

fn fail_claim(
    pool: &DbPool,
    worker_id: &str,
    claimed: &ClaimedProcessJob,
    code: &str,
    error: impl std::fmt::Display,
) -> Result<()> {
    let message = error.to_string();
    tracing::error!(job_id = %claimed.job.job_id, code, reason = %message, "process service claim failed");
    repository::fail_job(
        pool,
        &claimed.job.job_id,
        claimed.job.attempt,
        claimed.job.fence,
        worker_id,
        code,
        &message,
        now_ms(),
    )?;
    Ok(())
}

pub async fn execute_claimed(
    pool: &DbPool,
    dispatcher: &Arc<FlowDispatcher>,
    worker_id: &str,
    claimed: ClaimedProcessJob,
    cancel: CancellationToken,
) -> Result<()> {
    let node = claimed
        .snapshot
        .model
        .nodes
        .iter()
        .find(|node| node.id == claimed.job.node_id)
        .context("claimed job node missing")?;
    let ProcessNodeKind::ServiceTask {
        flow_id,
        timeout_seconds,
        ..
    } = &node.kind
    else {
        return fail_claim(
            pool,
            worker_id,
            &claimed,
            "INVALID_SERVICE_JOB",
            "job node is not a service task",
        );
    };
    let snapshot = claimed
        .snapshot
        .service_snapshots
        .iter()
        .find(|snapshot| snapshot.info.node_id == node.id && snapshot.info.flow_id == *flow_id)
        .context("pinned service graph missing")?;
    let pinned = PinnedFlowSnapshot {
        flow_id: snapshot.info.flow_id.clone(),
        source_version: snapshot.info.source_version,
        graph_json: snapshot.graph_json.clone(),
        graph_sha256: snapshot.info.graph_sha256.clone(),
    };
    let mut meta = match dispatcher.authorize_process_flow(
        flow_id,
        &claimed.actor.user_id,
        &claimed.actor.org_id,
    ) {
        Ok(meta) => meta,
        Err(error) => return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error),
    };
    meta.request_id = format!(
        "{}:{}:{}",
        claimed.job.job_id, claimed.job.attempt, claimed.job.fence
    );
    meta.correlation_id = Some(claimed.job.instance_id.clone());
    meta.cancel_token = cancel.clone();
    meta.deadline = Some(Instant::now() + Duration::from_secs(u64::from(*timeout_seconds)));
    let mut envelope = FlowEnvelope::empty();
    envelope.payload = FlowValue::Json(
        claimed
            .job
            .input
            .get("payload")
            .context("service input payload missing")?
            .clone(),
    );
    envelope.variables = claimed
        .job
        .input
        .get("variables")
        .and_then(Value::as_object)
        .context("service input variables missing")?
        .iter()
        .map(|(key, value)| (key.clone(), FlowValue::Json(value.clone())))
        .collect();

    // Cancellation may commit between the claim and running-handle registration.
    // Check the durable claim after registration, before polling the executor.
    if cancel.is_cancelled() {
        return fail_claim(
            pool,
            worker_id,
            &claimed,
            "INTERRUPTED",
            "the worker was cancelled before executing the service task",
        );
    }
    match repository::renew_job_lease(
        pool,
        &claimed.job.job_id,
        claimed.job.attempt,
        claimed.job.fence,
        worker_id,
        now_ms(),
    ) {
        Ok(true) => {}
        Ok(false) => {
            cancel.cancel();
            return fail_claim(
                pool,
                worker_id,
                &claimed,
                "LEASE_LOST",
                "service job no longer owns its execution lease",
            );
        }
        Err(error) => {
            cancel.cancel();
            return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error);
        }
    }

    let mut renewal =
        tokio::time::interval_at(tokio::time::Instant::now() + LEASE_RENEWAL, LEASE_RENEWAL);
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let execution = dispatcher.dispatch_pinned_flow(&pinned, envelope, meta);
    tokio::pin!(execution);
    let timeout = tokio::time::sleep(Duration::from_secs(u64::from(*timeout_seconds)));
    tokio::pin!(timeout);
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return fail_claim(pool, worker_id, &claimed, "INTERRUPTED", "the worker was cancelled before its external effect was confirmed");
            }
            _ = renewal.tick() => {
                if let Err(error) = dispatcher.authorize_process_flow(flow_id, &claimed.actor.user_id, &claimed.actor.org_id) {
                    cancel.cancel();
                    return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error);
                }
                match repository::renew_job_lease(pool, &claimed.job.job_id, claimed.job.attempt, claimed.job.fence, worker_id, now_ms()) {
                    Ok(true) => {},
                    Ok(false) => {
                        cancel.cancel();
                        return fail_claim(pool, worker_id, &claimed, "LEASE_LOST", "service job no longer owns its execution lease");
                    }
                    Err(error) => {
                        cancel.cancel();
                        return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error);
                    }
                }
            }
            _ = &mut timeout => {
                cancel.cancel();
                break failure("SERVICE_TIMEOUT", "the service task exceeded its configured timeout");
            }
            outcome = &mut execution => {
                break match outcome {
                    Ok(outcome) => normalize_outcome(outcome).unwrap_or_else(|error| failure("OUTPUT_LIMIT", error.to_string())),
                    Err(error) => failure("FLOW_ERROR", error.to_string()),
                };
            }
        }
    };

    // A CAS retry only recalculates transitions from the already observed result.
    // It never repeats an external flow execution.
    for _ in 0..8 {
        let current =
            match repository::runtime_snapshot(pool, &claimed.actor, &claimed.job.instance_id) {
                Ok(current) => current,
                Err(error) => {
                    return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error)
                }
            };
        let Some(job) = current
            .jobs
            .iter()
            .find(|job| job.job_id == claimed.job.job_id)
        else {
            return Ok(());
        };
        if job.status != "running"
            || job.attempt != claimed.job.attempt
            || job.fence != claimed.job.fence
        {
            return Ok(());
        }
        if let Err(error) = dispatcher.authorize_process_flow(
            flow_id,
            &claimed.actor.user_id,
            &claimed.actor.org_id,
        ) {
            return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error);
        }
        let at_ms = now_ms();
        let plan = match plan_job_result(&current, job, &result, at_ms) {
            Ok(plan) => plan,
            Err(error) => return fail_claim(pool, worker_id, &claimed, "TRANSITION_ERROR", error),
        };
        match repository::accept_job_result(
            pool,
            &claimed.actor,
            &job.job_id,
            job.attempt,
            job.fence,
            worker_id,
            &result,
            current.instance.revision,
            &plan,
            at_ms,
        ) {
            Ok(_) => return Ok(()),
            Err(error) if error.to_string().contains("revision conflict") => continue,
            Err(error) => return fail_claim(pool, worker_id, &claimed, "RESULT_REJECTED", error),
        }
    }
    fail_claim(
        pool,
        worker_id,
        &claimed,
        "REVISION_CONFLICT",
        "concurrent process transitions prevented result publication",
    )
}

#[cfg(test)]
mod tests {
    use super::super::runtime::{self, test_support::*};
    use super::*;
    use tentaflow_protocol::processes::{
        ActivityVerification, ProcessInstanceStatus, ProcessUserTaskStatus,
    };

    async fn execute(fixture: &Fixture, worker: &str) -> ClaimedProcessJob {
        let claimed = repository::claim_job(&fixture.db, worker, now_ms())
            .unwrap()
            .expect("claim persisted service");
        execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            worker,
            claimed.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        claimed
    }

    async fn observe_effect(fixture: &Fixture, claimed: &ClaimedProcessJob) -> ActivityResult {
        let source = &claimed.snapshot.service_snapshots[0];
        let pinned = PinnedFlowSnapshot {
            flow_id: source.info.flow_id.clone(),
            source_version: source.info.source_version,
            graph_json: source.graph_json.clone(),
            graph_sha256: source.info.graph_sha256.clone(),
        };
        let meta = fixture
            .dispatcher()
            .authorize_process_flow(
                &pinned.flow_id,
                &claimed.actor.user_id,
                &claimed.actor.org_id,
            )
            .unwrap();
        let envelope =
            FlowEnvelope::with_payload(FlowValue::Json(claimed.job.input["payload"].clone()));
        normalize_outcome(
            fixture
                .dispatcher()
                .dispatch_pinned_flow(&pinned, envelope, meta)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn published_service_executes_pinned_graph_after_live_source_edit() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("published", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "outputs.variables.marker == 'published'".into(),
                },
            ),
        );
        update_flow(
            &fixture.db,
            &fixture.owner,
            &flow_id,
            &graph("edited", None),
        );
        let current = crate::db::repository::get_flow(&fixture.db, &flow_id)
            .unwrap()
            .unwrap();
        let claim = execute(&fixture, "pinned-worker").await;
        assert!(
            current.version > i64::from(claim.snapshot.service_snapshots[0].info.source_version)
        );
        let completed =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(completed.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.instance.variables["answer"], "published");
        let job = completed
            .jobs
            .iter()
            .find(|job| job.job_id == claim.job.job_id)
            .unwrap();
        assert_eq!(
            job.result.as_ref().unwrap().outputs["variables"]["marker"],
            "published"
        );
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            history
                .iter()
                .filter(|event| event.kind == "service_result")
                .count(),
            1
        );
        let result = job.result.as_ref().unwrap();
        let plan = runtime::plan_job_result(&claim.snapshot, &claim.job, result, at_ms).unwrap();
        repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "pinned-worker",
            result,
            claim.snapshot.instance.revision,
            &plan,
            at_ms,
        )
        .unwrap();
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            history
        );
    }

    #[tokio::test]
    async fn human_verification_uses_persisted_result_and_explicit_retry_after_rejection() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("evidence", None));
        let started = start_model(
            &fixture,
            &service_model(&flow_id, ActivityVerification::Human),
        );
        let first = execute(&fixture, "human-worker").await;
        let waiting =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(waiting.instance.status, ProcessInstanceStatus::Waiting);
        assert!(waiting.instance.variables.get("answer").is_none());
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let detail = repository::get_user_task(
            &fixture.db,
            &fixture.owner,
            &started.instance_id,
            &task.user_task_id,
        )
        .unwrap();
        assert_eq!(detail.outputs["outputs"]["variables"]["marker"], "evidence");
        assert!(runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &Value::Null,
            None,
            at_ms
        )
        .is_err());
        let rejection = runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &json!("insufficient"),
            Some(false),
            at_ms,
        )
        .unwrap();
        let rejected = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("reject result"),
            &started.instance_id,
            &task.user_task_id,
            waiting.instance.revision,
            &json!("insufficient"),
            Some(false),
            &rejection,
            at_ms,
        )
        .unwrap();
        assert_eq!(rejected.status, ProcessInstanceStatus::Incident);
        assert_eq!(rejected.incidents[0].code, "HUMAN_REJECTED");
        assert_eq!(
            rejected.incidents[0].job_id.as_deref(),
            Some(first.job.job_id.as_str())
        );
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("retry rejected service"),
            &started.instance_id,
            &first.job.job_id,
            rejected.revision,
        )
        .unwrap();
        let second = execute(&fixture, "human-worker").await;
        assert!(second.job.attempt > first.job.attempt);
        assert!(second.job.fence > first.job.fence);
        let waiting =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let approval = runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &json!({"answer":"client cannot replace service output"}),
            Some(true),
            at_ms,
        )
        .unwrap();
        let completed = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("approve observed result"),
            &started.instance_id,
            &task.user_task_id,
            waiting.instance.revision,
            &json!({"answer":"client cannot replace service output"}),
            Some(true),
            &approval,
            at_ms,
        )
        .unwrap();
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.variables["answer"], "evidence");
        let executions =
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap();
        assert_eq!(
            executions.len(),
            2,
            "only the explicit retry executes the service again"
        );
    }

    #[tokio::test]
    async fn observed_effect_before_crash_is_not_reexecuted_without_retry_and_old_fence_is_rejected(
    ) {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("observed", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let claim = repository::claim_job(&fixture.db, "lost-worker", now_ms())
            .unwrap()
            .unwrap();
        let observed = observe_effect(&fixture, &claim).await;
        let late_plan =
            runtime::plan_job_result(&claim.snapshot, &claim.job, &observed, at_ms).unwrap();
        let path = fixture.directory.path().join("processes.db");
        let actor = fixture.owner.clone();
        let Fixture {
            directory,
            db,
            router,
            participant,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let db = crate::db::init(&path).unwrap();
        assert_eq!(repository::recover_jobs(&db, None, now_ms()).unwrap(), 1);
        assert_eq!(repository::recover_jobs(&db, None, now_ms()).unwrap(), 0);
        assert!(repository::claim_job(&db, "replacement", now_ms())
            .unwrap()
            .is_none());
        let interrupted = repository::get_instance(&db, &actor, &started.instance_id).unwrap();
        assert_eq!(interrupted.status, ProcessInstanceStatus::Incident);
        assert_eq!(interrupted.incidents[0].code, "INTERRUPTED");
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&db, &flow_id, 100)
                .unwrap()
                .len(),
            1
        );
        assert!(repository::accept_job_result(
            &db,
            &actor,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "lost-worker",
            &observed,
            claim.snapshot.instance.revision,
            &late_plan,
            at_ms
        )
        .is_err());
        repository::retry_job(
            &db,
            &actor,
            &stamp("retry uncertain effect"),
            &started.instance_id,
            &claim.job.job_id,
            interrupted.revision,
        )
        .unwrap();
        let router = Arc::new(
            crate::routing::Router::new(crate::config::RouterConfig::default(), Some(db.clone()))
                .unwrap(),
        );
        let fixture = Fixture {
            directory,
            db,
            router,
            owner: actor,
            participant,
        };
        execute(&fixture, "replacement").await;
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn queued_revocation_and_cancelled_claim_never_publish_a_late_result() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("authorized", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "deny",
        )
        .unwrap();
        assert!(
            repository::claim_job(&fixture.db, "revoked-worker", now_ms())
                .unwrap()
                .is_none()
        );
        let denied =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(denied.status, ProcessInstanceStatus::Incident);
        assert_eq!(denied.incidents[0].code, "SOURCE_ACCESS_REVOKED");
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "allow",
        )
        .unwrap();
        let job_id = denied.incidents[0].job_id.as_ref().unwrap();
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("restore source and retry"),
            &started.instance_id,
            job_id,
            denied.revision,
        )
        .unwrap();
        let claim = repository::claim_job(&fixture.db, "cancelled-worker", now_ms())
            .unwrap()
            .unwrap();
        let observed = observe_effect(&fixture, &claim).await;
        let plan = runtime::plan_job_result(&claim.snapshot, &claim.job, &observed, at_ms).unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        repository::cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("cancel before result"),
            &started.instance_id,
            current.revision,
        )
        .unwrap();
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert!(!repository::renew_job_lease(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            now_ms()
        )
        .unwrap());
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            &observed,
            current.revision,
            &plan,
            at_ms
        )
        .is_err());
        assert!(!repository::fail_job(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            "INTERRUPTED",
            "late worker",
            now_ms()
        )
        .unwrap());
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Cancelled
        );
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            events
        );
    }

    #[tokio::test]
    async fn cancelled_or_expired_claim_never_enters_the_flow_executor() {
        for scenario in ["cancelled", "expired", "worker_stopped"] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("must not execute", None),
            );
            let started = start_model(
                &fixture,
                &service_model(
                    &flow_id,
                    ActivityVerification::Condition {
                        expression: "true".into(),
                    },
                ),
            );
            let claimed_at = if scenario == "expired" {
                now_ms() - 31_000
            } else {
                now_ms()
            };
            let claim = repository::claim_job(&fixture.db, scenario, claimed_at)
                .unwrap()
                .expect("claim the actual persisted job before its execution gate");
            let cancel = CancellationToken::new();
            if scenario == "cancelled" {
                repository::cancel_instance(
                    &fixture.db,
                    &fixture.owner,
                    &stamp("cancel in the claim registration window"),
                    &started.instance_id,
                    claim.snapshot.instance.revision,
                )
                .unwrap();
                assert!(!cancel.is_cancelled());
            } else if scenario == "worker_stopped" {
                cancel.cancel();
            }
            let before =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            execute_claimed(&fixture.db, fixture.dispatcher(), scenario, claim, cancel)
                .await
                .unwrap();
            assert!(
                crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                    .unwrap()
                    .is_empty(),
                "{scenario} claim entered the actual flow executor"
            );
            let current =
                repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let events =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            assert!(current.variables.get("answer").is_none());
            assert!(events.iter().all(|event| event.kind != "service_result"));
            if scenario == "cancelled" {
                assert_eq!(current.status, ProcessInstanceStatus::Cancelled);
                assert_eq!(events, before);
            } else {
                assert_eq!(current.status, ProcessInstanceStatus::Incident);
                assert_eq!(
                    current.incidents[0].code,
                    if scenario == "expired" {
                        "LEASE_LOST"
                    } else {
                        "INTERRUPTED"
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn current_acl_is_rechecked_after_actual_service_execution_has_started() {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("not publishable", Some(500)),
        );
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let claim = repository::claim_job(&fixture.db, "running-worker", now_ms())
            .unwrap()
            .unwrap();
        let db = fixture.db.clone();
        let dispatcher = fixture.dispatcher().clone();
        let task = tokio::spawn(async move {
            execute_claimed(
                &db,
                &dispatcher,
                "running-worker",
                claim,
                CancellationToken::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("observe actual flow execution row before revoking access");
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "deny",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "SOURCE_ACCESS_REVOKED");
        assert!(current.variables.get("answer").is_none());
        assert!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0
                .iter()
                .all(|event| event.kind != "service_result")
        );
    }

    #[tokio::test]
    async fn false_condition_and_oversized_output_record_incidents_instead_of_success() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("evidence", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "outputs.variables.marker == 'unproved'".into(),
                },
            ),
        );
        let first = execute(&fixture, "condition-worker").await;
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "VERIFICATION_FAILED");
        assert!(current.variables.get("answer").is_none());
        assert!(current.can_retry);
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("explicit condition retry"),
            &started.instance_id,
            &first.job.job_id,
            current.revision,
        )
        .unwrap();
        execute(&fixture, "condition-worker").await;
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(
            current.incidents.len(),
            1,
            "prior retry incident is resolved, not silently accepted"
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap()
                .len(),
            2
        );

        let mut large_graph: Value = serde_json::from_str(&graph("unused", None)).unwrap();
        large_graph["nodes"][0]["config"]["output_mapping"]["marker"] = json!("payload.large");
        let large_flow = flow(&fixture.db, &fixture.owner, &large_graph.to_string());
        let mut large_model = service_model(&large_flow, ActivityVerification::Human);
        large_model
            .variables
            .insert("large".into(), json!("e".repeat(160 * 1024)));
        let started = start_model(&fixture, &large_model);
        execute(&fixture, "bounded-output-worker").await;
        let current =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(current.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.instance.incidents[0].code, "OUTPUT_LIMIT");
        assert_eq!(
            current.jobs[0].status, "error",
            "a real post-effect output error must terminate the claim"
        );
        assert!(current.instance.can_retry);
        assert!(
            current.instance.user_tasks.is_empty(),
            "oversized output is not a fictitious verification success"
        );
    }

    #[tokio::test]
    async fn expired_lease_rejects_observed_result_and_long_failure_keeps_explicit_provenance() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("late", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let at_ms = now_ms() - 31_000;
        let claim = repository::claim_job(&fixture.db, "overdue-worker", at_ms)
            .unwrap()
            .unwrap();
        assert!(at_ms < claim.job.lease_until_ms.unwrap());
        assert!(claim.job.lease_until_ms.unwrap() < now_ms());
        let observed = observe_effect(&fixture, &claim).await;
        let plan = runtime::plan_job_result(&claim.snapshot, &claim.job, &observed, at_ms).unwrap();
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "overdue-worker",
            &observed,
            claim.snapshot.instance.revision,
            &plan,
            at_ms
        )
        .is_err());
        let reason = "A real service failure with Unicode detail: 🧪".repeat(1200);
        let result = failure("LARGE_REASON", reason.clone());
        assert!(result.summary.contains("original bytes="));
        assert!(result.summary.contains("sha256="));
        fail_claim(&fixture.db, "overdue-worker", &claim, "LEASE_LOST", &reason).unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "LEASE_LOST");
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let failure = history
            .iter()
            .find(|event| event.kind == "job_failed")
            .unwrap();
        assert!(failure.data["message"]
            .as_str()
            .unwrap()
            .contains("original bytes="));
        assert!(failure.data["message"]
            .as_str()
            .unwrap()
            .contains("sha256="));
        assert!(history.iter().all(|event| event.kind != "service_result"));
    }
}
