// ===== File: examples/bus_e2e_bench.rs — TentaBus cross-process p99 =====
// ===== publish->consume harness (SUM/tentabus/PLAN.md §5.4 item 5 /       =====
// ===== OTWARTE-POZYCJE.md `bus-e2e-bench-example-missing`)                =====
//
// `benches/bus_path.rs`'s P4 gate measures publish->consume latency
// IN-PROCESS: one producer thread and one consumer thread inside the same
// `BusService`, same address space, same scheduler — a real number, but not
// what PLAN §5.4 item 5 actually asked for ("miedzyprocesowy pomiar p99
// publish->consume"). This file is that missing cross-process harness.
//
// WHY NOT TWO PROCESSES SHARING ONE ON-DISK `bus_dir` DIRECTLY: that would
// be the simplest possible design, but it does not work — `Partition::open`
// (`tentaflow-bus/src/partition.rs:1522`) takes an EXCLUSIVE advisory flock
// on the partition directory for the engine's entire lifetime (the same
// single-writer-per-partition invariant `CLAUDE.md`'s Build & Run section
// documents), so a second process opening the same `bus_dir` fails fast
// instead of sharing it. Production only ever has ONE process own a given
// `BusService`; every other process/client reaches it over the network
// (Tier 1 binary protocol or Tier 2 REST, per `CLAUDE.md`'s transport
// section) — never by mapping the same log files from two processes.
//
// DESIGN: this binary plays THREE roles depending on `BUS_E2E_ROLE`
// (unset/coordinator, `producer`, `consumer`), all built from the SAME
// `cargo build --example bus_e2e_bench` artifact — the coordinator
// re-execs `std::env::current_exe()` with that env var set, mirroring the
// self-reexec pattern `tests/process_three_node_bus_failover.rs` /
// `tests/process_four_node_sync.rs` use to spawn real OS processes of the
// same test binary (their own module docs call this out explicitly). Here:
//   - the COORDINATOR (no env var) owns the one real `BusService` instance
//     (same construction as `benches/support/mod.rs::bench_world`, inlined
//     — an example cannot depend on a bench's `mod support`) and creates
//     the topic, then listens on a loopback TCP socket and acts as a tiny
//     RPC hub: `PUB <timestamp_ms>` -> one `svc.publish` call -> `ACK`;
//     `FETCH` -> one `handle.fetch(...)` long-poll call -> zero or more
//     `REC <timestamp_ms>` lines, then `END`.
//   - the PRODUCER child connects once and, for every message, captures
//     `now_ms()` **before** sending `PUB <ts>` — that timestamp is what the
//     engine stores as the record's `timestamp_ms` (PLAN §6.1's own field),
//     so the measured window covers the full producer->coordinator IPC hop,
//     not just the in-process `publish` call.
//   - the CONSUMER child connects once and long-polls `FETCH` in a loop;
//     for every `REC <ts>` line it computes `now_ms() - ts` the INSTANT it
//     parses the line — covering the full engine-fetch->coordinator->
//     consumer IPC hop on the receiving side too. It prints one
//     `RESULT n=.. p50_ms=.. p99_ms=.. p999_ms=.. mean_ms=..` line to stdout
//     when done; the coordinator captures that line from the child's piped
//     stdout and re-prints it as this run's answer.
// Same millisecond-resolution caveat as `bus_path.rs`'s P4 gate (PublishRecord::
// timestamp_ms/FetchedRecordMeta::timestamp_ms are millisecond fields) — fine
// for a cross-process number whose whole point is IPC/scheduling overhead,
// which dwarfs 1 ms of clock quantization.
//
// Usage: `cargo run -p tentaflow-core --example bus_e2e_bench --release`
// (single-machine loopback TCP; the coordinator's temp dir/db/bus_dir are
// cleaned up automatically on exit).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tentaflow_core::bus::{
    self, groups, quota, topics, BusAction, BusCallContext, BusInitConfig, BusService,
    BusServiceError, ConsumerConfig, PublishBatch, PublishRecord,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::org::DEFAULT_ORG_ID;

const ROLE_ENV: &str = "BUS_E2E_ROLE";
const ADDR_ENV: &str = "BUS_E2E_ADDR";
const TOTAL_ENV: &str = "BUS_E2E_TOTAL";
const TOPIC: &str = "bus.e2e.bench";
/// Small record — this harness measures LATENCY, not throughput (that is
/// `bus_path.rs`'s P1). Payload content is irrelevant to the measurement.
const RECORD_PAYLOAD_BYTES: usize = 256;
/// Modest total so a default `cargo run --release` finishes in well under a
/// minute even on a host shared with other agents' builds — this is a
/// harness/example, not a CI gate with its own pass/fail threshold.
const TOTAL_MESSAGES: usize = 2_000;
/// Per-`FETCH` long-poll budget on the coordinator's `handle.fetch` call —
/// large enough that a slow producer does not force the consumer into a
/// busy-loop of empty `FETCH`/`END` round trips, small enough that the
/// consumer notices "done" promptly once the producer's last record lands.
const FETCH_LONG_POLL_MS: u32 = 100;
const FETCH_MAX_BYTES: usize = 4 * 1024 * 1024;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

/// Deterministic filler so the payload isn't a single repeated byte
/// (irrelevant for this latency measurement, but avoids the record looking
/// like an uninitialized/degenerate buffer to anyone eyeballing a dump).
fn record_payload() -> Bytes {
    let mut buf = Vec::with_capacity(RECORD_PAYLOAD_BYTES);
    let mut state: u64 = 0x9E3779B97F4A7C15;
    while buf.len() < RECORD_PAYLOAD_BYTES {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        buf.extend_from_slice(&z.to_le_bytes());
    }
    buf.truncate(RECORD_PAYLOAD_BYTES);
    Bytes::from(buf)
}

/// Allow-all authorizer — this harness measures the engine/IPC path, not
/// RBAC (same pattern `benches/support/mod.rs::AllowAllAuthorizer` and
/// `tests/bus_demo_seed.rs` use).
struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }
    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }
    fn generation(&self) -> u64 {
        0
    }
}

fn unlimited_quota() -> quota::QuotaConfig {
    quota::QuotaConfig {
        max_topics: 10_000,
        max_partitions: 100_000,
        max_bytes_total: u64::MAX,
        produce_msgs_per_sec: 0,
        produce_bytes_per_sec: 0,
        max_groups: 10_000,
    }
}

fn main() -> anyhow::Result<()> {
    match std::env::var(ROLE_ENV).ok().as_deref() {
        Some("producer") => run_producer(),
        Some("consumer") => run_consumer(),
        Some(other) => anyhow::bail!("bus_e2e_bench: unknown {ROLE_ENV}={other:?}"),
        None => run_coordinator(),
    }
}

// ===== Coordinator: owns the real BusService, acts as the RPC hub =====

fn run_coordinator() -> anyhow::Result<()> {
    println!("=== bus_e2e_bench: cross-process publish->consume (n={TOTAL_MESSAGES}) ===");

    let tmp = tempfile::Builder::new()
        .prefix("tentaflow-bus-e2e-bench-")
        .tempdir()?;
    let db_dir = tmp.path().join("data");
    std::fs::create_dir_all(&db_dir)?;
    let db_path = db_dir.join("tentaflow.db");
    let db = tentaflow_core::db::init(&db_path).map_err(|e| anyhow::anyhow!("db init: {e}"))?;
    let bus_dir = tmp.path().join("bus");

    let local_conn = rusqlite::Connection::open_in_memory()?;
    bus::db::migrate(&local_conn).map_err(|e| anyhow::anyhow!("local db migrate: {e}"))?;
    let local_db: DbPool = Arc::new(tentaflow_core::db::Db::from_connection(local_conn));

    let svc = Arc::new(
        BusService::new(BusInitConfig {
            instance_id: bus::instance::BusInstanceId::parse("tentabus-e2eb0001")
                .expect("valid instance id"),
            local_db,
            bus_dir,
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .map_err(|e| anyhow::anyhow!("BusService::new: {e}"))?,
    );
    svc.quota().set_org_quota(DEFAULT_ORG_ID, unlimited_quota());

    let ctx = BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id: DEFAULT_ORG_ID.to_string(),
        actor: Some("bus-e2e-bench".to_string()),
        correlation_id: None,
        origin: "bus_e2e_bench".to_string(),
    };

    svc.create_topic(
        &ctx,
        TOPIC,
        topics::TopicOptions {
            partitions: Some(1),
            ..Default::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("create_topic: {e}"))?;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    println!("coordinator: listening on {addr}, bus_dir={}", tmp.path().display());

    let exe = std::env::current_exe()?;
    let producer_child = Command::new(&exe)
        .env(ROLE_ENV, "producer")
        .env(ADDR_ENV, addr.to_string())
        .env(TOTAL_ENV, TOTAL_MESSAGES.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let consumer_child = Command::new(&exe)
        .env(ROLE_ENV, "consumer")
        .env(ADDR_ENV, addr.to_string())
        .env(TOTAL_ENV, TOTAL_MESSAGES.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    // Two connections are expected (producer, consumer); role is decided by
    // each connection's own `HELLO <role>` line, not by accept order.
    let served = Arc::new(AtomicUsize::new(0));
    let mut handler_threads = Vec::with_capacity(2);
    for _ in 0..2 {
        let (stream, _) = listener.accept()?;
        let svc = Arc::clone(&svc);
        let ctx = ctx.clone();
        let served = Arc::clone(&served);
        handler_threads.push(std::thread::spawn(move || {
            serve_connection(&svc, &ctx, stream);
            served.fetch_add(1, Ordering::Relaxed);
        }));
    }

    let producer_output = producer_child.wait_with_output()?;
    let consumer_output = consumer_child.wait_with_output()?;
    for h in handler_threads {
        let _ = h.join();
    }

    if !producer_output.status.success() {
        anyhow::bail!(
            "producer child exited with {:?}; stdout={}",
            producer_output.status,
            String::from_utf8_lossy(&producer_output.stdout)
        );
    }
    if !consumer_output.status.success() {
        anyhow::bail!(
            "consumer child exited with {:?}; stdout={}",
            consumer_output.status,
            String::from_utf8_lossy(&consumer_output.stdout)
        );
    }

    let consumer_stdout = String::from_utf8_lossy(&consumer_output.stdout).into_owned();
    let result_line = consumer_stdout
        .lines()
        .find(|l| l.starts_with("RESULT "))
        .ok_or_else(|| {
            anyhow::anyhow!("consumer child produced no RESULT line; stdout={consumer_stdout}")
        })?;

    println!("{result_line}");
    println!("=== bus_e2e_bench: done ===");
    Ok(())
}

/// Reads the connection's `HELLO <role>` line and dispatches to the
/// matching serve loop. Never panics on a protocol error — a malformed
/// child just makes this connection return early, and the coordinator's
/// own exit-status checks on both children report the real failure.
fn serve_connection(svc: &Arc<BusService>, ctx: &BusCallContext, stream: TcpStream) {
    if let Err(e) = stream.set_nodelay(true) {
        eprintln!("coordinator: set_nodelay failed: {e}");
    }
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("coordinator: clone stream failed: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(stream);

    let mut hello = String::new();
    if reader.read_line(&mut hello).unwrap_or(0) == 0 {
        eprintln!("coordinator: connection closed before HELLO");
        return;
    }
    let role = hello.trim().strip_prefix("HELLO ").unwrap_or("").to_string();

    match role.as_str() {
        "producer" => serve_producer(svc, ctx, &mut reader, &mut writer),
        "consumer" => serve_consumer(svc, ctx, &mut reader, &mut writer),
        other => eprintln!("coordinator: unknown role {other:?}"),
    }
}

fn serve_producer(
    svc: &Arc<BusService>,
    ctx: &BusCallContext,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) {
    let payload = record_payload();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim();
        if line == "BYE" {
            break;
        }
        let Some(ts_str) = line.strip_prefix("PUB ") else {
            eprintln!("coordinator: producer sent unexpected line {line:?}");
            continue;
        };
        let Ok(timestamp_ms) = ts_str.parse::<i64>() else {
            eprintln!("coordinator: producer sent unparseable timestamp {ts_str:?}");
            continue;
        };
        let batch = PublishBatch {
            partition: Some(0),
            producer: None,
            records: vec![PublishRecord {
                key: None,
                headers: Vec::new(),
                payload: payload.clone(),
                timestamp_ms,
                schema_id: 0,
            }],
        };
        match svc.publish(ctx, TOPIC, batch) {
            Ok(_) => {
                if writeln!(writer, "ACK").is_err() {
                    break;
                }
            }
            Err(e) => {
                eprintln!("coordinator: publish failed: {e}");
                let _ = writeln!(writer, "ERR {e}");
            }
        }
    }
}

fn serve_consumer(
    svc: &Arc<BusService>,
    ctx: &BusCallContext,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) {
    let handle = match svc.open_consumer(
        ctx,
        "bus-e2e-bench-consumer",
        &[TOPIC.to_string()],
        ConsumerConfig {
            commit_mode: groups::CommitMode::Explicit,
        },
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("coordinator: open_consumer failed: {e}");
            return;
        }
    };

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim();
        if line == "BYE" {
            break;
        }
        if line != "FETCH" {
            eprintln!("coordinator: consumer sent unexpected line {line:?}");
            continue;
        }
        match handle.fetch(FETCH_MAX_BYTES, FETCH_LONG_POLL_MS) {
            Ok(batch) => {
                let mut io_ok = true;
                for rec in &batch.records {
                    if writeln!(writer, "REC {}", rec.timestamp_ms).is_err() {
                        io_ok = false;
                        break;
                    }
                }
                if io_ok && writeln!(writer, "END").is_err() {
                    break;
                }
                if !io_ok {
                    break;
                }
            }
            Err(e) => {
                eprintln!("coordinator: fetch failed: {e}");
                let _ = writeln!(writer, "END");
            }
        }
    }
}

// ===== Producer child: publishes TOTAL_MESSAGES records, one at a time =====

fn run_producer() -> anyhow::Result<()> {
    let addr = std::env::var(ADDR_ENV)?;
    let total: usize = std::env::var(TOTAL_ENV)?.parse()?;

    let stream = TcpStream::connect(&addr)?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    writeln!(writer, "HELLO producer")?;

    let started = Instant::now();
    for _ in 0..total {
        let ts = now_ms();
        writeln!(writer, "PUB {ts}")?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            anyhow::bail!("producer: coordinator closed the connection early");
        }
        let line = line.trim();
        if line != "ACK" {
            anyhow::bail!("producer: unexpected reply {line:?}");
        }
    }
    writeln!(writer, "BYE")?;

    eprintln!(
        "producer: published {total} messages in {:.2?} ({:.0} msg/s)",
        started.elapsed(),
        total as f64 / started.elapsed().as_secs_f64().max(1e-9),
    );
    Ok(())
}

// ===== Consumer child: long-polls FETCH until it has `total` samples =====

fn run_consumer() -> anyhow::Result<()> {
    let addr = std::env::var(ADDR_ENV)?;
    let total: usize = std::env::var(TOTAL_ENV)?.parse()?;

    let stream = TcpStream::connect(&addr)?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    writeln!(writer, "HELLO consumer")?;

    let mut latencies_ms: Vec<i64> = Vec::with_capacity(total);
    // Generous overall deadline: FETCH_LONG_POLL_MS-bounded round trips times
    // enough iterations to drain `total` records even if the producer is
    // slow, plus headroom for a shared/loaded host — not a gate threshold.
    let deadline = Instant::now() + Duration::from_secs(120);
    while latencies_ms.len() < total && Instant::now() < deadline {
        writeln!(writer, "FETCH")?;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                anyhow::bail!("consumer: coordinator closed the connection early");
            }
            let line = line.trim();
            if line == "END" {
                break;
            }
            if let Some(ts_str) = line.strip_prefix("REC ") {
                let ts: i64 = ts_str.parse()?;
                let now = now_ms();
                latencies_ms.push((now - ts).max(0));
            }
        }
    }
    writeln!(writer, "BYE")?;

    if latencies_ms.len() < total {
        eprintln!(
            "consumer: WARNING only received {}/{total} records before the deadline",
            latencies_ms.len()
        );
    }

    latencies_ms.sort_unstable();
    let n = latencies_ms.len();
    let percentile = |q: f64| -> i64 {
        if n == 0 {
            return 0;
        }
        let idx = (((n - 1) as f64) * q).round() as usize;
        latencies_ms[idx]
    };
    let p50 = percentile(0.50);
    let p99 = percentile(0.99);
    let p999 = percentile(0.999);
    let mean = if n > 0 {
        latencies_ms.iter().sum::<i64>() / n as i64
    } else {
        0
    };

    println!(
        "RESULT n={n} p50_ms={p50} p99_ms={p99} p999_ms={p999} mean_ms={mean} \
         (cross-process publish->consume, ms-resolution timestamps)"
    );
    Ok(())
}
