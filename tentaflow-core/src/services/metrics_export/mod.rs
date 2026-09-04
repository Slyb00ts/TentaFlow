// =============================================================================
// File: services/metrics_export/mod.rs — Zabbix metrics exporter (Z5)
// =============================================================================
//
// Collects a snapshot of THIS node's operational metrics — CPU/RAM/GPU/fs
// from `mesh::node_info_collector` (the same source the mesh heartbeat uses
// for itself), request/token counters from `RouterMetrics`, service and
// flow-execution counts from SQLite, and this node's own mesh visibility from
// `MeshPeerStore` — and serves them at `GET /v1/metrics/zabbix`.
//
// Format decision: Zabbix HTTP agent, `key value\n` per line — the simpler of
// the two options `ZADANIA.md` left open. Metric names use `tentaflow_*`
// (underscores, not dots): this is what makes the body a REAL label-less
// Prometheus exposition (Prometheus metric names are `[a-zA-Z_:][a-zA-Z0-9_:]*`
// — dots are not legal there), so `assets/zabbix-template.xml` can split it
// into per-metric dependent items with the built-in "Prometheus pattern"
// preprocessing step instead of a bespoke parser. An earlier revision used
// dotted keys and shipped a template that silently matched nothing — fixed
// per code review (P1.1). The JSON trapper protocol was intentionally NOT
// implemented — it needs `zabbix_sender` push semantics this pull endpoint
// does not have; revisit if CMC monitoring needs push-based delivery instead
// of HTTP-agent polling.
//
// Per-node scope decision: each node exposes ONLY its own metrics, never
// mesh peers' — Zabbix is expected to poll every node individually (one
// Zabbix host per TentaFlow node), mirroring the per-node design of
// `HeartbeatMetrics` in `mesh/peer_store.rs`. `collect()` reads local system
// state directly rather than aggregating `MeshPeerStore` peer rows (those
// describe OTHER nodes as last observed over mesh, which would silently go
// stale and is not this node's own health); `MeshPeerStore::seed_local` also
// puts THIS node's own row in that store, so `collect()` excludes
// `local_node_id` from the peer counts it does report (mesh visibility of
// others), or it would count the node against itself (P2.1).
//
// `tentaflow_fs_*` reports usage of the whole filesystem mounted under
// `TENTAFLOW_HOME`, not TentaFlow's own footprint on it (other data can share
// that mount) — named `fs`, not `disk`, so a dashboard doesn't read it as
// "how much disk TentaFlow uses" (P1.1 review note).
//
// Auth: `Authorization: Bearer <monitoring_zabbix_token>` on the METRICS route
// only, compared constant-time (`subtle::ConstantTimeEq`, same pattern as
// `services/signed_urls/issuer.rs`). This is NOT the `/v1` API-key gate — a
// Zabbix agent has no session and no `api_keys` row, so `identity_for_api_key`
// does not apply here. The TEMPLATE route (`/v1/metrics/zabbix/template`) is
// deliberately UNAUTHENTICATED (P2.3, coordinator decision): it is a static
// asset with no secrets baked in (the `{$TENTAFLOW.METRICS.TOKEN}` macro ships
// empty), so the UI can offer a plain `<a href download>` without having to
// smuggle the token into a query string. Both routes still fail-closed on
// `monitoring_zabbix_enabled` being off (404 — the route does not exist) and
// both are rate-limited (`api::rate_limit::rate_limiter()`) before anything
// else runs. A decrypt failure reading `monitoring_zabbix_token` (as opposed
// to it simply being unset) is logged via `warn!` — most likely a legacy
// plaintext value predating `SettingsCipher` — without ever logging the token
// itself; the request is still rejected either way (fail-closed).
//
// `handle_request` does blocking I/O throughout (rusqlite via `db.read()`,
// `SettingsCipher::decrypt`, `sysinfo` refreshes, a filesystem stat) and must
// be called from `tokio::task::spawn_blocking` by the HTTP layer — see the
// call site in `api/unified_server.rs` (pattern: `api/model_bundle.rs`'s
// `sha256_file_hex`, `api/dashboard/ws_binary.rs:380-386`).
//
// Known gap: `ZADANIA.md` also asks this collector to surface "sync ledger
// open conflicts". The ledger (`sync::ledger::FjallSyncLedgerStore`) is owned
// by `sync::runtime` and is not threaded through `start_unified_server`, and
// wiring that in touches files reserved for the parallel sync/mesh work in
// this batch. Left out of the snapshot rather than faked — add a
// `flow`-style `conflicts_open` field once a ledger handle is reachable here.
//
// `tentaflow_bus_*` is the ONE label-bearing exception to the label-less rule
// above (plan-app-platform §3.7): TentaBus is `singleton = false`, so a node
// can run several fully isolated instances at once, and this exporter must
// never fold two instances' counters into one number (that would be exactly
// the cross-instance data mixing the whole conversion exists to prevent).
// Every `tentaflow_bus_*` sample therefore carries a `bus_instance="<id>"`
// label and is repeated once per entry of `bus::running_instances()` — zero
// instances running emits none of these lines at all, never a zero-valued
// line attributed to no instance. The label is named `bus_instance`, not
// `instance`: Prometheus reserves `instance` for its own scrape-time
// relabeling (`host:port` of the target), and a metric shipping its own
// `instance` label collides with that convention. See `collect_bus_metrics`
// for how the per-instance rollups are built and `format_zabbix` for how the
// label is rendered. OPERATIONAL IMPACT: the shipped `assets/zabbix-
// template.xml` still declares one static `PROMETHEUS_PATTERN` dependent
// item per bare metric name (no label filter) for backward compatibility —
// with exactly zero or one instance running that keeps working exactly as
// before (zero instances: item goes "not supported", no matching series,
// instead of the old fabricated zero; one instance: single unambiguous
// match). With two or more instances running, EVERY `tentaflow_bus_*`
// dependent item in that template becomes ambiguous (multiple label
// combinations match the same bare pattern) and Zabbix reports the item as
// failing rather than picking one arbitrarily. Making the template itself
// instance-aware (per-instance dependent items, which need Zabbix low-level
// discovery keyed off the dynamic instance set) is not part of this change.

use hyper::StatusCode;
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::api::rate_limit::{rate_limiter, RateLimitResult};
use crate::bus::instance::BusInstanceId;
use crate::crypto::SettingsCipher;
use crate::db::{self, DbPool};
use crate::mesh::node_info_collector;
use crate::mesh::peer_store::MeshPeerStore;
use crate::metrics::RouterMetrics;
use crate::paths;
use crate::services::storage_admin;
use crate::services_repo::services::{list_all, ServiceStatus};

/// Embedded Zabbix template — one HTTP-agent master item polling
/// `/v1/metrics/zabbix`, plus a dependent item per metric key below
/// (Prometheus-pattern preprocessing). Exported at `<version>6.0</version>`
/// (plain top-level `<groups>`, not the `<template_groups>` split that only
/// exists from Zabbix 6.2 onward — 6.0 is importable into 6.0-7.x, the
/// reverse is not true). Served verbatim at `GET /v1/metrics/zabbix/template`.
const ZABBIX_TEMPLATE_XML: &[u8] = include_bytes!("../../../assets/zabbix-template.xml");

/// Snapshot of this node's operational metrics, ready for [`format_zabbix`].
/// Field grouping mirrors the sources named in `ZADANIA.md` Z5.
#[derive(Debug)]
pub struct ExportedMetrics {
    pub cpu_usage_percent: f32,
    pub cpu_temperature_c: Option<f32>,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub swap_used_mb: u64,
    pub swap_total_mb: u64,
    pub gpu_count: u64,
    pub gpu_usage_percent_avg: f32,
    pub gpu_vram_used_mb: u64,
    pub gpu_vram_total_mb: u64,
    pub router_requests_total: u64,
    pub router_errors_total: u64,
    pub router_requests_active: u64,
    pub router_tokens_per_second: u64,
    pub router_input_tokens_per_second: u64,
    pub services_total: u64,
    pub services_running: u64,
    pub services_degraded: u64,
    pub services_failed: u64,
    pub services_stopped: u64,
    pub flow_executions_total: u64,
    pub flow_executions_running: u64,
    pub flow_executions_completed: u64,
    pub flow_executions_error: u64,
    pub flow_executions_cancelled: u64,
    pub mesh_peers_known: u64,
    pub mesh_peers_connected: u64,
    pub fs_total_bytes: u64,
    pub fs_available_bytes: u64,
    pub fs_used_percent: f32,
    pub system_uptime_seconds: u64,
    // ---- TentaBus M2 (PLAN §8.4), multi-instance (plan-app-platform §3.7)
    // — see `collect_bus_metrics`'s doc for how each entry is sourced, what
    // "best-effort" means for it, and why this is a `Vec` (one rollup per
    // RUNNING instance) rather than 16 flat scalar fields. `pub(crate)`,
    // not `pub` like every other field here: it carries `BusMetricsRollup`,
    // itself deliberately `pub(crate)` (see that type's own doc — it also
    // backs the `__bus.metrics` wire payload and isn't a stabilized public
    // type yet). Nothing outside this crate constructs or reads
    // `ExportedMetrics` today (`format_zabbix`/`handle_request` are its only
    // consumers), so this is a compile-time-checked "not part of any public
    // API surface" note, not a behavior change.
    pub(crate) bus_instances: Vec<(BusInstanceId, BusMetricsRollup)>,
}

/// Aggregates the snapshot from live in-process state (`router_metrics`,
/// `mesh_peer_store`) and SQLite (`db`). Best-effort: a source that fails to
/// read (e.g. a lock timeout) contributes zeros rather than failing the whole
/// scrape — a Zabbix poller times out on a missing endpoint far worse than it
/// handles a temporarily-zeroed counter. `local_node_id` excludes THIS node's
/// own row (seeded into `mesh_peer_store` by `seed_local`) from the peer
/// counts (P2.1) — those describe visibility of OTHER nodes.
pub fn collect(
    db: &DbPool,
    router_metrics: &RouterMetrics,
    mesh_peer_store: &MeshPeerStore,
    local_node_id: &str,
) -> ExportedMetrics {
    let fast = node_info_collector::collect_fast_metrics();
    let mem_total_mb = node_info_collector::total_memory_mb();

    let gpu_count = fast.gpus.len() as u64;
    let gpu_usage_percent_avg = if fast.gpus.is_empty() {
        0.0
    } else {
        fast.gpus.iter().map(|g| g.usage_percent).sum::<f32>() / fast.gpus.len() as f32
    };
    let gpu_vram_used_mb = fast.gpus.iter().map(|g| g.vram_used_mb).sum();
    let gpu_vram_total_mb = fast.gpus.iter().map(|g| g.vram_total_mb).sum();

    let router = router_metrics.snapshot();

    let services = service_counts(db);
    let flows = flow_execution_counts(db);

    let others = mesh_peer_store
        .list()
        .into_iter()
        .filter(|p| p.node_id != local_node_id)
        .count();
    let mesh_peers_connected = mesh_peer_store
        .list()
        .into_iter()
        .filter(|p| p.node_id != local_node_id && p.quic_connected)
        .count() as u64;
    let mesh_peers_known = others as u64;

    let (fs_total_bytes, fs_available_bytes) =
        storage_admin::disk_space(paths::tentaflow_home()).unwrap_or((0, 0));
    let fs_used_percent = if fs_total_bytes == 0 {
        0.0
    } else {
        let used = fs_total_bytes.saturating_sub(fs_available_bytes);
        (used as f64 / fs_total_bytes as f64 * 100.0) as f32
    };

    let bus_instances = collect_bus_metrics(db);

    ExportedMetrics {
        cpu_usage_percent: fast.cpu_usage_percent,
        cpu_temperature_c: fast.cpu_temperature_c,
        mem_used_mb: fast.ram_used_mb,
        mem_total_mb,
        swap_used_mb: fast.swap_used_mb,
        swap_total_mb: fast.swap_total_mb,
        gpu_count,
        gpu_usage_percent_avg,
        gpu_vram_used_mb,
        gpu_vram_total_mb,
        router_requests_total: router.total_requests,
        router_errors_total: router.total_errors,
        router_requests_active: router.active_requests,
        router_tokens_per_second: router.tokens_per_second,
        router_input_tokens_per_second: router.input_tokens_per_second,
        services_total: services.0,
        services_running: services.1,
        services_degraded: services.2,
        services_failed: services.3,
        services_stopped: services.4,
        flow_executions_total: flows.0,
        flow_executions_running: flows.1,
        flow_executions_completed: flows.2,
        flow_executions_error: flows.3,
        flow_executions_cancelled: flows.4,
        mesh_peers_known,
        mesh_peers_connected,
        fs_total_bytes,
        fs_available_bytes,
        fs_used_percent,
        system_uptime_seconds: sysinfo::System::uptime(),
        bus_instances,
    }
}

/// Intermediate result of `collect_bus_metrics`, before being flattened into
/// [`ExportedMetrics`]'s `bus_*` fields. Also reused verbatim (via
/// `pub(crate)` + `Serialize`) as the record body for the `__bus.metrics`
/// internal topic's 1s rollup (PLAN §8.4/M4 dogfooding) — see
/// `bus::spawn_metrics_rollup_timer`.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct BusMetricsRollup {
    publish_msgs_total: u64,
    publish_bytes_total: u64,
    consume_msgs_total: u64,
    throttled_total: u64,
    fsync_p99_us: u64,
    append_p99_us: u64,
    consumer_lag_max: u64,
    consumer_lag_sum: u64,
    dlq_depth: u64,
    topic_count: u64,
    partition_count: u64,
    disk_bytes: u64,
    isr_size_min: u64,
    isr_shrink_total: u64,
    leader_epoch_max: u64,
    replication_lag_bytes_max: u64,
}

/// TentaBus M2 (PLAN §8.4), multi-instance (plan-app-platform §3.7): one
/// `BusMetricsRollup` of the 16 `tentaflow_bus_*` metrics per entry of
/// `bus::running_instances()` — TentaBus is `singleton = false`, so a node
/// can run several fully isolated instances at once, and folding them into
/// one node-wide number would misattribute one instance's data to another
/// (or to whichever instance happened to be alone) exactly as `bus::
/// global()` used to. Zero running instances yields an empty `Vec` — never a
/// zero-valued rollup attributed to nothing, and never a panic (`running_
/// instances()` returning empty simply makes the iterator below yield
/// nothing).
///
/// Best-effort per instance, same convention as every other collector in
/// this file: nothing here can fail the whole scrape.
pub(crate) fn collect_bus_metrics(db: &DbPool) -> Vec<(BusInstanceId, BusMetricsRollup)> {
    crate::bus::running_instances()
        .into_iter()
        .map(|svc| {
            // `bus::running_instances()` only ever returns engines keyed by
            // an already-validated `BusInstanceId` (`BusService::new` takes
            // one, never a raw string, and re-derives `instance_id()` from
            // it) — same invariant `BusService::typed_instance_id` relies
            // on internally.
            let id = BusInstanceId::parse(svc.instance_id())
                .expect("running BusService instance ids are always valid BusInstanceIds");
            let rollup = collect_instance_bus_metrics(db, &svc);
            (id, rollup)
        })
        .collect()
}

/// Single-instance rollup for a caller that already holds the engine —
/// e.g. `BusService::publish_metrics_rollup` publishing its OWN instance's
/// snapshot onto its own `__bus.metrics` topic (plan-app-platform §3.7:
/// "`__bus.metrics` stays a per-instance internal topic — each engine
/// publishes its own rollup into its own instance"). Shares every byte of
/// logic with `collect_bus_metrics` (below) via this one function, so the
/// two never drift.
pub(crate) fn collect_instance_bus_metrics(
    db: &DbPool,
    svc: &crate::bus::BusService,
) -> BusMetricsRollup {
    let (publish_msgs_total, publish_bytes_total, consume_msgs_total, throttled_total) =
        svc.bus_metrics_snapshot();
    let (append_p99_us, fsync_p99_us) = crate::bus::BusService::bus_engine_p99_us();
    let (topic_count, partition_count) = bus_topic_and_partition_counts(db, svc.instance_id());
    let disk_bytes = dir_size_bytes(svc.bus_dir());

    let mut rollup = BusMetricsRollup {
        publish_msgs_total,
        publish_bytes_total,
        consume_msgs_total,
        throttled_total,
        append_p99_us,
        fsync_p99_us,
        topic_count,
        partition_count,
        disk_bytes,
        ..Default::default()
    };

    if let Some(coordinator) = svc.replication() {
        // No per-org scope to work from here (unlike every authenticated
        // `BusService` method, this collector has no `BusCallContext`), so
        // every org this node knows a bus topic for is rolled into one
        // node-wide figure — matching this whole endpoint's per-node (not
        // per-org) scope, same as `services_total`/`flow_executions_total`
        // above.
        let orgs = bus_org_ids(db, svc.instance_id());
        let mut isr_min: Option<u64> = None;
        let mut leader_epoch_max = 0u64;
        let mut lag_bytes_max = 0u64;
        let mut dlq_depth = 0u64;
        // (org, topic, partition) -> high_watermark, collected alongside
        // the rollup above so the consumer-lag join below never has to
        // re-derive which partitions exist.
        let mut hw_by_key: std::collections::HashMap<(String, String, u32), u64> =
            std::collections::HashMap::new();

        for org in &orgs {
            let snapshot = coordinator.snapshot(org, None);
            for p in &snapshot.partitions {
                let isr_len = p.isr.len() as u64;
                isr_min = Some(isr_min.map_or(isr_len, |m| m.min(isr_len)));
                leader_epoch_max = leader_epoch_max.max(p.leader_epoch as u64);
                if let Some(max_lag) = p.lagging.iter().map(|l| l.lag_bytes).max() {
                    lag_bytes_max = lag_bytes_max.max(max_lag);
                }
                if p.topic.starts_with(crate::bus::dlq::DLQ_TOPIC_PREFIX) {
                    dlq_depth += p.log_end_offset;
                }
                hw_by_key.insert(
                    (org.clone(), p.topic.clone(), p.partition),
                    p.high_watermark,
                );
            }
        }

        rollup.isr_size_min = isr_min.unwrap_or(0);
        rollup.isr_shrink_total = coordinator.isr_shrink_total();
        rollup.leader_epoch_max = leader_epoch_max;
        rollup.replication_lag_bytes_max = lag_bytes_max;
        rollup.dlq_depth = dlq_depth;

        // Consumer lag: join every registered (org, group, topic)
        // subscription (`bus_groups`) against the high-watermarks just
        // collected above, one committed-offset lookup per (partition) the
        // topic's own snapshot rows describe. Fjall's `GroupOffsetStore`
        // has no enumeration API (only point lookups keyed by (org, group,
        // topic, partition)), so a group with no `bus_groups` row (never
        // opened a consumer here) is correctly invisible to this join —
        // this reports lag only for currently-registered subscriptions,
        // not a full historical scan.
        for (org, group, topic) in bus_group_subscriptions(svc.local_db()) {
            for ((o, t, partition), hw) in &hw_by_key {
                if *o != org || *t != topic {
                    continue;
                }
                if let Ok(committed) = svc.group_committed_offset(&org, &group, &topic, *partition)
                {
                    let lag = hw.saturating_sub(committed);
                    rollup.consumer_lag_max = rollup.consumer_lag_max.max(lag);
                    rollup.consumer_lag_sum += lag;
                }
            }
        }
    }
    // No coordinator installed: every replication-derived field above stays
    // at `BusMetricsRollup::default()`'s `0` — including `isr_shrink_total`,
    // which is a true reading here (no coordinator means nothing has ever
    // shrunk an ISR on this node), not a placeholder.

    rollup
}

/// Distinct org ids that have at least one row in `bus_topics` FOR THIS
/// INSTANCE (plan-app-platform §7 W4: `bus_topics` is keyed by `instance_id`
/// now) — this collector has no per-request org scope of its own, so every
/// replication-derived bus metric aggregates across every org THIS
/// INSTANCE knows about (see `collect_bus_metrics`'s doc).
fn bus_org_ids(db: &DbPool, instance_id: &str) -> Vec<String> {
    let Ok(conn) = db.read() else {
        return Vec::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT org_id FROM bus_topics WHERE instance_id = ?1")
    else {
        return Vec::new();
    };
    let Ok(mapped) = stmt.query_map([instance_id], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    mapped.filter_map(std::result::Result::ok).collect()
}

/// Every registered consumer-group subscription — `(org_id, group_id,
/// topic)` rows from `bus_groups` — feeding the consumer-lag join in
/// `collect_bus_metrics`. `db` here is the CALLER's `BusService::local_db()`
/// (plan-app-platform §7 W4: `bus_groups` lives in the per-instance content
/// database now, not the main pool), so no `instance_id` filter is needed —
/// the pool itself already is that one instance's own table.
fn bus_group_subscriptions(db: &DbPool) -> Vec<(String, String, String)> {
    let Ok(conn) = db.read() else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT org_id, group_id, topic FROM bus_groups") else {
        return Vec::new();
    };
    let Ok(mapped) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) else {
        return Vec::new();
    };
    mapped.filter_map(std::result::Result::ok).collect()
}

/// `(topic_count, partition_count)`, scoped to `instance_id` (plan-app-
/// platform §7 W4: `bus_topics` is keyed by `instance_id` now). `topic_count`
/// excludes internal `__dlq.*` topics (`bus::dlq::DLQ_TOPIC_PREFIX`) — an
/// operator's alerting on "how many topics exist" should not double-count
/// the DLQ TentaBus auto-creates per source topic. `partition_count` sums
/// every real topic's configured `partitions` column, DLQ topics included: a
/// DLQ topic's partitions are still real on-disk footprint this node
/// carries.
fn bus_topic_and_partition_counts(db: &DbPool, instance_id: &str) -> (u64, u64) {
    let Ok(conn) = db.read() else {
        return (0, 0);
    };
    let topic_count = conn
        .query_row(
            "SELECT COUNT(*) FROM bus_topics WHERE instance_id = ?1 AND name NOT LIKE '__dlq.%'",
            [instance_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .unwrap_or(0);
    let partition_count = conn
        .query_row(
            "SELECT COALESCE(SUM(partitions), 0) FROM bus_topics WHERE instance_id = ?1",
            [instance_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
        .unwrap_or(0);
    (topic_count, partition_count)
}

/// Best-effort recursive size (bytes) of every regular file under `dir` —
/// backs `tentaflow_bus_disk_bytes`. Plain `std::fs`, deliberately not a new
/// bus-engine API (PLAN §8.4): this is a metrics-only figure, not something
/// the engine itself needs to expose. Any error at any level (missing dir,
/// permissions, a file racing a concurrent delete) collapses that entry to
/// `0` rather than failing the whole scrape, matching every other collector
/// in this file. Depth-bounded purely as a defense against a symlink cycle,
/// not because the bus directory layout nests anywhere near this deep.
fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path, depth: u32) -> u64 {
        if depth > 64 {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut total = 0u64;
        for entry in entries.filter_map(std::result::Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                total += walk(&entry.path(), depth + 1);
            } else if file_type.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        total
    }
    walk(dir, 0)
}

/// (total, running, degraded, failed, stopped). `deploying`/`starting`/
/// `interrupted` are transient states folded into `total` only, to keep the
/// exported key set to the steady-state statuses an operator alerts on.
fn service_counts(db: &DbPool) -> (u64, u64, u64, u64, u64) {
    let Ok(conn) = db.read() else {
        return (0, 0, 0, 0, 0);
    };
    let Ok(rows) = list_all(&conn) else {
        return (0, 0, 0, 0, 0);
    };
    let total = rows.len() as u64;
    let mut running = 0u64;
    let mut degraded = 0u64;
    let mut failed = 0u64;
    let mut stopped = 0u64;
    for row in &rows {
        match row.status {
            ServiceStatus::Running => running += 1,
            ServiceStatus::Degraded => degraded += 1,
            ServiceStatus::Failed => failed += 1,
            ServiceStatus::Stopped => stopped += 1,
            ServiceStatus::Deploying | ServiceStatus::Starting | ServiceStatus::Interrupted => {}
        }
    }
    (total, running, degraded, failed, stopped)
}

/// (total, running, completed, error, cancelled) — status values written by
/// `flow_engine::executor::persist_execution` / `create_execution_record`.
fn flow_execution_counts(db: &DbPool) -> (u64, u64, u64, u64, u64) {
    let Ok(conn) = db.read() else {
        return (0, 0, 0, 0, 0);
    };
    let Ok(mut stmt) = conn.prepare("SELECT status, COUNT(*) FROM flow_executions GROUP BY status")
    else {
        return (0, 0, 0, 0, 0);
    };
    let Ok(mapped) = stmt.query_map([], |row| {
        let status: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((status, count))
    }) else {
        return (0, 0, 0, 0, 0);
    };
    let rows: Vec<(String, i64)> = mapped.filter_map(std::result::Result::ok).collect();

    let mut total = 0u64;
    let mut running = 0u64;
    let mut completed = 0u64;
    let mut error = 0u64;
    let mut cancelled = 0u64;
    for (status, count) in rows {
        let count = count.max(0) as u64;
        total += count;
        match status.as_str() {
            "running" => running += count,
            "completed" => completed += count,
            "error" => error += count,
            "cancelled" => cancelled += count,
            _ => {}
        }
    }
    (total, running, completed, error, cancelled)
}

/// Pushes an integer-valued line unconditionally (no NaN/Inf concept for
/// integers). `key` takes `impl Into<String>` rather than `&'static str`
/// (pre-multi-instance-bus shape) because a labeled `tentaflow_bus_*` key
/// (`push_bus_metric`, below) is built at runtime from an instance id, not a
/// static string.
fn push_int(lines: &mut Vec<(String, String)>, key: impl Into<String>, value: u64) {
    lines.push((key.into(), value.to_string()));
}

/// Pushes a float-valued line, but only when `value` is finite (P3.3) — a
/// NaN or +/-inf reading (e.g. a `0/0` percentage on a since-fixed source)
/// must not reach Zabbix as a bogus number a trigger could act on; skipping
/// the line is indistinguishable from "not collected", which is the correct,
/// safe reading for a scrape that could not produce a real value.
fn push_float(lines: &mut Vec<(String, String)>, key: impl Into<String>, value: f32) {
    if value.is_finite() {
        lines.push((key.into(), format!("{value:.2}")));
    }
}

/// Pushes one `tentaflow_bus_*` sample labeled `bus_instance="<id>"` (plan-
/// app-platform §3.7) — `bus_instance`, never `instance`: Prometheus scrape
/// configs reserve the `instance` label name for their own target
/// relabeling, and a metric that ships its own `instance` label collides
/// with it. One call per (instance, metric) pair in `format_zabbix`'s bus
/// loop below.
fn push_bus_metric(
    lines: &mut Vec<(String, String)>,
    name: &str,
    instance: &BusInstanceId,
    value: u64,
) {
    push_int(
        lines,
        format!("{name}{{bus_instance=\"{instance}\"}}"),
        value,
    );
}

/// Renders a snapshot as Zabbix HTTP-agent / Prometheus-exposition lines:
/// `key value\n`, one metric per line, `tentaflow_*` (underscored — see the
/// module header on why dots would be wrong here). Key order is fixed
/// (matches the field order in [`ExportedMetrics`] /
/// `assets/zabbix-template.xml`) so a diff between two scrapes is stable;
/// `tentaflow_cpu_temperature_c` is omitted entirely when the platform
/// exposes no CPU temperature sensor (or a non-finite reading) rather than
/// emitting a sentinel value a trigger could mistake for a real reading.
/// `tentaflow_bus_*` is the one label-bearing exception — see the module
/// header's multi-instance note and `push_bus_metric`.
pub fn format_zabbix(m: &ExportedMetrics) -> String {
    let mut lines: Vec<(String, String)> = Vec::with_capacity(31 + m.bus_instances.len() * 16);
    push_float(
        &mut lines,
        "tentaflow_cpu_usage_percent",
        m.cpu_usage_percent,
    );
    if let Some(temp) = m.cpu_temperature_c {
        push_float(&mut lines, "tentaflow_cpu_temperature_c", temp);
    }
    push_int(&mut lines, "tentaflow_mem_used_mb", m.mem_used_mb);
    push_int(&mut lines, "tentaflow_mem_total_mb", m.mem_total_mb);
    push_int(&mut lines, "tentaflow_swap_used_mb", m.swap_used_mb);
    push_int(&mut lines, "tentaflow_swap_total_mb", m.swap_total_mb);
    push_int(&mut lines, "tentaflow_gpu_count", m.gpu_count);
    push_float(
        &mut lines,
        "tentaflow_gpu_usage_percent_avg",
        m.gpu_usage_percent_avg,
    );
    push_int(&mut lines, "tentaflow_gpu_vram_used_mb", m.gpu_vram_used_mb);
    push_int(
        &mut lines,
        "tentaflow_gpu_vram_total_mb",
        m.gpu_vram_total_mb,
    );
    push_int(
        &mut lines,
        "tentaflow_router_requests_total",
        m.router_requests_total,
    );
    push_int(
        &mut lines,
        "tentaflow_router_errors_total",
        m.router_errors_total,
    );
    push_int(
        &mut lines,
        "tentaflow_router_requests_active",
        m.router_requests_active,
    );
    push_int(
        &mut lines,
        "tentaflow_router_tokens_per_second",
        m.router_tokens_per_second,
    );
    push_int(
        &mut lines,
        "tentaflow_router_input_tokens_per_second",
        m.router_input_tokens_per_second,
    );
    push_int(&mut lines, "tentaflow_services_total", m.services_total);
    push_int(&mut lines, "tentaflow_services_running", m.services_running);
    push_int(
        &mut lines,
        "tentaflow_services_degraded",
        m.services_degraded,
    );
    push_int(&mut lines, "tentaflow_services_failed", m.services_failed);
    push_int(&mut lines, "tentaflow_services_stopped", m.services_stopped);
    push_int(
        &mut lines,
        "tentaflow_flows_executions_total",
        m.flow_executions_total,
    );
    push_int(
        &mut lines,
        "tentaflow_flows_executions_running",
        m.flow_executions_running,
    );
    push_int(
        &mut lines,
        "tentaflow_flows_executions_completed",
        m.flow_executions_completed,
    );
    push_int(
        &mut lines,
        "tentaflow_flows_executions_error",
        m.flow_executions_error,
    );
    push_int(
        &mut lines,
        "tentaflow_flows_executions_cancelled",
        m.flow_executions_cancelled,
    );
    push_int(&mut lines, "tentaflow_mesh_peers_known", m.mesh_peers_known);
    push_int(
        &mut lines,
        "tentaflow_mesh_peers_connected",
        m.mesh_peers_connected,
    );
    push_int(&mut lines, "tentaflow_fs_total_bytes", m.fs_total_bytes);
    push_int(
        &mut lines,
        "tentaflow_fs_available_bytes",
        m.fs_available_bytes,
    );
    push_float(&mut lines, "tentaflow_fs_used_percent", m.fs_used_percent);
    push_int(
        &mut lines,
        "tentaflow_system_uptime_seconds",
        m.system_uptime_seconds,
    );

    // ---- TentaBus M2 (PLAN §8.4), multi-instance (plan-app-platform §3.7):
    // counters, p99s, lag, dlq, counts, disk, isr, epoch, replication lag,
    // throttled — see `collect_bus_metrics`'s doc for how each is sourced.
    // One full set of 16 samples per RUNNING instance, each labeled
    // `bus_instance="<id>"` (`push_bus_metric`) — zero running instances
    // emits none of these lines at all.
    for (instance, rollup) in &m.bus_instances {
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_publish_msgs_total",
            instance,
            rollup.publish_msgs_total,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_publish_bytes_total",
            instance,
            rollup.publish_bytes_total,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_consume_msgs_total",
            instance,
            rollup.consume_msgs_total,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_fsync_p99_us",
            instance,
            rollup.fsync_p99_us,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_append_p99_us",
            instance,
            rollup.append_p99_us,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_consumer_lag_max",
            instance,
            rollup.consumer_lag_max,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_consumer_lag_sum",
            instance,
            rollup.consumer_lag_sum,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_dlq_depth",
            instance,
            rollup.dlq_depth,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_topic_count",
            instance,
            rollup.topic_count,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_partition_count",
            instance,
            rollup.partition_count,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_disk_bytes",
            instance,
            rollup.disk_bytes,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_isr_size_min",
            instance,
            rollup.isr_size_min,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_isr_shrink_total",
            instance,
            rollup.isr_shrink_total,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_leader_epoch_max",
            instance,
            rollup.leader_epoch_max,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_replication_lag_bytes_max",
            instance,
            rollup.replication_lag_bytes_max,
        );
        push_bus_metric(
            &mut lines,
            "tentaflow_bus_throttled_total",
            instance,
            rollup.throttled_total,
        );
    }

    let mut out = String::new();
    for (key, value) in lines {
        out.push_str(&key);
        out.push(' ');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// Constant-time token comparison — same pattern as
/// `services/signed_urls/issuer.rs` (`subtle::ConstantTimeEq`). An empty
/// `expected` never matches, even against an empty `provided` (P3.7): the
/// caller (`handle_request`) already refuses to treat an empty configured
/// token as "configured", but this guard keeps the primitive itself safe
/// regardless of caller discipline. A length mismatch still short-circuits
/// (as `issuer.rs` does too): hiding that leak would require padding both
/// sides to a fixed size, which the token's length never needs to be secret
/// for.
fn token_matches(provided: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    provided.len() == expected.len() && bool::from(provided.ct_eq(expected))
}

/// HTTP-agnostic result of a Zabbix route. The caller in
/// `api/unified_server.rs` wraps this into a `hyper::Response`.
pub struct ZabbixResponse {
    pub status: StatusCode,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// `Some("no-store")` for the metrics route (P3.10) — never cache a live
    /// scrape. `None` for the template (a static asset; letting normal HTTP
    /// caching apply is fine and is what makes a plain `<a href download>`
    /// pleasant to use).
    pub cache_control: Option<&'static str>,
    /// Seconds the caller should wait before retrying — set on 429 only.
    pub retry_after_secs: Option<u64>,
}

impl ZabbixResponse {
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            cache_control: None,
            retry_after_secs: None,
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            cache_control: None,
            retry_after_secs: None,
        }
    }

    fn rate_limited(retry_after_secs: f64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            cache_control: None,
            retry_after_secs: Some(retry_after_secs.ceil().max(1.0) as u64),
        }
    }
}

fn setting_enabled(db: &DbPool, key: &str) -> bool {
    matches!(
        db::repository::get_setting(db, key)
            .ok()
            .flatten()
            .as_deref(),
        Some("1") | Some("true")
    )
}

/// Logs a rejected request without ever including the token value — callers
/// pass a short, fixed `reason` (never request-controlled data).
fn log_unauthorized(client_ip: &str, reason: &str) {
    warn!(
        target: "security",
        client_ip,
        reason,
        "zabbix metrics endpoint: request rejected"
    );
}

/// Handles both Zabbix routes (`/v1/metrics/zabbix` and
/// `/v1/metrics/zabbix/template`). `bearer_token` is the raw value after
/// `Bearer ` in the `Authorization` header, `client_ip` the caller's address
/// with any port stripped (both already extracted by the caller — this
/// function has no HTTP-parsing concerns, only the auth/rate-limit decision
/// and the response body). GET-only and blocking-IO-via-`spawn_blocking` are
/// also the caller's responsibility (`api/unified_server.rs`).
#[allow(clippy::too_many_arguments)] // each param is a distinct request-scoped input, not a group to bundle
pub fn handle_request(
    path: &str,
    bearer_token: Option<&str>,
    client_ip: &str,
    db: &DbPool,
    settings_cipher: &SettingsCipher,
    router_metrics: &RouterMetrics,
    mesh_peer_store: &MeshPeerStore,
    local_node_id: &str,
) -> ZabbixResponse {
    if !setting_enabled(db, "monitoring_zabbix_enabled") {
        // Fail-closed: the feature is off, so neither route exists.
        return ZabbixResponse::not_found();
    }

    if path == "/v1/metrics/zabbix/template" {
        return match rate_limiter().check(client_ip) {
            RateLimitResult::Allow => ZabbixResponse {
                status: StatusCode::OK,
                content_type: "application/xml",
                body: ZABBIX_TEMPLATE_XML.to_vec(),
                cache_control: None,
                retry_after_secs: None,
            },
            RateLimitResult::IpLimit {
                retry_after_secs, ..
            }
            | RateLimitResult::GlobalLimit { retry_after_secs } => {
                ZabbixResponse::rate_limited(retry_after_secs)
            }
        };
    }

    if path != "/v1/metrics/zabbix" {
        return ZabbixResponse::not_found();
    }

    match rate_limiter().check(client_ip) {
        RateLimitResult::Allow => {}
        RateLimitResult::IpLimit {
            retry_after_secs, ..
        }
        | RateLimitResult::GlobalLimit { retry_after_secs } => {
            return ZabbixResponse::rate_limited(retry_after_secs);
        }
    }

    // Distinguish "not configured" from "stored but undecryptable" (P2.5): the
    // latter is almost always a value left over from before `_token` keys
    // were auto-encrypted, and deserves an operator-visible hint distinct
    // from a plain 401 — without ever logging the value itself.
    let configured_token = match db::repository::get_setting_secure(
        db,
        "monitoring_zabbix_token",
        settings_cipher,
    ) {
        Ok(Some(t)) if !t.is_empty() => Some(t),
        Ok(_) => None,
        Err(_) => {
            warn!(
                target: "security",
                "monitoring_zabbix_token appears stored as plaintext — re-save via settings as secret"
            );
            None
        }
    };
    let Some(configured_token) = configured_token else {
        // Enabled but nothing to authenticate against — cannot ever grant access.
        log_unauthorized(client_ip, "token not configured");
        return ZabbixResponse::unauthorized();
    };
    let authorized = bearer_token.is_some_and(|t| token_matches(t, &configured_token));
    if !authorized {
        log_unauthorized(client_ip, "missing or invalid bearer token");
        return ZabbixResponse::unauthorized();
    }

    let snapshot = collect(db, router_metrics, mesh_peer_store, local_node_id);
    ZabbixResponse {
        status: StatusCode::OK,
        content_type: "text/plain; charset=utf-8",
        body: format_zabbix(&snapshot).into_bytes(),
        cache_control: Some("no-store"),
        retry_after_secs: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    fn sample_metrics() -> ExportedMetrics {
        ExportedMetrics {
            cpu_usage_percent: 12.5,
            cpu_temperature_c: Some(48.3),
            mem_used_mb: 4096,
            mem_total_mb: 16384,
            swap_used_mb: 0,
            swap_total_mb: 2048,
            gpu_count: 1,
            gpu_usage_percent_avg: 30.0,
            gpu_vram_used_mb: 1024,
            gpu_vram_total_mb: 8192,
            router_requests_total: 42,
            router_errors_total: 1,
            router_requests_active: 2,
            router_tokens_per_second: 15,
            router_input_tokens_per_second: 3,
            services_total: 5,
            services_running: 4,
            services_degraded: 0,
            services_failed: 1,
            services_stopped: 0,
            flow_executions_total: 10,
            flow_executions_running: 1,
            flow_executions_completed: 7,
            flow_executions_error: 1,
            flow_executions_cancelled: 1,
            mesh_peers_known: 3,
            mesh_peers_connected: 2,
            fs_total_bytes: 1_000_000_000,
            fs_available_bytes: 400_000_000,
            fs_used_percent: 60.0,
            system_uptime_seconds: 86_400,
            bus_instances: vec![(sample_bus_instance_id(), sample_bus_rollup())],
        }
    }

    fn sample_bus_instance_id() -> BusInstanceId {
        BusInstanceId::parse("tentabus-1a2b3c4d").expect("valid test instance id")
    }

    fn sample_bus_rollup() -> BusMetricsRollup {
        BusMetricsRollup {
            publish_msgs_total: 1_234,
            publish_bytes_total: 987_654,
            consume_msgs_total: 1_100,
            throttled_total: 7,
            fsync_p99_us: 850,
            append_p99_us: 120,
            consumer_lag_max: 42,
            consumer_lag_sum: 88,
            dlq_depth: 3,
            topic_count: 5,
            partition_count: 20,
            disk_bytes: 5_000_000,
            isr_size_min: 2,
            isr_shrink_total: 1,
            leader_epoch_max: 4,
            replication_lag_bytes_max: 65_536,
        }
    }

    /// Every non-empty line is exactly `key value` (one space), the key set
    /// is the stable 47-entry list (31 flat metrics + 16 `tentaflow_bus_*`
    /// samples for the one running instance `sample_metrics` sets up), every
    /// key is a legal Prometheus metric name (`tentaflow_*`, no dots) with
    /// at most one `{bus_instance="..."}` label suffix, and every value
    /// parses as a plain, finite f64 — no thousands separators, no
    /// locale-dependent decimal comma, no NaN/inf.
    #[test]
    fn format_zabbix_produces_stable_key_value_lines() {
        let text = format_zabbix(&sample_metrics());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 47, "expected 47 metric lines, got: {text}");

        let mut keys = Vec::with_capacity(lines.len());
        for line in &lines {
            let mut parts = line.splitn(2, ' ');
            let key = parts.next().expect("key");
            let value = parts
                .next()
                .unwrap_or_else(|| panic!("missing value in line: {line}"));
            assert!(
                key.starts_with("tentaflow_") && !key.contains('.'),
                "key must be a dot-free Prometheus-legal name: {key}"
            );
            let parsed = value
                .parse::<f64>()
                .unwrap_or_else(|e| panic!("value not a plain number ({e}): {line}"));
            assert!(parsed.is_finite(), "non-finite value in line: {line}");
            keys.push(key);
        }
        // Base metric name (label suffix, if any, stripped) — every
        // `tentaflow_bus_*` sample carries `{bus_instance="..."}`, everything
        // else is label-less.
        let base_names: std::collections::HashSet<&str> =
            keys.iter().map(|k| k.split('{').next().unwrap()).collect();

        assert!(base_names.contains("tentaflow_cpu_usage_percent"));
        assert!(base_names.contains("tentaflow_cpu_temperature_c"));
        assert!(base_names.contains("tentaflow_fs_used_percent"));
        assert!(base_names.contains("tentaflow_system_uptime_seconds"));
        assert!(base_names.contains("tentaflow_bus_publish_msgs_total"));
        assert!(base_names.contains("tentaflow_bus_publish_bytes_total"));
        assert!(base_names.contains("tentaflow_bus_consume_msgs_total"));
        assert!(base_names.contains("tentaflow_bus_throttled_total"));
        assert!(base_names.contains("tentaflow_bus_fsync_p99_us"));
        assert!(base_names.contains("tentaflow_bus_append_p99_us"));
        assert!(base_names.contains("tentaflow_bus_consumer_lag_max"));
        assert!(base_names.contains("tentaflow_bus_consumer_lag_sum"));
        assert!(base_names.contains("tentaflow_bus_dlq_depth"));
        assert!(base_names.contains("tentaflow_bus_topic_count"));
        assert!(base_names.contains("tentaflow_bus_partition_count"));
        assert!(base_names.contains("tentaflow_bus_disk_bytes"));
        assert!(base_names.contains("tentaflow_bus_isr_size_min"));
        assert!(base_names.contains("tentaflow_bus_isr_shrink_total"));
        assert!(base_names.contains("tentaflow_bus_leader_epoch_max"));
        assert!(base_names.contains("tentaflow_bus_replication_lag_bytes_max"));
        assert!(
            text.contains("bus_instance=\"tentabus-1a2b3c4d\""),
            "bus samples must carry the bus_instance label, not a bare `instance` label: {text}"
        );
        // The reserved `instance` label name must never appear on its own —
        // every occurrence of `instance="` in the body is part of the
        // `bus_instance="..."` label, never a standalone `instance="..."`.
        for (i, _) in text.match_indices("instance=\"") {
            assert!(
                text[..i].ends_with("bus_"),
                "must never emit the reserved `instance` label name on its own: {text}"
            );
        }
        assert_eq!(
            keys.iter().collect::<std::collections::HashSet<_>>().len(),
            keys.len(),
            "duplicate key"
        );
    }

    /// Zero running TentaBus instances (`ExportedMetrics::bus_instances`
    /// empty) emits none of the 16 `tentaflow_bus_*` lines at all — never a
    /// zero-valued line attributed to no instance (plan-app-platform §3.7).
    #[test]
    fn format_zabbix_omits_bus_metrics_when_no_instance_is_running() {
        let mut metrics = sample_metrics();
        metrics.bus_instances = Vec::new();
        let text = format_zabbix(&metrics);
        assert!(
            !text.contains("tentaflow_bus_"),
            "no bus_* line expected with zero running instances: {text}"
        );
        assert_eq!(text.lines().count(), 47 - 16);
    }

    /// When the platform exposes no CPU temperature sensor, the key is
    /// omitted entirely rather than emitting a sentinel a trigger could
    /// misread as a real reading.
    #[test]
    fn format_zabbix_omits_missing_cpu_temperature() {
        let mut metrics = sample_metrics();
        metrics.cpu_temperature_c = None;
        let text = format_zabbix(&metrics);
        assert!(!text.contains("tentaflow_cpu_temperature_c"));
        assert_eq!(text.lines().count(), 46);
    }

    /// A NaN or infinite reading is dropped from the body instead of being
    /// serialised as a bogus number (P3.3) — `f64::NAN.to_string()` /
    /// `f32::INFINITY` would otherwise produce `NaN`/`inf` literals that are
    /// not valid Prometheus-pattern numbers and could wedge a trigger.
    #[test]
    fn format_zabbix_drops_non_finite_floats() {
        let mut metrics = sample_metrics();
        metrics.cpu_usage_percent = f32::NAN;
        metrics.gpu_usage_percent_avg = f32::INFINITY;
        metrics.fs_used_percent = f32::NEG_INFINITY;
        let text = format_zabbix(&metrics);
        assert!(!text.contains("tentaflow_cpu_usage_percent"));
        assert!(!text.contains("tentaflow_gpu_usage_percent_avg"));
        assert!(!text.contains("tentaflow_fs_used_percent"));
        for line in text.lines() {
            let value = line.splitn(2, ' ').nth(1).expect("value");
            assert!(value.parse::<f64>().is_ok_and(f64::is_finite));
        }
    }

    /// Every metric key `format_zabbix` can emit has a matching Zabbix item
    /// `<key>` in the shipped template (P3.4) — this is exactly the class of
    /// bug that shipped once already (P1.1): a template whose preprocessing
    /// parameters silently matched nothing. The label suffix on a
    /// `tentaflow_bus_*` sample (`{bus_instance="..."}`, plan-app-platform
    /// §3.7) is stripped before comparison: the shipped template still
    /// declares one static, label-less `PROMETHEUS_PATTERN` dependent item
    /// per bus metric NAME (see the module header's operational-impact
    /// note) — this test only guards the metric-name contract, not the
    /// template's ability to disambiguate multiple running instances.
    #[test]
    fn template_item_keys_match_format_zabbix_keys() {
        let all_present = ExportedMetrics {
            cpu_temperature_c: Some(48.3),
            ..zeroed_metrics()
        };
        let body = format_zabbix(&all_present);
        let emitted: std::collections::HashSet<&str> = body
            .lines()
            .map(|l| l.split(' ').next().unwrap().split('{').next().unwrap())
            .collect();

        let template_keys = template_item_keys();
        // The master (raw scrape) item's own key is not an exported metric.
        let template_keys: std::collections::HashSet<&str> = template_keys
            .iter()
            .map(String::as_str)
            .filter(|k| *k != "tentaflow_metrics_raw")
            .collect();

        assert_eq!(
            emitted, template_keys,
            "format_zabbix keys and template dependent-item keys must match exactly"
        );
    }

    fn zeroed_metrics() -> ExportedMetrics {
        ExportedMetrics {
            cpu_usage_percent: 0.0,
            cpu_temperature_c: None,
            mem_used_mb: 0,
            mem_total_mb: 0,
            swap_used_mb: 0,
            swap_total_mb: 0,
            gpu_count: 0,
            gpu_usage_percent_avg: 0.0,
            gpu_vram_used_mb: 0,
            gpu_vram_total_mb: 0,
            router_requests_total: 0,
            router_errors_total: 0,
            router_requests_active: 0,
            router_tokens_per_second: 0,
            router_input_tokens_per_second: 0,
            services_total: 0,
            services_running: 0,
            services_degraded: 0,
            services_failed: 0,
            services_stopped: 0,
            flow_executions_total: 0,
            flow_executions_running: 0,
            flow_executions_completed: 0,
            flow_executions_error: 0,
            flow_executions_cancelled: 0,
            mesh_peers_known: 0,
            mesh_peers_connected: 0,
            fs_total_bytes: 0,
            fs_available_bytes: 0,
            fs_used_percent: 0.0,
            system_uptime_seconds: 0,
            // One all-zero-valued instance rather than an empty `Vec` — this
            // fixture's job (`template_item_keys_match_format_zabbix_keys`)
            // is to emit every `tentaflow_bus_*` metric NAME at least once so
            // the template-key comparison sees the full set; zero running
            // instances would instead emit none of them at all (see
            // `format_zabbix_omits_bus_metrics_when_no_instance_is_running`).
            bus_instances: vec![(sample_bus_instance_id(), BusMetricsRollup::default())],
        }
    }

    /// Extracts every `<key>...</key>` text value in the template (master +
    /// dependent items alike — a set naturally collapses the master key's
    /// repeated appearances inside each dependent item's `<master_item>`).
    fn template_item_keys() -> std::collections::HashSet<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let xml = std::str::from_utf8(ZABBIX_TEMPLATE_XML).expect("template is utf8");
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut keys = std::collections::HashSet::new();
        let mut in_key = false;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Eof) => break,
                Ok(Event::Start(e)) if e.name().as_ref() == b"key" => in_key = true,
                Ok(Event::End(e)) if e.name().as_ref() == b"key" => in_key = false,
                Ok(Event::Text(t)) if in_key => {
                    keys.insert(t.unescape().expect("valid text").into_owned());
                }
                Err(e) => panic!("template xml parse error: {e}"),
                _ => {}
            }
            buf.clear();
        }
        keys
    }

    #[test]
    fn token_matches_accepts_only_the_exact_token() {
        assert!(token_matches("secret-token-value", "secret-token-value"));
        assert!(!token_matches("secret-token-valuex", "secret-token-value"));
        assert!(!token_matches("wrong", "secret-token-value"));
        assert!(!token_matches("", "secret-token-value"));
        // An empty `expected` never matches — including against an empty
        // `provided` (P3.7): "no token configured" must never look like "any
        // caller may provide the empty string".
        assert!(!token_matches("", ""));
    }

    fn fresh_db() -> DbPool {
        db::init(Path::new(":memory:")).expect("in-memory db")
    }

    fn test_cipher() -> SettingsCipher {
        SettingsCipher::new(&[7u8; 32])
    }

    /// Feature disabled (default) -> 404, regardless of any token header —
    /// the route does not exist until an operator turns it on.
    #[test]
    fn disabled_by_default_is_not_found() {
        let pool = fresh_db();
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix",
            Some("anything"),
            "203.0.113.1",
            &pool,
            &test_cipher(),
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
        assert!(resp.body.is_empty());
    }

    /// The template route is disabled too when the feature flag is off — it
    /// is not a secret-free bypass of the enable/disable switch, only of the
    /// per-request token (P2.3).
    #[test]
    fn disabled_by_default_hides_template_too() {
        let pool = fresh_db();
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix/template",
            None,
            "203.0.113.1",
            &pool,
            &test_cipher(),
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
    }

    /// Enabled but no token configured -> 401: nothing could ever
    /// authenticate, so it fails closed instead of opening the route.
    #[test]
    fn enabled_without_configured_token_is_unauthorized() {
        let pool = fresh_db();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix",
            Some("anything"),
            "203.0.113.2",
            &pool,
            &test_cipher(),
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::UNAUTHORIZED);
        assert!(resp.body.is_empty());
    }

    #[test]
    fn missing_bearer_is_unauthorized() {
        let pool = fresh_db();
        let cipher = test_cipher();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        db::repository::set_setting_secure(
            &pool,
            "monitoring_zabbix_token",
            "correct-token",
            &cipher,
        )
        .expect("set token");
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix",
            None,
            "203.0.113.3",
            &pool,
            &cipher,
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::UNAUTHORIZED);
        assert!(resp.body.is_empty());
    }

    #[test]
    fn wrong_bearer_is_unauthorized() {
        let pool = fresh_db();
        let cipher = test_cipher();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        db::repository::set_setting_secure(
            &pool,
            "monitoring_zabbix_token",
            "correct-token",
            &cipher,
        )
        .expect("set token");
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix",
            Some("wrong-token"),
            "203.0.113.4",
            &pool,
            &cipher,
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::UNAUTHORIZED);
        assert!(resp.body.is_empty());
    }

    /// Correct bearer -> 200, `Cache-Control: no-store`, body is parseable
    /// `key value` text matching `format_zabbix`'s output shape (this
    /// exercises `collect()` against a real in-memory DB + fresh
    /// RouterMetrics + empty MeshPeerStore, so all zero-state fallbacks in
    /// `service_counts`/`flow_execution_counts` run).
    #[test]
    fn correct_bearer_returns_metrics_body() {
        let pool = fresh_db();
        let cipher = test_cipher();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        db::repository::set_setting_secure(
            &pool,
            "monitoring_zabbix_token",
            "correct-token",
            &cipher,
        )
        .expect("set token");
        let router_metrics = RouterMetrics::new();
        router_metrics.record_request();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix",
            Some("correct-token"),
            "203.0.113.5",
            &pool,
            &cipher,
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.content_type, "text/plain; charset=utf-8");
        assert_eq!(resp.cache_control, Some("no-store"));
        let body = String::from_utf8(resp.body).expect("utf8 body");
        assert!(body.contains("tentaflow_router_requests_total 1"));
        assert!(body.contains("tentaflow_router_requests_active 1"));
        assert!(body.contains("tentaflow_services_total 0"));
        for line in body.lines() {
            let mut parts = line.splitn(2, ' ');
            let _key = parts.next().expect("key");
            let value = parts.next().expect("value");
            value.parse::<f64>().expect("numeric value");
        }
    }

    /// A wrong/missing bearer against the metrics route is 401 even without
    /// `monitoring_zabbix_enabled` friction — but the TEMPLATE route never
    /// asks for a bearer at all (P2.3): it is public once the feature is on.
    #[test]
    fn template_route_requires_no_token_and_returns_xml() {
        let pool = fresh_db();
        let cipher = test_cipher();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();

        let ok = handle_request(
            "/v1/metrics/zabbix/template",
            None,
            "203.0.113.6",
            &pool,
            &cipher,
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(ok.status, StatusCode::OK);
        assert_eq!(ok.content_type, "application/xml");
        assert_eq!(ok.cache_control, None);
        let xml = String::from_utf8(ok.body).expect("utf8 xml");
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<zabbix_export>"));
        assert!(xml.contains("tentaflow_cpu_usage_percent"));
    }

    /// An unknown path under the same auth is a 404, not a 401 — token
    /// validity and route existence are independent checks.
    #[test]
    fn unknown_path_with_valid_token_is_not_found() {
        let pool = fresh_db();
        let cipher = test_cipher();
        db::repository::set_setting(&pool, "monitoring_zabbix_enabled", "1").expect("set enabled");
        db::repository::set_setting_secure(
            &pool,
            "monitoring_zabbix_token",
            "correct-token",
            &cipher,
        )
        .expect("set token");
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        let resp = handle_request(
            "/v1/metrics/zabbix/unknown",
            Some("correct-token"),
            "203.0.113.7",
            &pool,
            &cipher,
            &router_metrics,
            &mesh,
            "local-node",
        );
        assert_eq!(resp.status, StatusCode::NOT_FOUND);
    }

    /// `MeshPeerStore::seed_local` puts this node's own row in the store —
    /// `collect()` must exclude it from the peer counts (P2.1), or a
    /// single-node install would report itself as one known peer.
    #[test]
    fn collect_excludes_the_local_node_from_peer_counts() {
        let pool = fresh_db();
        let router_metrics = RouterMetrics::new();
        let mesh = MeshPeerStore::new();
        mesh.seed_local(
            "local-node",
            "local-host".to_string(),
            "linux".to_string(),
            "linux".to_string(),
            0,
            0,
            vec![],
            vec![],
            false,
            String::new(),
        );

        let snapshot = collect(&pool, &router_metrics, &mesh, "local-node");
        assert_eq!(snapshot.mesh_peers_known, 0);
        assert_eq!(snapshot.mesh_peers_connected, 0);
    }

    // ---- collect_bus_metrics: multi-instance (plan-app-platform §3.7) ----
    //
    // These exercise `collect_bus_metrics` against REAL `BusService`s in the
    // process-wide `bus::running_instances()` registry — the same registry
    // every OTHER `#[cfg(test)]` module in this crate's `--lib` binary
    // shares (`bus::reactor::tests::registry_bus_service` documents the same
    // convention). `REGISTRY_TEST_LOCK` below serializes the three tests
    // that touch it so none of them ever observes a sibling's still-
    // registered instance; this file's own GATE command (`-- metrics_export`)
    // already limits which OTHER tests in the binary could run alongside
    // these to none (no other module's test path contains that substring).

    static REGISTRY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct AllowAllTestAuthorizer;

    impl crate::bus::BusAuthorizer for AllowAllTestAuthorizer {
        fn authorize(
            &self,
            _ctx: &crate::bus::BusCallContext,
            _action: crate::bus::BusAction,
            _topic: &str,
        ) -> Result<(), crate::bus::BusServiceError> {
            Ok(())
        }

        fn authorize_group(
            &self,
            _ctx: &crate::bus::BusCallContext,
            _action: crate::bus::BusAction,
            _topic: &str,
            _group: &str,
        ) -> Result<(), crate::bus::BusServiceError> {
            Ok(())
        }

        fn generation(&self) -> u64 {
            0
        }
    }

    /// Registers a real `BusService` under `id` in the process-wide
    /// registry (`bus::init_instance`), sharing `db` (mirrors production:
    /// one platform `tentaflow.db` shared by every instance, `bus_topics`
    /// scoped by `instance_id`) but with its OWN temp `bus_dir` and its OWN
    /// in-memory `local_db` (mirrors production: one `tentabus.db` per
    /// instance). Caller must `crate::bus::stop_instance(&id)` when done —
    /// same convention as `bus::reactor::tests::registry_bus_service`.
    fn registry_instance(
        id: BusInstanceId,
        db: DbPool,
    ) -> (tempfile::TempDir, Arc<crate::bus::BusService>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
        crate::bus::db::migrate(&local_conn).expect("migrate local db");
        let local_db: DbPool = Arc::new(crate::db::Db::from_connection(local_conn));
        let svc = crate::bus::init_instance(crate::bus::BusInitConfig {
            instance_id: id,
            local_db,
            bus_dir: dir.path().join("bus"),
            db,
            authorizer: Arc::new(AllowAllTestAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: crate::bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("init registry instance");
        (dir, svc)
    }

    fn bus_ctx_for(instance_id: &BusInstanceId) -> crate::bus::BusCallContext {
        crate::bus::BusCallContext {
            instance_id: instance_id.clone(),
            org_id: "org-default".to_string(),
            actor: Some("tester".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    fn shared_bus_db() -> DbPool {
        let db = fresh_db();
        db::repository::bus_test_support::create_bus_tables(&db).expect("bus fixture tables");
        db
    }

    fn rollup_for<'a>(
        results: &'a [(BusInstanceId, BusMetricsRollup)],
        id: &BusInstanceId,
    ) -> &'a BusMetricsRollup {
        results
            .iter()
            .find(|(rid, _)| rid == id)
            .map(|(_, r)| r)
            .unwrap_or_else(|| panic!("instance {id} present in results"))
    }

    /// Two running instances -> two entries, each keyed by its OWN
    /// `BusInstanceId`, each with a DISTINCT `disk_bytes` reading taken from
    /// its OWN `<instance data dir>/log` root (`svc.bus_dir()`) — never one
    /// instance's directory being summed into the other's rollup. Compares
    /// against each instance's OWN baseline (`BusService::new` already
    /// leaves real bytes on disk before any topic exists) rather than an
    /// absolute value, so the test only asserts the one thing this change
    /// actually guarantees: `dir_size_bytes` is walked per-instance.
    #[test]
    fn collect_bus_metrics_reports_one_rollup_per_running_instance_with_distinct_disk_bytes() {
        let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let id_a = BusInstanceId::parse("tentabus-5e000001").expect("valid instance id");
        let id_b = BusInstanceId::parse("tentabus-5e000002").expect("valid instance id");
        let db = shared_bus_db();
        let (_dir_a, svc_a) = registry_instance(id_a.clone(), db.clone());
        let (_dir_b, svc_b) = registry_instance(id_b.clone(), db.clone());

        let baseline = collect_bus_metrics(&db);
        let base_a = rollup_for(&baseline, &id_a).disk_bytes;
        let base_b = rollup_for(&baseline, &id_b).disk_bytes;

        // Distinct, deterministic ADDITIONS to each instance's OWN `bus_dir`
        // — independent of the engine's own segment format.
        std::fs::create_dir_all(svc_a.bus_dir()).expect("create instance A bus dir");
        std::fs::write(svc_a.bus_dir().join("seed.bin"), vec![0u8; 111]).expect("write A padding");
        std::fs::create_dir_all(svc_b.bus_dir()).expect("create instance B bus dir");
        std::fs::write(svc_b.bus_dir().join("seed.bin"), vec![0u8; 222]).expect("write B padding");

        let results = collect_bus_metrics(&db);
        let rollup_a = rollup_for(&results, &id_a);
        let rollup_b = rollup_for(&results, &id_b);

        assert_eq!(rollup_a.disk_bytes, base_a + 111);
        assert_eq!(rollup_b.disk_bytes, base_b + 222);
        assert_ne!(
            rollup_a.disk_bytes, rollup_b.disk_bytes,
            "each instance's disk_bytes must reflect only its OWN bus_dir"
        );

        // The label rendering itself: two distinct `bus_instance` values.
        let mut metrics = zeroed_metrics();
        metrics.bus_instances = results;
        let text = format_zabbix(&metrics);
        assert!(text.contains("bus_instance=\"tentabus-5e000001\""));
        assert!(text.contains("bus_instance=\"tentabus-5e000002\""));

        crate::bus::stop_instance(&id_a);
        crate::bus::stop_instance(&id_b);
    }

    /// One instance's topic/partition counts never include another
    /// instance's topics, even though both share the SAME underlying
    /// `bus_topics` table (scoped only by the `instance_id` column) — the
    /// HARD isolation requirement this whole conversion exists for.
    #[test]
    fn collect_bus_metrics_topic_counts_never_leak_across_instances() {
        let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let id_a = BusInstanceId::parse("tentabus-5e000003").expect("valid instance id");
        let id_b = BusInstanceId::parse("tentabus-5e000004").expect("valid instance id");
        let db = shared_bus_db();
        let (_dir_a, svc_a) = registry_instance(id_a.clone(), db.clone());
        let (_dir_b, svc_b) = registry_instance(id_b.clone(), db.clone());

        let ctx_a = bus_ctx_for(&id_a);
        svc_a
            .create_topic(
                &ctx_a,
                "orders.created",
                crate::bus::topics::TopicOptions::default(),
            )
            .expect("create topic on instance A");
        // Instance B creates NO topics at all.

        let results = collect_bus_metrics(&db);
        let rollup_a = rollup_for(&results, &id_a);
        let rollup_b = rollup_for(&results, &id_b);

        assert_eq!(
            rollup_a.topic_count, 1,
            "instance A owns exactly the one topic it created"
        );
        assert_eq!(
            rollup_b.topic_count, 0,
            "instance B must not see instance A's topic through the shared bus_topics table"
        );

        let _ = &svc_b; // kept alive (and registered) for the duration of the assertions above
        crate::bus::stop_instance(&id_a);
        crate::bus::stop_instance(&id_b);
    }

    /// Zero running instances -> an empty `Vec`, never an error and never a
    /// panic (plan-app-platform §3.7).
    #[test]
    fn collect_bus_metrics_returns_exactly_one_rollup_per_running_instance() {
        let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // `REGISTRY_TEST_LOCK` only serialises THIS module. `bus::BUS_INSTANCES`
        // is process-wide and other modules' test suites (`api::bus_rest`,
        // `addon::host_functions::bus`) register their own engines into it
        // from the same test binary without taking this lock, so "the registry
        // is empty" is not a property any test here can assert — an earlier
        // version of this test did, passed when the filter was `metrics_export`
        // alone, and failed the moment the three W8 filters ran together.
        //
        // The invariant that IS true regardless of who else is registered:
        // one rollup out per running instance in, each labelled with that
        // instance's own id and none repeated. With an empty registry this
        // still asserts the zero case; `format_zabbix_omits_bus_metrics_when_
        // no_instance_is_running` covers the observable zero-instance output
        // directly, without depending on global state at all.
        let db = shared_bus_db();
        let running = crate::bus::running_instances();
        let results = collect_bus_metrics(&db);
        assert_eq!(
            results.len(),
            running.len(),
            "expected one rollup per running instance, got {} for {} running",
            results.len(),
            running.len()
        );
        let mut ids: Vec<String> = results.iter().map(|(id, _)| id.as_str().to_string()).collect();
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(before, ids.len(), "an instance was rolled up twice: {ids:?}");
        let mut expected: Vec<String> = running
            .iter()
            .map(|svc| svc.instance_id().to_string())
            .collect();
        expected.sort();
        assert_eq!(ids, expected);
    }
}
