//! What a top-level flow run holds while it executes under an SLA policy: the
//! gate's permit and the policy's execution deadline.
//!
//! The dispatcher takes an [`SlaAdmission`] once per top-level entry
//! (`FlowDispatcher::admit_run`) and ends it with the way the run ended. A
//! blocking run ends it when `execute_blocking` returns. A streaming run hands
//! it to [`SlaAdmission::attach`], which moves it into the stream the caller
//! consumes: the slot is held until the stream finishes, fails, times out or is
//! dropped, whichever way the consumer destructures the `StreamingExecution`.
//!
//! An admission is bound to the flow it was taken for, so one taken for a flow
//! without a policy cannot be spent on a flow that has one.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use futures::Stream;
use tokio_util::sync::CancellationToken;

use std::sync::Arc;

use crate::db::{repository, DbPool};
use crate::flow_engine::dispatcher::DispatchError;
use crate::flow_engine::envelope::{EnvelopeDelta, FinishReason, FlowExecutionOutcome};
use crate::flow_engine::executor::{StreamingExecution, DEADLINE_EXCEEDED};
use crate::flow_engine::node_adapter::RunSettle;
use crate::sla::{Outcome, SlaPermit};

/// How long a run that was told to stop (its policy timeout elapsed) gets to
/// stop and persist its own result before it is dropped where it stands.
pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// The execution timeout of the policy a run was admitted under.
#[derive(Debug, Clone, Copy)]
struct PolicyTimeout {
    deadline: Instant,
    timeout: Duration,
}

/// The `flow_executions` row of the run, to be closed when the run times out:
/// a run that is cancelled or dropped by its timeout may never reach the code
/// that finalizes the row. The row is named by its id, which the executor
/// publishes through the run's [`RunSettle`] once it has created it, so a row of
/// an earlier attempt with the same request id is never touched.
#[derive(Clone)]
struct RunRow {
    db: DbPool,
    settle: Arc<RunSettle>,
}

impl std::fmt::Debug for RunRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRow")
            .field("execution_id", &self.settle.execution_id())
            .finish()
    }
}

impl RunRow {
    /// Tells the executor's finalizer that this run ended by timeout, so it
    /// writes `error` instead of `cancelled`. Must happen before the run is
    /// cancelled.
    ///
    /// The mark is held by the returned guard and removed when it is dropped,
    /// on every path, including a future dropped half way, so the set cannot
    /// outgrow the runs that are being closed.
    fn mark_timed_out(&self) -> Option<repository_timed_out::Mark> {
        let id = self.settle.execution_id();
        (id != 0).then(|| repository_timed_out::mark(id))
    }

    /// Marks the row `error` if the run left it `running`. A timeout is a
    /// failure of the run, not a cancellation by the caller, and
    /// `flow_executions.status` has no value of its own for it.
    async fn close(self, _mark: Option<repository_timed_out::Mark>) {
        let id = self.settle.execution_id();
        if id == 0 {
            return;
        }
        let closed = tokio::task::spawn_blocking(move || {
            repository::close_timed_out_flow_execution(&self.db, id)
        })
        .await;
        if !matches!(closed, Ok(Ok(_))) {
            tracing::warn!("could not close the flow execution row of a timed out run");
        }
    }

    /// For a stream, whose producer is still stopping: closes the row once the
    /// grace period is over, after the run's own finalizer had its chance.
    fn close_after_grace(self, mark: Option<repository_timed_out::Mark>) {
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_GRACE).await;
            self.close(mark).await;
        });
    }
}

/// Execution rows whose run ended by its policy timeout and has not yet been
/// finalized, by row id. The executor's finalizer reads it, so a run that stops
/// on the cancel token the timeout fired records `error` and not `cancelled`.
pub(crate) mod repository_timed_out {
    use std::sync::OnceLock;

    use dashmap::DashSet;

    fn set() -> &'static DashSet<i64> {
        static SET: OnceLock<DashSet<i64>> = OnceLock::new();
        SET.get_or_init(DashSet::new)
    }

    /// Removes its row's mark when dropped.
    #[derive(Debug)]
    pub(super) struct Mark(i64);

    impl Drop for Mark {
        fn drop(&mut self) {
            set().remove(&self.0);
        }
    }

    pub(super) fn mark(id: i64) -> Mark {
        set().insert(id);
        Mark(id)
    }

    #[cfg(test)]
    pub(crate) fn contains(id: i64) -> bool {
        set().contains(&id)
    }

    /// Whether row `id` was marked, consuming the mark.
    pub(crate) fn take(id: i64) -> bool {
        set().remove(&id).is_some()
    }
}

/// How a run bounded by a policy timeout ended.
pub(crate) enum Bounded<T> {
    /// It finished before its deadline.
    Done(T),
    /// The deadline passed. The run was told to cancel; `Some` when it stopped
    /// within the grace period (with what it returned), `None` when it had to be
    /// dropped.
    TimedOut(Option<T>),
}

/// One admitted top-level run. Dropping it without an outcome records the run
/// as cancelled, which is what a caller that went away looks like.
#[derive(Debug)]
#[must_use = "dropping the admission ends the run as cancelled"]
pub struct SlaAdmission {
    flow_id: String,
    permit: SlaPermit,
    policy: Option<PolicyTimeout>,
    row: Option<RunRow>,
    /// The token of the run itself: a child of the caller's, so the timeout
    /// stops the run without cancelling something the caller owns, and the
    /// caller's own cancellation still reaches the run.
    cancel: CancellationToken,
}

impl SlaAdmission {
    /// A run no policy governs: a flow without one, or a call made from inside
    /// a running flow. Holds nothing and records nothing.
    pub(crate) fn untracked(flow_id: &str) -> Self {
        Self {
            flow_id: flow_id.to_string(),
            permit: SlaPermit::untracked(),
            policy: None,
            row: None,
            cancel: CancellationToken::new(),
        }
    }

    /// `permit` may be untracked (the caller holds the slot, as the bus reactor
    /// does) and the admission still carries the policy's timeout, which is the
    /// backstop the bus path lacked.
    pub(crate) fn governed(
        flow_id: &str,
        permit: SlaPermit,
        timeout: Option<Duration>,
        db: &DbPool,
        settle: Arc<RunSettle>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            flow_id: flow_id.to_string(),
            permit,
            policy: timeout.map(|timeout| PolicyTimeout {
                deadline: Instant::now() + timeout,
                timeout,
            }),
            row: Some(RunRow {
                db: db.clone(),
                settle,
            }),
            cancel,
        }
    }

    /// The flow this admission was taken for.
    pub(crate) fn flow_id(&self) -> &str {
        &self.flow_id
    }

    /// When the policy's timeout elapses; `None` without one.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.policy.map(|policy| policy.deadline)
    }

    pub(crate) fn timeout_ms(&self) -> u64 {
        self.policy
            .map_or(0, |policy| policy.timeout.as_millis() as u64)
    }

    pub(crate) fn past_deadline(&self) -> bool {
        self.policy
            .is_some_and(|policy| Instant::now() >= policy.deadline)
    }

    /// Takes the admission back: the run never started, so the slot and the
    /// window call are returned and nothing is recorded.
    pub(crate) fn forgo(self) {
        self.permit.forgo();
    }

    pub(crate) fn finish_ok(self) {
        self.permit.finish(Outcome::Ok);
    }

    pub(crate) fn finish_cancelled(self) {
        self.permit.finish(Outcome::Cancelled);
    }

    /// The policy's timeout elapsed: counts it and closes the run's row.
    pub(crate) async fn finish_timed_out(self) {
        let row = self.row.clone();
        let mark = row.as_ref().and_then(RunRow::mark_timed_out);
        self.permit.finish(Outcome::Timeout);
        if let Some(row) = row {
            row.close(mark).await;
        }
    }

    /// [`Self::finish_timed_out`] for a caller that cannot wait for the row.
    fn finish_timed_out_later(self, mark: Option<repository_timed_out::Mark>) {
        let row = self.row.clone();
        self.permit.finish(Outcome::Timeout);
        if let Some(row) = row {
            row.close_after_grace(mark);
        }
    }

    /// A run that failed. One that failed because the policy's deadline passed
    /// (the executor stops between nodes when `meta.deadline` is reached) is a
    /// timeout, whichever of the two clocks noticed first.
    pub(crate) fn finish_failed(self) {
        let outcome = if self.past_deadline() {
            Outcome::Timeout
        } else {
            Outcome::Error
        };
        self.permit.finish(outcome);
    }

    /// Whether `outcome` ended because the POLICY's deadline passed: the run
    /// reports the deadline it was given, which is the caller's own when that is
    /// stricter, and such a run is the caller's timeout, not the policy's.
    pub(crate) fn outcome_is_timeout(&self, outcome: &FlowExecutionOutcome) -> bool {
        self.past_deadline() && outcome.error.as_deref() == Some(DEADLINE_EXCEEDED)
    }

    /// [`Self::outcome_is_timeout`] for a run that stopped with an error.
    pub(crate) fn error_is_timeout(&self, error: &anyhow::Error) -> bool {
        self.past_deadline() && error.to_string() == DEADLINE_EXCEEDED
    }

    /// A run that returned an outcome; the outcome may itself carry an error.
    pub(crate) fn finish_outcome(self, outcome: &FlowExecutionOutcome) {
        match (&outcome.error, outcome.finish_reason) {
            (None, _) => self.finish_ok(),
            (Some(_), FinishReason::Cancelled) => self.finish_cancelled(),
            (Some(_), _) if self.outcome_is_timeout(outcome) => {
                self.permit.finish(Outcome::Timeout);
            }
            (Some(_), _) => self.finish_failed(),
        }
    }

    /// Runs `run` until the policy's deadline. When it passes, the run's own
    /// token is fired and the run gets [`CANCEL_GRACE`] to stop and persist its
    /// result; only then is it dropped. A run without a policy timeout is just
    /// awaited.
    pub(crate) async fn run_within_policy<F: Future>(&self, run: F) -> Bounded<F::Output> {
        tokio::pin!(run);
        let Some(deadline) = self.deadline() else {
            return Bounded::Done(run.await);
        };
        tokio::select! {
            biased;
            output = &mut run => Bounded::Done(output),
            _ = tokio::time::sleep_until(deadline.into()) => {
                let _mark = self.row.as_ref().and_then(RunRow::mark_timed_out);
                self.cancel.cancel();
                match tokio::time::timeout(CANCEL_GRACE, &mut run).await {
                    Ok(output) => Bounded::TimedOut(Some(output)),
                    Err(_) => Bounded::TimedOut(None),
                }
            }
        }
    }

    /// Moves the admission into the stream of `exec`. A run with nothing to
    /// hold and no timeout is returned untouched. A stream that outlives the
    /// policy's timeout, or is dropped while still running, cancels the run's
    /// own token so the producer stops instead of working for nobody and the
    /// slot is never freed under it.
    pub(crate) fn attach(self, exec: StreamingExecution) -> StreamingExecution {
        if !self.permit.is_tracked() && self.policy.is_none() {
            return exec;
        }
        let StreamingExecution {
            stream,
            outcome,
            producer_input,
        } = exec;
        let cancel = self.cancel.clone();
        StreamingExecution {
            stream: Box::pin(GuardedStream::new(stream, self, cancel)),
            outcome,
            producer_input,
        }
    }
}

type DeltaResult = anyhow::Result<EnvelopeDelta>;

/// A stream that owns the admission of the run behind it.
struct GuardedStream {
    inner: BoxStream<'static, DeltaResult>,
    /// `None` once the run has ended.
    admission: Option<SlaAdmission>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    cancel: CancellationToken,
    /// An `Err` item or a trailer carrying an error went by.
    failed: bool,
    /// A chunk reported that the run was cancelled.
    cancelled: bool,
}

impl GuardedStream {
    fn new(
        inner: BoxStream<'static, DeltaResult>,
        admission: SlaAdmission,
        cancel: CancellationToken,
    ) -> Self {
        let sleep = admission
            .deadline()
            .map(|deadline| Box::pin(tokio::time::sleep_until(deadline.into())));
        Self {
            inner,
            admission: Some(admission),
            sleep,
            cancel,
            failed: false,
            cancelled: false,
        }
    }

    fn note(&mut self, delta: &EnvelopeDelta) {
        let (finish, error) = match delta {
            EnvelopeDelta::Llm(chunk) => (chunk.finish_reason, chunk.error.is_some()),
            EnvelopeDelta::Audio(chunk) => (chunk.finish_reason, false),
        };
        self.cancelled |= finish == Some(FinishReason::Cancelled);
        self.failed |= error || finish == Some(FinishReason::Error);
    }

    /// The stream ended on its own: the run's outcome is what went by.
    fn end(&mut self) {
        let Some(admission) = self.admission.take() else {
            return;
        };
        if self.cancelled || self.cancel.is_cancelled() {
            admission.finish_cancelled();
        } else if self.failed {
            admission.finish_failed();
        } else {
            admission.finish_ok();
        }
    }
}

impl Stream for GuardedStream {
    type Item = DeltaResult;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(admission) = this.admission.as_ref() else {
            return Poll::Ready(None);
        };
        if let Some(sleep) = this.sleep.as_mut() {
            if sleep.as_mut().poll(cx).is_ready() {
                let error = DispatchError::TimedOut {
                    flow_id: admission.flow_id().to_string(),
                    timeout_ms: admission.timeout_ms(),
                };
                let mark = admission.row.as_ref().and_then(RunRow::mark_timed_out);
                this.cancel.cancel();
                if let Some(admission) = this.admission.take() {
                    admission.finish_timed_out_later(mark);
                }
                this.sleep = None;
                // The text starts with the stable `sla.timeout` code, which the
                // edges that only see the stream's error text pass on.
                return Poll::Ready(Some(Err(anyhow::anyhow!(error.to_string()))));
            }
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(delta))) => {
                this.note(&delta);
                Poll::Ready(Some(Ok(delta)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.failed = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.end();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for GuardedStream {
    fn drop(&mut self) {
        let Some(admission) = self.admission.take() else {
            return;
        };
        // The consumer went away with the run still going: stop the producer
        // first, so the slot is not handed to a new call while the old one is
        // still working.
        self.cancel.cancel();
        if self.failed {
            admission.finish_failed();
        } else {
            admission.finish_cancelled();
        }
    }
}
