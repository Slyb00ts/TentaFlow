// ===== File: events/metrics.rs — §2.7 timings, read back out of the log =====
//
// Every number here is a DIFFERENCE BETWEEN TWO STORED EVENTS. Nothing measures
// anything at run time and no adapter carries a stopwatch (invariant 5), so a
// timing can only ever be as good as the pair of rows it comes from — and when
// one of the pair is missing there is no number, not a zero.
//
// Two rules do the work.
//
// **Tools pair by `call_id`, never by name.** Once tools started running side by
// side, matching a start to an end by tool name joined one call's start to
// another call's end and produced a duration neither of them had.
//
// **An unclosed call is dropped when its turn closes.** A call whose result
// never arrived has no duration; counting it as zero flatters the tool and
// counting it as "until now" poisons the average of that tool permanently.
// Dropping it leaves a hole that can be seen, which is the honest failure mode.

use anyhow::{anyhow, Result};

use crate::db::DbPool;

/// One streaming step's two halves. `request_started` is not in the log (no
/// engine event opens a run), so TTFT is measured from the step that produced
/// the token — which is exactly §2.7's "in the same step".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLatency {
    pub node_id: String,
    /// Node type as recorded at `step_start`; empty when the start was never
    /// seen.
    pub step: String,
    /// `step_start` → `first_token`.
    pub ttft_ms: i64,
    /// `first_token` → the step's `step_end`. `None` when the step never
    /// closed: it failed (stored as `error`) or is still running. A decode time
    /// that was never completed has no value, and the log will not invent one.
    pub decode_ms: Option<i64>,
}

/// One tool call that completed inside its turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDuration {
    pub call_id: String,
    pub tool: String,
    pub ms: i64,
}

/// TTFT and decode time per streaming step of one run.
pub fn step_latencies(pool: &DbPool, run_id: &str) -> Result<Vec<StepLatency>> {
    let conn = pool.read().map_err(|e| anyhow!("events db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT f.node_id, \
                s.payload_json ->> '$.step'                          AS step, \
                f.at_ms - s.at_ms                                    AS ttft_ms, \
                (SELECT e.at_ms FROM run_events e \
                  WHERE e.run_id = f.run_id AND e.node_id = f.node_id \
                    AND e.kind = 'step_end' AND e.seq > f.seq \
                  ORDER BY e.seq LIMIT 1) - f.at_ms                   AS decode_ms \
           FROM run_events f \
           JOIN run_events s \
             ON s.run_id = f.run_id AND s.node_id = f.node_id AND s.kind = 'step_start' \
            AND s.seq = (SELECT MAX(x.seq) FROM run_events x \
                          WHERE x.run_id = f.run_id AND x.node_id = f.node_id \
                            AND x.kind = 'step_start' AND x.seq < f.seq) \
          WHERE f.run_id = ?1 AND f.kind = 'first_token' \
          ORDER BY f.seq",
    )?;
    let rows = stmt.query_map(rusqlite::params![run_id], |row| {
        Ok(StepLatency {
            node_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            step: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ttft_ms: row.get(2)?,
            decode_ms: row.get(3)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| anyhow!("step latencies of run {run_id}: {e}"))
}

/// Duration of every tool call of one run that closed before its turn did.
///
/// The inner join is what excludes a call that never came back; the `NOT EXISTS`
/// is what excludes a result that arrived after the turn had already closed, so
/// a late answer cannot resurrect a call the run had given up on.
pub fn tool_durations(pool: &DbPool, run_id: &str) -> Result<Vec<ToolDuration>> {
    let conn = pool.read().map_err(|e| anyhow!("events db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT c.call_id, \
                c.payload_json ->> '$.name' AS tool, \
                r.at_ms - c.at_ms           AS ms \
           FROM run_events c \
           JOIN run_events r \
             ON r.run_id = c.run_id AND r.call_id = c.call_id \
            AND r.kind = 'tool_result' AND r.seq > c.seq \
          WHERE c.run_id = ?1 AND c.kind = 'tool_call' \
            AND NOT EXISTS (SELECT 1 FROM run_events t \
                             WHERE t.run_id = c.run_id AND t.kind = 'turn_end' \
                               AND t.seq > c.seq AND t.seq < r.seq) \
          ORDER BY c.seq",
    )?;
    let rows = stmt.query_map(rusqlite::params![run_id], |row| {
        Ok(ToolDuration {
            call_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            tool: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ms: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| anyhow!("tool durations of run {run_id}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use serde_json::json;
    use tokio::sync::mpsc;

    use crate::events::progress_log::RunEventLog;
    use crate::events::store::read_run;
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};
    use crate::flow_engine::envelope::{
        EnvelopeDelta, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk, NodeInput,
    };
    use crate::flow_engine::executor::execute_streaming;
    use crate::flow_engine::node_adapter::{
        test_support::stub_ctx, AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec,
        StreamProducerAdapter,
    };
    use crate::flow_engine::node_adapters::{OutputNodeAdapter, TriggerNodeAdapter};
    use crate::flow_engine::progress_broker::{BrokerProgressSink, ProgressBroker, RunProvenance};
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
    use crate::flow_engine::types::{FlowDataType, FlowNode};

    const SCOPE: &str = "metrics-scope";

    fn provenance(run_id: &str) -> RunProvenance {
        let mut meta = FlowRequestMeta::new(run_id, FlowOrigin::Chat, FlowActor::user("u-1"));
        meta.org_id = Some("org-1".into());
        meta.session_id = Some(SCOPE.into());
        RunProvenance::from_meta(&meta)
    }

    fn main_db() -> crate::db::DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0','test','{}','active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    /// Streaming producer fed from a channel, so the test decides exactly when
    /// the first token appears and when the stream ends. Both distances are
    /// real waits, which is what makes the asserted numbers a measurement of
    /// the log rather than of arithmetic.
    struct ChannelProducer {
        rx: Mutex<Option<mpsc::Receiver<LlmStreamChunk>>>,
    }

    #[async_trait]
    impl NodeAdapter for ChannelProducer {
        fn node_type(&self) -> &str {
            "metrics_probe"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![
                PortSpec::new("stream", FlowDataType::Text),
                PortSpec::new("full", FlowDataType::Text),
            ]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> AnyResult<FlowEnvelope> {
            Ok((*ctx.initial_envelope).clone())
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for ChannelProducer {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> AnyResult<BoxStream<'static, AnyResult<EnvelopeDelta>>> {
            let rx = self
                .rx
                .lock()
                .expect("producer lock")
                .take()
                .expect("produce_stream runs once per flow");
            Ok(
                futures::stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|c| (Ok(EnvelopeDelta::Llm(c)), rx))
                })
                .boxed(),
            )
        }
    }

    /// Runs a real streaming flow (trigger → probe → output) whose progress goes
    /// through the production `BrokerProgressSink` into a real event log, with a
    /// `wait_before_first_token` pause before the first delta and a
    /// `wait_before_end` pause before the stream closes.
    async fn streaming_run(
        run_id: &str,
        wait_before_first_token: Duration,
        wait_before_end: Duration,
    ) -> (tempfile::TempDir, DbPool, RunEventLog) {
        let (dir, events_pool) = crate::events::test_support::events_db();
        let broker = Arc::new(ProgressBroker::new());
        broker.bind_run_provenance(SCOPE, provenance(run_id));
        let log = RunEventLog::new(events_pool.clone(), broker.clone());
        log.attach(SCOPE);

        let (tx, rx) = mpsc::channel::<LlmStreamChunk>(8);
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(TriggerNodeAdapter::new()));
        registry.register(Arc::new(OutputNodeAdapter::new()));
        registry.register_stream_producer(Arc::new(ChannelProducer {
            rx: Mutex::new(Some(rx)),
        }));
        let registry = Arc::new(registry);

        let flow = json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "p", "type": "metrics_probe", "config": {}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "to": "p", "from_port": "text", "to_port": "in"},
                {"from": "p", "to": "o", "from_port": "stream", "to_port": "text"}
            ]
        });
        let compiled =
            Arc::new(CompiledFlow::from_json("0", &flow.to_string(), &registry).expect("compile"));

        let mut ctx = stub_ctx();
        ctx.request_id = run_id.to_string();
        ctx.progress = Arc::new(BrokerProgressSink::new(broker.clone()));
        ctx.progress_scope = SCOPE.to_string();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(main_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");
        let mut stream = exec.stream;

        tokio::time::sleep(wait_before_first_token).await;
        tx.send(LlmStreamChunk {
            text_delta: "hello".into(),
            ..Default::default()
        })
        .await
        .expect("send first delta");
        stream.next().await.expect("first delta forwarded").expect("delta is not an error");

        tokio::time::sleep(wait_before_end).await;
        tx.send(LlmStreamChunk {
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        })
        .await
        .expect("send terminal chunk");
        drop(tx);
        while stream.next().await.is_some() {}

        (dir, events_pool, log)
    }

    /// Waits until `run_id` holds a row of `kind`, so an assertion never races
    /// the writer task.
    async fn await_kind(pool: &DbPool, run_id: &str, kind: crate::events::EventKind) {
        for _ in 0..300 {
            let rows = read_run(pool, run_id, 0, 1000).expect("read run");
            if rows.iter().any(|r| r.kind == kind) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for a {} row of run {run_id}", kind.slug());
    }

    /// §2.11 stage 3 — TTFT is COMPUTED from the stored rows and checked against
    /// the wait the run actually contained. 220 ms of silence before the first
    /// delta has to come back out of the database as ~220 ms.
    #[tokio::test]
    async fn ttft_is_computed_from_the_stored_events() {
        let (dir, pool, log) = streaming_run(
            "run-ttft",
            Duration::from_millis(220),
            Duration::from_millis(40),
        )
        .await;
        await_kind(&pool, "run-ttft", crate::events::EventKind::FirstToken).await;

        let steps = step_latencies(&pool, "run-ttft").expect("latencies");
        let step = steps
            .iter()
            .find(|s| s.node_id == "p")
            .expect("the streaming node has a first token");
        assert_eq!(step.step, "metrics_probe");
        assert!(
            (200..600).contains(&step.ttft_ms),
            "TTFT {} ms is not the ~220 ms the run waited",
            step.ttft_ms
        );
        log.stop();
        drop(dir);
    }

    /// §2.11 stage 3 — decode time, likewise computed from the log. The stream
    /// stayed open 260 ms after its first token, and that is what the two rows
    /// have to say.
    #[tokio::test]
    async fn decode_time_is_computed_from_the_stored_events() {
        let (dir, pool, log) = streaming_run(
            "run-decode",
            Duration::from_millis(40),
            Duration::from_millis(260),
        )
        .await;
        await_kind(&pool, "run-decode", crate::events::EventKind::StepEnd).await;

        let steps = step_latencies(&pool, "run-decode").expect("latencies");
        let step = steps
            .iter()
            .find(|s| s.node_id == "p")
            .expect("the streaming node has a first token");
        let decode = step.decode_ms.expect("the step closed, so decoding has an end");
        assert!(
            (240..700).contains(&decode),
            "decode {decode} ms is not the ~260 ms the stream stayed open"
        );
        log.stop();
        drop(dir);
    }

    /// Drives the progress stream directly for the tool tests: the harness that
    /// emits `ToolCallStarted` needs a live agent service, and what is under test
    /// is the pairing, not the harness.
    struct ToolRun {
        _dir: tempfile::TempDir,
        pool: DbPool,
        sink: BrokerProgressSink,
        log: RunEventLog,
    }

    impl ToolRun {
        fn start(run_id: &str) -> Self {
            let (dir, pool) = crate::events::test_support::events_db();
            let broker = Arc::new(ProgressBroker::new());
            broker.bind_run_provenance(SCOPE, provenance(run_id));
            let log = RunEventLog::new(pool.clone(), broker.clone());
            log.attach(SCOPE);
            Self {
                _dir: dir,
                pool,
                sink: BrokerProgressSink::new(broker),
                log,
            }
        }

        fn emit(&self, event: ProgressEvent) {
            self.sink.emit(SCOPE, event);
        }

        async fn await_rows(&self, run_id: &str, count: usize) {
            for _ in 0..300 {
                if read_run(&self.pool, run_id, 0, 1000).expect("read run").len() >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("timed out waiting for {count} rows of run {run_id}");
        }
    }

    impl Drop for ToolRun {
        fn drop(&mut self) {
            self.log.stop();
        }
    }

    /// §2.7 — two calls of the SAME tool overlap and finish in the opposite
    /// order to the one they started in. Paired by `call_id` the durations are
    /// ~240 ms and ~30 ms; paired by name they would be ~60 ms and ~210 ms —
    /// two numbers neither call ever had. The assertions exclude the name-paired
    /// values on purpose.
    #[tokio::test]
    async fn concurrent_calls_of_one_tool_pair_by_call_id() {
        let run = ToolRun::start("run-tools");
        run.emit(ProgressEvent::IterationStarted {
            node_id: "loop".into(),
            n: 1,
            max: 4,
        });
        run.emit(ProgressEvent::ToolCallStarted {
            call_id: "call-slow".into(),
            name: "search".into(),
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        run.emit(ProgressEvent::ToolCallStarted {
            call_id: "call-fast".into(),
            name: "search".into(),
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        run.emit(ProgressEvent::ToolCallFinished {
            call_id: "call-fast".into(),
            name: "search".into(),
            status: "ok".into(),
        });
        tokio::time::sleep(Duration::from_millis(210)).await;
        run.emit(ProgressEvent::ToolCallFinished {
            call_id: "call-slow".into(),
            name: "search".into(),
            status: "ok".into(),
        });
        run.emit(ProgressEvent::IterationFinished {
            node_id: "loop".into(),
            n: 1,
        });
        run.await_rows("run-tools", 6).await;

        let durations = tool_durations(&run.pool, "run-tools").expect("durations");
        assert_eq!(durations.len(), 2, "both calls closed: {durations:?}");
        assert!(
            durations.iter().all(|d| d.tool == "search"),
            "both calls are the same tool: {durations:?}"
        );
        let slow = durations
            .iter()
            .find(|d| d.call_id == "call-slow")
            .expect("the slow call");
        let fast = durations
            .iter()
            .find(|d| d.call_id == "call-fast")
            .expect("the fast call");
        assert!(
            (200..500).contains(&slow.ms),
            "the slow call ran ~240 ms, not {} ms — name pairing would say ~60",
            slow.ms
        );
        assert!(
            (10..120).contains(&fast.ms),
            "the fast call ran ~30 ms, not {} ms — name pairing would say ~210",
            fast.ms
        );
    }

    /// §2.7 — a call that never came back is EXCLUDED when the turn closes. Not
    /// zero, not "still running": one failure that counted as either would sit
    /// in that tool's statistics forever.
    #[tokio::test]
    async fn an_unclosed_call_is_excluded_at_turn_end() {
        let run = ToolRun::start("run-open");
        run.emit(ProgressEvent::IterationStarted {
            node_id: "loop".into(),
            n: 1,
            max: 4,
        });
        run.emit(ProgressEvent::ToolCallStarted {
            call_id: "call-closed".into(),
            name: "search".into(),
        });
        run.emit(ProgressEvent::ToolCallStarted {
            call_id: "call-lost".into(),
            name: "search".into(),
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        run.emit(ProgressEvent::ToolCallFinished {
            call_id: "call-closed".into(),
            name: "search".into(),
            status: "ok".into(),
        });
        run.emit(ProgressEvent::IterationFinished {
            node_id: "loop".into(),
            n: 1,
        });
        // The lost call answers only after its turn has closed; the answer is
        // too late to count.
        tokio::time::sleep(Duration::from_millis(30)).await;
        run.emit(ProgressEvent::ToolCallFinished {
            call_id: "call-lost".into(),
            name: "search".into(),
            status: "ok".into(),
        });
        run.await_rows("run-open", 6).await;

        let durations = tool_durations(&run.pool, "run-open").expect("durations");
        assert_eq!(
            durations.len(),
            1,
            "only the call that closed inside its turn counts: {durations:?}"
        );
        assert_eq!(durations[0].call_id, "call-closed");
        assert!(
            !durations.iter().any(|d| d.call_id == "call-lost"),
            "the unclosed call must be absent, not zero and not open-ended"
        );
    }
}
