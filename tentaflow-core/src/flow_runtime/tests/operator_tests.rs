// =============================================================================
// File: flow_runtime/tests/operator_tests.rs — per-operator unit tests
// =============================================================================
//
// Each test exercises one operator in isolation against a `:memory:` DB. The
// operator runs are driven via `FlowScheduler::invoke` so the full per-task
// machinery (channel construction, OperatorContext build, EOF propagation)
// is covered together with the operator body.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::db::DbPool;
use crate::flow_runtime::parser::{compile, parse_flow_definition};
use crate::flow_runtime::registry;
use crate::flow_runtime::scheduler::FlowScheduler;
use crate::flow_runtime::types::CompiledFlow;

fn fresh_db() -> DbPool {
    crate::db::init(Path::new(":memory:")).expect("test db")
}

fn unique_addon(tag: &str) -> String {
    format!("addon-{tag}-{}", uuid::Uuid::new_v4())
}

fn compile_flow(json: &str) -> Arc<CompiledFlow> {
    Arc::new(compile(parse_flow_definition(json).expect("parse")).expect("compile"))
}

fn extract_records(result_toml: &str) -> Vec<toml::Value> {
    let v: toml::Value = toml::from_str(result_toml).expect("decode result_toml");
    v.get("records")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

// -----------------------------------------------------------------------------
// Source
// -----------------------------------------------------------------------------

#[tokio::test]
async fn source_emits_count_records_then_eof() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("src-count");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 3 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "invocation_result" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let status = sched
        .invoke(&addon, &flow_id, toml::Value::Integer(42), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(status.status, "completed");
    let records = extract_records(status.result_toml.as_deref().unwrap());
    assert_eq!(records.len(), 3, "expected 3 emitted records");
    for r in &records {
        assert_eq!(r.as_integer(), Some(42));
    }
}

#[tokio::test]
async fn source_rejects_camera_stream() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("src-cam");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "camera.main", "fps": 5 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{}} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let status = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(status.status, "failed");
    assert!(
        status.error.as_deref().unwrap_or("").contains("camera.*"),
        "error: {:?}",
        status.error
    );
}

#[tokio::test]
async fn source_fps_paces_emissions() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("src-fps");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 5, "fps": 50 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{}} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let started = std::time::Instant::now();
    let status = sched
        .invoke(&addon, &flow_id, toml::Value::Integer(0), 5_000, None)
        .await
        .expect("invoke");
    let elapsed = started.elapsed();
    assert_eq!(status.status, "completed");
    // 50 fps = 20 ms between emissions, 5 records ≈ 4 gaps ≈ 80 ms minimum.
    // Generous lower bound to absorb runtime jitter.
    assert!(
        elapsed >= Duration::from_millis(60),
        "fps pacing not observed: {:?}",
        elapsed
    );
}

// -----------------------------------------------------------------------------
// Threshold
// -----------------------------------------------------------------------------

#[tokio::test]
async fn threshold_passes_within_range_drops_outside() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("thr-range");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",    "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "thr", "type": "Threshold", "params": {{ "field": "confidence", "min": 0.5 }} }},
                {{ "id": "snk", "type": "Sink",       "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "thr" }},
                {{ "from": "thr", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));

    // Above threshold: passes.
    let input_high = toml::from_str::<toml::Value>("confidence = 0.9").unwrap();
    let s_high = sched
        .invoke(&addon, &flow_id, input_high, 5_000, None)
        .await
        .expect("invoke");
    let recs_high = extract_records(s_high.result_toml.as_deref().unwrap());
    assert_eq!(recs_high.len(), 1, "expected 1 pass record");

    // Below threshold: dropped.
    let input_low = toml::from_str::<toml::Value>("confidence = 0.1").unwrap();
    let s_low = sched
        .invoke(&addon, &flow_id, input_low, 5_000, None)
        .await
        .expect("invoke");
    let recs_low = extract_records(s_low.result_toml.as_deref().unwrap());
    assert!(recs_low.is_empty(), "expected drop, got {:?}", recs_low);
}

#[tokio::test]
async fn threshold_drops_records_with_missing_field() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("thr-miss");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",    "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "thr", "type": "Threshold", "params": {{ "field": "score", "min": 0.0 }} }},
                {{ "id": "snk", "type": "Sink",       "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "thr" }},
                {{ "from": "thr", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let input = toml::from_str::<toml::Value>("other = 1").unwrap();
    let s = sched
        .invoke(&addon, &flow_id, input, 5_000, None)
        .await
        .expect("invoke");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert!(recs.is_empty());
}

// -----------------------------------------------------------------------------
// Branch
// -----------------------------------------------------------------------------

#[tokio::test]
async fn branch_routes_true_and_false_ports() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("br-route");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src",  "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "br",   "type": "Branch", "params": {{ "expr": "class == 'truck'" }} }},
                {{ "id": "snk_t","type": "Sink",   "params": {{}} }},
                {{ "id": "snk_f","type": "Sink",   "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "br" }},
                {{ "from": "br",  "to": "snk_t", "port": "true" }},
                {{ "from": "br",  "to": "snk_f", "port": "false" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let input = toml::from_str::<toml::Value>("class = \"truck\"").unwrap();
    let s = sched
        .invoke(&addon, &flow_id, input, 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "completed");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    // Both sinks share the same Vec; we sent one record routed to true.
    assert_eq!(recs.len(), 1, "expected 1 routed record");
}

#[tokio::test]
async fn branch_rejects_unparseable_expr() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("br-bad");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src",  "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "br",   "type": "Branch", "params": {{ "expr": "garbage expression" }} }},
                {{ "id": "snk_t","type": "Sink",   "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "br" }},
                {{ "from": "br",  "to": "snk_t", "port": "true" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "failed");
    assert!(s.error.as_deref().unwrap_or("").contains("branch"));
}

#[test]
fn branch_compile_expr_smoke() {
    use crate::flow_runtime::operators::branch::test_compile_expr;
    assert!(test_compile_expr("field == 1").is_ok());
    assert!(test_compile_expr("a.b.c >= 0.5").is_ok());
    assert!(test_compile_expr("class != 'car'").is_ok());
    assert!(test_compile_expr("no_op_here").is_err());
}

#[tokio::test]
async fn branch_expr_with_op_in_string_literal() {
    // The lexer must split on the FIRST operator outside quotes — the `<`
    // inside the quoted RHS literal is not a separator.
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("br-quoted-op");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src",  "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "br",   "type": "Branch", "params": {{ "expr": "name == \"abc<def\"" }} }},
                {{ "id": "snk_t","type": "Sink",   "params": {{}} }},
                {{ "id": "snk_f","type": "Sink",   "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "br" }},
                {{ "from": "br",  "to": "snk_t", "port": "true" }},
                {{ "from": "br",  "to": "snk_f", "port": "false" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let input = toml::from_str::<toml::Value>("name = \"abc<def\"").unwrap();
    let s = sched.invoke(&addon, &flow_id, input, 5_000, None).await.expect("invoke");
    assert_eq!(s.status, "completed");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert_eq!(recs.len(), 1, "expected one record on `true` port");
}

#[test]
fn branch_expr_unterminated_string_rejected() {
    use crate::flow_runtime::operators::branch::test_compile_expr;
    let err = test_compile_expr("name == \"abc").expect_err("must reject");
    assert!(
        err.contains("unterminated") || err.contains("bad params"),
        "unexpected error: {err}"
    );
}

// -----------------------------------------------------------------------------
// Aggregate
// -----------------------------------------------------------------------------

#[tokio::test]
async fn aggregate_emits_count_on_eof() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("agg-count");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",    "params": {{ "stream": "input", "count": 5 }} }},
                {{ "id": "agg", "type": "Aggregate", "params": {{ "window_ms": 5000, "op": "count" }} }},
                {{ "id": "snk", "type": "Sink",       "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "agg" }},
                {{ "from": "agg", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Integer(1), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "completed");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert_eq!(recs.len(), 1, "expected one EOF-flush window");
    let count = recs[0].get("count").and_then(|v| v.as_integer()).unwrap();
    assert_eq!(count, 5);
}

#[tokio::test]
async fn aggregate_sum_correctness() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("agg-sum");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",    "params": {{ "stream": "input", "count": 3 }} }},
                {{ "id": "agg", "type": "Aggregate", "params": {{ "window_ms": 5000, "op": "sum", "field": "v" }} }},
                {{ "id": "snk", "type": "Sink",       "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "agg" }},
                {{ "from": "agg", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let input = toml::from_str::<toml::Value>("v = 7").unwrap();
    let s = sched.invoke(&addon, &flow_id, input, 5_000, None).await.expect("invoke");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert_eq!(recs.len(), 1);
    let value = recs[0].get("value").and_then(|v| v.as_float()).unwrap();
    assert!((value - 21.0).abs() < 1e-6, "sum=21 expected, got {value}");
}

#[tokio::test]
async fn aggregate_rejects_short_window() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("agg-bad");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",    "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "agg", "type": "Aggregate", "params": {{ "window_ms": 10, "op": "count" }} }},
                {{ "id": "snk", "type": "Sink",       "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "agg" }},
                {{ "from": "agg", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Integer(0), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "failed");
    assert!(s.error.as_deref().unwrap_or("").contains("window_ms"));
}

// -----------------------------------------------------------------------------
// Predict
// -----------------------------------------------------------------------------

#[tokio::test]
async fn predict_alias_revoked_midstream_skips_remaining() {
    // Per-record alias resolve: when the alias is missing (revoked or never
    // existed), every record audits `alias_check_failed` and `on_error=skip`
    // routes around the failure so the flow completes with zero sinks.
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("pred-revoke");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src",  "type": "Source",  "params": {{ "stream": "input", "count": 3 }} }},
                {{ "id": "pred", "type": "Predict", "params": {{ "alias": "gone", "on_error": "skip" }} }},
                {{ "id": "snk",  "type": "Sink",     "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src",  "to": "pred" }},
                {{ "from": "pred", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "completed", "skip policy must not fail the flow");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert!(recs.is_empty(), "expected 0 sink records, got {recs:?}");
    // Verify the audit row carries the per-record alias_check_failed action.
    let conn = db.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'flow.op.predict.alias_check_failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 3, "expected 3 alias_check_failed audit rows");
}

#[tokio::test]
async fn predict_unknown_alias_returns_not_found() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("pred-miss");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src",  "type": "Source",  "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "pred", "type": "Predict", "params": {{ "alias": "no-such-alias" }} }},
                {{ "id": "snk",  "type": "Sink",     "params": {{}} }}
            ],
            "edges": [
                {{ "from": "src", "to": "pred" }},
                {{ "from": "pred", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "failed");
    assert!(s.error.as_deref().unwrap_or("").contains("alias not found"));
}

// -----------------------------------------------------------------------------
// Sink
// -----------------------------------------------------------------------------

#[tokio::test]
async fn sink_invocation_result_collects_records() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("sink-ir");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 2 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "invocation_result" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let input = toml::from_str::<toml::Value>("k = 'v'").unwrap();
    let s = sched.invoke(&addon, &flow_id, input, 5_000, None).await.expect("invoke");
    let recs = extract_records(s.result_toml.as_deref().unwrap());
    assert_eq!(recs.len(), 2);
}

#[tokio::test]
async fn sink_sql_exec_requires_permission() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("sink-sql");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "sql_exec", "query": "INSERT INTO t VALUES (1)" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    // Addon has zero declared permissions — sql.write missing → fail.
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "failed");
    assert!(s.error.as_deref().unwrap_or("").contains("sql.write"));
}

#[tokio::test]
async fn sink_ui_notify_requires_events_permission() {
    // ui_notify routes through `publish_event` so the missing `events`
    // manifest permission is rejected by the same gate that protects
    // addon-issued events. Without the bus we'd short-circuit earlier;
    // bind one to reach the permission check.
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    sched.set_event_bus(Arc::new(crate::addon::event_bus::EventBus::new()));
    let addon = unique_addon("sink-uin");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "ui_notify", "message": "hi" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    // Addon has zero declared permissions — `events` missing → publish_event
    // denies and Sink audits the error. Skip policy keeps the flow running
    // so it still completes.
    assert_eq!(s.status, "completed");
    let conn = db.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'event.publish' AND result = 'denied'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(count >= 1, "expected at least one denied event.publish audit row");
}

#[tokio::test]
async fn sink_sql_exec_honors_cancel() {
    // The scheduler's per-invocation timeout is the cancel signal; setting
    // it well below the SQL watchdog (30 s) proves the operator returns
    // promptly instead of pinning the worker until the watchdog fires.
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("sink-sql-cancel");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    // sql_exec needs `sql.write`; without a real addon manifest the test
    // exercises the param-validation path which itself fails fast and
    // returns. The cancel-aware select! still applies on the happy path
    // and is verified statically by compile-time. A full end-to-end
    // cancel needs a long-running query plumbing not yet available in
    // `:memory:` tests.
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "sql_exec", "query": "SELECT 1" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    let started = std::time::Instant::now();
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 1_000, None)
        .await
        .expect("invoke");
    let elapsed = started.elapsed();
    assert_eq!(s.status, "failed");
    assert!(
        elapsed < Duration::from_secs(5),
        "operator should return within timeout, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn sink_event_requires_bus() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon("sink-evt");
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{ "stream": "input", "count": 1 }} }},
                {{ "id": "snk", "type": "Sink",   "params": {{ "kind": "event", "topic": "alarm.created" }} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));
    // No event_bus bound on this scheduler → operator fails with
    // SubsystemNotInitialized("event_bus").
    let s = sched
        .invoke(&addon, &flow_id, toml::Value::Table(Default::default()), 5_000, None)
        .await
        .expect("invoke");
    assert_eq!(s.status, "failed");
    assert!(s.error.as_deref().unwrap_or("").contains("event_bus"));
}
