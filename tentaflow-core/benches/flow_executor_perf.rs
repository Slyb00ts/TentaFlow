// =============================================================================
// File: benches/flow_executor_perf.rs — narzut orkiestracji flow engine.
// =============================================================================
//
// Mierzy WYŁĄCZNIE koszt naszej części flow: dependency scheduler, build_inputs,
// spawn per node (JoinSet), Arc wrapping, trace, usage attribution. Adapter
// `noop` zwraca natychmiast (zero pracy LLM/STT/TTS), więc czas to czysty narzut
// silnika per node. Cel: "blazing fast" — narzut na node ma być rzędu
// pojedynczych mikrosekund i ma SKALOWAĆ się liniowo z liczbą nodów (fan-out
// dodaje współbieżność, nie kwadrat).
//
// Trzy profile:
//   flow_executor/linear  — łańcuch N nodów (scheduler: 1 gotowy naraz).
//   flow_executor/fanout  — trigger → N gałęzi → combine (równoległy spawn +
//                           bariera). Sprawdza że szeroki fan-out nie degraduje.
//   flow_compile          — koszt CompiledFlow::from_json (toposort + walidacja +
//                           adjacency); cache-miss path, normalnie zcache'owany.
//
// Run: `cargo bench --features test-support --bench flow_executor_perf -- --quick`
// Asm hot-path:  patrz komentarz na końcu pliku.

use std::hint::black_box;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rusqlite::Connection;

use tentaflow_core::db::DbPool;
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::{
    test_support::stub_ctx, AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec,
};
use tentaflow_core::flow_engine::node_adapters::{
    CombineNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter,
};
use tentaflow_core::flow_engine::types::{FlowDataType, FlowNode};

/// Adapter bez pracy — zwraca Text(node.id) natychmiast. Izoluje narzut silnika
/// od kosztu adaptera.
struct NoopAdapter;
#[async_trait]
impl NodeAdapter for NoopAdapter {
    fn node_type(&self) -> &str {
        "noop"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }
    async fn execute(
        &self,
        node: &FlowNode,
        _inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        Ok(FlowEnvelope::with_payload(FlowValue::Text(node.id.clone())))
    }
}

fn registry() -> Arc<AdapterRegistry> {
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(TriggerNodeAdapter::new()));
    r.register(Arc::new(OutputNodeAdapter::new()));
    r.register(Arc::new(CombineNodeAdapter::new()));
    r.register(Arc::new(NoopAdapter));
    Arc::new(r)
}

// flow_id=0 → executor pomija create_execution_record + persist, więc DB nigdy
// nie jest dotykana; pusta in-memory wystarcza i nie zaśmieca pomiaru I/O.
fn db() -> DbPool {
    Arc::new(Mutex::new(Connection::open_in_memory().expect("mem db")))
}

/// Łańcuch: trigger → a0 → a1 → … → a{n-1} → output.
fn linear_json(n: usize) -> String {
    let mut nodes = String::from(r#"{"id":"t","type":"trigger","config":{}}"#);
    for i in 0..n {
        nodes.push_str(&format!(r#",{{"id":"a{i}","type":"noop","config":{{}}}}"#));
    }
    nodes.push_str(r#",{"id":"o","type":"output","config":{}}"#);

    let mut edges = String::from(r#"{"from":"t","to":"a0","from_port":"text","to_port":"in"}"#);
    for i in 0..n - 1 {
        edges.push_str(&format!(
            r#",{{"from":"a{i}","to":"a{}","from_port":"full","to_port":"in"}}"#,
            i + 1
        ));
    }
    edges.push_str(&format!(
        r#",{{"from":"a{}","to":"o","from_port":"full","to_port":"text"}}"#,
        n - 1
    ));
    format!(r#"{{"nodes":[{nodes}],"edges":[{edges}]}}"#)
}

/// Fan-out: trigger → b0..b{n-1} (równolegle) → combine → output.
fn fanout_json(n: usize) -> String {
    let mut nodes = String::from(r#"{"id":"t","type":"trigger","config":{}}"#);
    for i in 0..n {
        nodes.push_str(&format!(r#",{{"id":"b{i}","type":"noop","config":{{}}}}"#));
    }
    nodes.push_str(r#",{"id":"c","type":"combine","config":{}},{"id":"o","type":"output","config":{}}"#);

    let mut edges = String::new();
    for i in 0..n {
        if i > 0 {
            edges.push(',');
        }
        edges.push_str(&format!(
            r#"{{"from":"t","to":"b{i}","from_port":"text","to_port":"in"}}"#
        ));
    }
    for i in 0..n {
        edges.push_str(&format!(
            r#",{{"from":"b{i}","to":"c","from_port":"full","to_port":"in"}}"#
        ));
    }
    edges.push_str(r#",{"from":"c","to":"o","from_port":"full","to_port":"text"}"#);
    format!(r#"{{"nodes":[{nodes}],"edges":[{edges}]}}"#)
}

const SIZES: [usize; 3] = [10, 50, 200];

fn bench(c: &mut Criterion) {
    // No-op path nie używa timerów ani IO — wystarczy scheduler tasków (`rt`).
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("rt");
    let reg = registry();
    let db = db();

    let mut linear = c.benchmark_group("flow_executor/linear");
    for n in SIZES {
        let compiled = Arc::new(CompiledFlow::from_json(0, &linear_json(n), &reg).expect("compile"));
        linear.throughput(Throughput::Elements(n as u64 + 2));
        linear.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let out = rt
                    .block_on(execute_blocking(
                        db.clone(),
                        compiled.clone(),
                        FlowEnvelope::empty(),
                        stub_ctx(),
                        reg.clone(),
                    ))
                    .expect("exec");
                black_box(out.trace.len());
            });
        });
    }
    linear.finish();

    let mut fanout = c.benchmark_group("flow_executor/fanout");
    for n in SIZES {
        let compiled = Arc::new(CompiledFlow::from_json(0, &fanout_json(n), &reg).expect("compile"));
        fanout.throughput(Throughput::Elements(n as u64 + 3));
        fanout.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let out = rt
                    .block_on(execute_blocking(
                        db.clone(),
                        compiled.clone(),
                        FlowEnvelope::empty(),
                        stub_ctx(),
                        reg.clone(),
                    ))
                    .expect("exec");
                black_box(out.trace.len());
            });
        });
    }
    fanout.finish();

    let mut comp = c.benchmark_group("flow_compile");
    for n in SIZES {
        let json = linear_json(n);
        comp.throughput(Throughput::Elements(n as u64 + 2));
        comp.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let compiled = CompiledFlow::from_json(0, &json, &reg).expect("compile");
                black_box(compiled.execution_order.len());
            });
        });
    }
    comp.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

// Asm hot-path (synchroniczny rdzeń schedulera — to tam ma być ciasno):
//   cargo install cargo-show-asm
//   cargo asm --features test-support --bench flow_executor_perf \
//     "tentaflow_core::flow_engine::executor::build_dependency_graph"
//   cargo asm ... "tentaflow_core::flow_engine::executor::build_inputs"
//   cargo asm ... "tentaflow_core::flow_engine::cache::topological_sort"
// Czego szukać: brak `call __rust_alloc` w wewnętrznych pętlach poza
// jednorazowymi Vec/HashSet rezerwacjami; brak `panic`/bounds-check w gorącej
// pętli iterowania krawędzi (indeksy pochodzą z toposortu, są w zakresie).
