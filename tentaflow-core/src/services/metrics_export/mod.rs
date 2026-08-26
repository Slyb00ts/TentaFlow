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

use hyper::StatusCode;
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::api::rate_limit::{rate_limiter, RateLimitResult};
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
    }
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
/// integers).
fn push_int(lines: &mut Vec<(&'static str, String)>, key: &'static str, value: u64) {
    lines.push((key, value.to_string()));
}

/// Pushes a float-valued line, but only when `value` is finite (P3.3) — a
/// NaN or +/-inf reading (e.g. a `0/0` percentage on a since-fixed source)
/// must not reach Zabbix as a bogus number a trigger could act on; skipping
/// the line is indistinguishable from "not collected", which is the correct,
/// safe reading for a scrape that could not produce a real value.
fn push_float(lines: &mut Vec<(&'static str, String)>, key: &'static str, value: f32) {
    if value.is_finite() {
        lines.push((key, format!("{value:.2}")));
    }
}

/// Renders a snapshot as Zabbix HTTP-agent / Prometheus-exposition lines:
/// `key value\n`, one metric per line, `tentaflow_*` (underscored — see the
/// module header on why dots would be wrong here). Key order is fixed
/// (matches the field order in [`ExportedMetrics`] /
/// `assets/zabbix-template.xml`) so a diff between two scrapes is stable;
/// `tentaflow_cpu_temperature_c` is omitted entirely when the platform
/// exposes no CPU temperature sensor (or a non-finite reading) rather than
/// emitting a sentinel value a trigger could mistake for a real reading.
pub fn format_zabbix(m: &ExportedMetrics) -> String {
    let mut lines: Vec<(&'static str, String)> = Vec::with_capacity(31);
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

    let mut out = String::new();
    for (key, value) in lines {
        out.push_str(key);
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
        }
    }

    /// Every non-empty line is exactly `key value` (one space), the key set
    /// is the stable 31-entry list, every key is a legal label-less
    /// Prometheus metric name (`tentaflow_*`, no dots), and every value
    /// parses as a plain, finite f64 — no thousands separators, no
    /// locale-dependent decimal comma, no NaN/inf.
    #[test]
    fn format_zabbix_produces_stable_key_value_lines() {
        let text = format_zabbix(&sample_metrics());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 31, "expected 31 metric lines, got: {text}");

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

        assert!(keys.contains(&"tentaflow_cpu_usage_percent"));
        assert!(keys.contains(&"tentaflow_cpu_temperature_c"));
        assert!(keys.contains(&"tentaflow_fs_used_percent"));
        assert!(keys.contains(&"tentaflow_system_uptime_seconds"));
        assert_eq!(
            keys.iter().collect::<std::collections::HashSet<_>>().len(),
            keys.len(),
            "duplicate key"
        );
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
        assert_eq!(text.lines().count(), 30);
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
    /// parameters silently matched nothing.
    #[test]
    fn template_item_keys_match_format_zabbix_keys() {
        let all_present = ExportedMetrics {
            cpu_temperature_c: Some(48.3),
            ..zeroed_metrics()
        };
        let body = format_zabbix(&all_present);
        let emitted: std::collections::HashSet<&str> =
            body.lines().map(|l| l.split(' ').next().unwrap()).collect();

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
}
