// ===== File: benches/support/mod.rs — shared harness for bus_path.rs =====
//
// Not itself a `[[bench]]` target: lives in a subdirectory of `benches/` so
// Cargo's auto-discovery (which only scans files directly inside `benches/`)
// never tries to build it as its own binary. Included via `mod support;`
// from `bus_path.rs`.
//
// Mirrors `tentaflow-bus/benches/support/mod.rs`'s percentile/latency
// helpers (PLAN §5.4 harness pattern) but adds what a SERVICE-layer bench
// needs that an engine-layer one does not: a throwaway SQLite DB (the real
// migrated app schema, via `tentaflow_core::db::init` — `:memory:` is not
// reachable from outside the crate) plus a `BusService` instance wired with
// an allow-all authorizer and an effectively-unlimited org quota, so the
// gates in `bus_path.rs` measure the log/fsync/service-code path itself,
// never RBAC or the org-level token bucket (`QuotaConfig::default()`'s
// `produce_msgs_per_sec = 200_000` would silently cap every P1 number in
// this file well below what the engine can do).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tentaflow_core::bus::{
    self, quota, topics, BusAction, BusCallContext, BusInitConfig, BusService, BusServiceError,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::org::DEFAULT_ORG_ID;

// ---- latency/throughput plumbing (mirrors tentaflow-bus/benches/support) --

pub fn percentile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = (((sorted.len() - 1) as f64) * q).round() as usize;
    sorted[idx]
}

pub struct LatencyReport {
    pub n: usize,
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub p999: Duration,
}

impl LatencyReport {
    pub fn from_sorted(sorted: &[Duration]) -> Self {
        let mean: Duration = if sorted.is_empty() {
            Duration::ZERO
        } else {
            sorted.iter().sum::<Duration>() / sorted.len() as u32
        };
        Self {
            n: sorted.len(),
            mean,
            p50: percentile(sorted, 0.50),
            p95: percentile(sorted, 0.95),
            p99: percentile(sorted, 0.99),
            p999: percentile(sorted, 0.999),
        }
    }
}

/// Deterministic, non-repeating fill (splitmix64) so lz4/on-disk-size
/// numbers see realistic, largely-incompressible payloads instead of a
/// trivially-compressible constant byte — same rationale and algorithm as
/// `tentaflow-bus/benches/support::pseudo_random_bytes`, duplicated here
/// rather than shared across the crate boundary (this is a bench-only,
/// dependency-free helper, not worth a shared crate for four lines of math).
pub fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed ^ 0x9E3779B97F4A7C15;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

/// Root directory every `bus_path` gate writes its throwaway DB/bus dir
/// under. `TENTABUS_BENCH_DIR` overrides the default (`std::env::temp_dir()`)
/// — same override `tentaflow-bus`'s benches honor, for the same reason:
/// some setups have `/tmp` on tmpfs, which would silently turn every
/// durability number in this file into a memory-bandwidth number instead.
fn bench_root() -> PathBuf {
    match std::env::var_os("TENTABUS_BENCH_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir(),
    }
}

/// Allow-all authorizer — the same pattern `tests/bus_demo_seed.rs` uses:
/// these gates measure the log/fsync/service-code path, not RBAC, and a
/// production `BusAuthorizer` is `dispatch/`'s territory, out of this
/// bench's ownership.
pub struct AllowAllAuthorizer;

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

/// One throwaway `BusService` + its own SQLite DB and bus directory, both
/// under a freshly created `tempfile::TempDir` that is removed on `Drop` —
/// every gate in `bus_path.rs` builds its own `BenchWorld` rather than
/// sharing a process-wide one, matching `bus::init`'s own doc: it is backed
/// by a `OnceCell` (`BUS_SERVICE`), so the free function silently ignores a
/// second, differently-configured call in the SAME process — going through
/// `BusService::new` directly (not `bus::init`) sidesteps that singleton
/// entirely and gives each gate an isolated instance.
pub struct BenchWorld {
    pub svc: Arc<BusService>,
    pub db: DbPool,
    pub ctx: BusCallContext,
    // Kept alive for `BenchWorld`'s lifetime purely for its `Drop` impl
    // (removes the directory tree); never read directly.
    _tmp: tempfile::TempDir,
}

/// Builds a fresh `BenchWorld`: creates `<tmp>/data/tentaflow.db` (runs the
/// crate's real migrations, same opener `tests/bus_demo_seed.rs` uses — a
/// bench has no access to the crate-internal `:memory:` path), opens a
/// `BusService` against `<tmp>/bus` with an allow-all authorizer, and raises
/// the default org's quota to effectively unlimited (`QuotaConfig::default()`
/// caps produce at 200k msg/s / 2 GiB/s, well below what several of this
/// file's gates are meant to measure).
pub fn bench_world(label: &str) -> BenchWorld {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("tentaflow-core-bus-path-{label}-"))
        .tempdir_in(bench_root())
        .expect("create bench tempdir");
    let db_dir = tmp.path().join("data");
    std::fs::create_dir_all(&db_dir).expect("create db dir");
    let db_path = db_dir.join("tentaflow.db");
    let db = tentaflow_core::db::init(&db_path).expect("open/migrate throwaway db");
    let bus_dir = tmp.path().join("bus");

    let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
    bus::db::migrate(&local_conn).expect("migrate local db");
    let local_db: DbPool = Arc::new(tentaflow_core::db::Db::from_connection(local_conn));

    let svc = Arc::new(
        BusService::new(BusInitConfig {
            instance_id: bus::instance::BusInstanceId::parse("tentabus-00000001")
                .expect("valid instance id"),
            local_db,
            bus_dir,
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            // No background sweeper: every gate that cares about retention
            // (P13) calls `run_retention_sweep()` by hand so its own timing
            // is exactly what gets measured, not diluted by a concurrent
            // periodic sweep racing it.
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("BusService::new"),
    );

    svc.quota().set_org_quota(DEFAULT_ORG_ID, unlimited_quota());

    let ctx = BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id: DEFAULT_ORG_ID.to_string(),
        actor: Some("bus-path-bench".to_string()),
        correlation_id: None,
        origin: "bus_path_bench".to_string(),
    };

    BenchWorld {
        svc,
        db,
        ctx,
        _tmp: tmp,
    }
}

/// A rate of `0` in `quota::TokenBucket` means "unlimited" (see that type's
/// doc) — used instead of `QuotaConfig::default()` (200k msg/s / 2 GiB/s)
/// for every gate in this file whose whole point is to find out how much
/// higher than that the log/fsync path itself can go.
pub fn unlimited_quota() -> quota::QuotaConfig {
    quota::QuotaConfig {
        max_topics: 10_000,
        max_partitions: 100_000,
        max_bytes_total: u64::MAX,
        produce_msgs_per_sec: 0,
        produce_bytes_per_sec: 0,
        max_groups: 10_000,
    }
}

/// `TopicOptions` builder for the byte-accounting gates (P1/P5/P10):
/// compression explicitly OFF so the on-disk bytes this file measures via
/// `BusService::partition_stats` are directly comparable to
/// `device_ceiling.rs`'s raw, uncompressed byte counts — matching
/// `log_perf.rs`'s own convention (its `build_batch` probe also defaults
/// `codec: None`) rather than confounding lz4 CPU cost with I/O throughput.
///
/// Takes a `DurabilityClass` (owner decision B), not a raw `DurabilityPolicy`
/// — `TopicOptions::durability_class` is resolved against the node's
/// environment by `DurabilityClass::resolve` exactly like a real caller's
/// topic would be (this bench's throwaway DB has no `node_environment`
/// setting row, so `get_node_environment` defaults to `Prod`, matching
/// decision B's Prod/Test resolution row: `Standard` -> `FsyncInterval{ms:
/// 50}`, `Critical` -> `FsyncBatchFull`). Every gate that needs the
/// concrete resolved policy for logging reads it back off the
/// `TopicConfig` `create_topic` returns, rather than re-deriving it here.
pub fn byte_accounting_topic_options(
    partitions: u32,
    durability_class: topics::DurabilityClass,
) -> topics::TopicOptions {
    topics::TopicOptions {
        partitions: Some(partitions),
        durability_class: Some(durability_class),
        compression: Some(topics::CompressionPolicy::None),
        ..Default::default()
    }
}
