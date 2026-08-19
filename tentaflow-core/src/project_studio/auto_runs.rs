// ===== File: project_studio/auto_runs.rs — automated test runs on the test-runner service (F3) =====
//
// Owns everything between "the user pressed Uruchom" and "the run row is
// terminal": runner discovery with a short health cache, the submission body
// (the ONLY place the environment secret is decrypted), the 2 s poller that
// mirrors the runner's snapshot into `test_run_items` / `test_run_steps` /
// `run_artifacts` in ONE transaction per poll, artifact download, the watchdog
// that ends a run whose runner stopped answering, the cancel registry and the
// lazy reconciliation of runs orphaned by a process restart.
//
// HTTP goes through `ureq` inside `spawn_blocking` (never `reqwest::blocking`,
// which builds a nested tokio runtime and panics on drop — see
// `ml_studio::train_autogluon`). The submission body is NEVER logged: it
// carries the decrypted environment secret.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::models::{AutoRunItemRecord, AutoRunMetaRecord, RunArtifactRecord};
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};

/// Poll interval of the runner status snapshot.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Consecutive failed polls after which the watchdog ends the run as 'error'.
pub const MAX_FAILED_POLLS: u32 = 15;
/// Absolute wall-clock budget of one automated run.
pub const MAX_RUN_SECS: i64 = 4 * 60 * 60;
/// TTL of a cached `GET /health` answer.
const HEALTH_TTL_MS: i64 = 30_000;
/// Per-request timeout of runner HTTP calls.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on a single downloaded artifact.
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on the NUMBER of artifacts one run may pull down. A runner (or
/// a test looping over screenshots) can otherwise fill the project directory
/// one 64 MiB file at a time.
pub const MAX_ARTIFACTS_PER_RUN: usize = 500;
/// Upper bound on the total artifact bytes of one run.
pub const MAX_ARTIFACT_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Upper bound on one status snapshot.
const MAX_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
/// Ephemeral try-run lifetime.
pub const TRY_RUN_TTL: Duration = Duration::from_secs(300);

/// Core setting gating runs on a runner that reports `isolated: false` (a
/// native deployment executes untrusted scripts without a container boundary).
/// Default OFF — an admin has to turn it on deliberately.
pub const ALLOW_UNISOLATED_SETTING: &str = "project_studio_allow_unisolated_runner";

/// Engine id of the runner service manifest.
pub const RUNNER_ENGINE_ID: &str = "test-runner";
/// Service category the runner is registered under.
pub const RUNNER_CATEGORY: &str = "tools";

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio auto runs read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio auto runs write: {e}")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

// =============================================================================
// Cancel registry
// =============================================================================

/// Anulowanie biezacych zadan tego procesu. Wspolny typ z
/// `services::cancel_registry` — kazda z trzech kopii tej mapy miala wlasna
/// implementacje, a ta w Project Studio wprost nazywala sie lustrem benchmarku.
static AUTO_RUN_CANCEL: crate::services::cancel_registry::CancelRegistry =
    crate::services::cancel_registry::CancelRegistry::new();

fn register_cancel(run_id: &str) -> Arc<AtomicBool> {
    AUTO_RUN_CANCEL.register(run_id)
}

fn unregister_cancel(run_id: &str) {
    AUTO_RUN_CANCEL.unregister(run_id)
}

/// Flags a live run for cancellation. `false` = this process does not own it.
pub fn signal_cancel(run_id: &str) -> bool {
    AUTO_RUN_CANCEL.signal(run_id)
}

pub fn is_running(run_id: &str) -> bool {
    AUTO_RUN_CANCEL.is_registered(run_id)
}

fn is_live(run_id: &str) -> bool {
    AUTO_RUN_CANCEL.is_registered(run_id)
}

// =============================================================================
// Runner discovery + health
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerToolchainInfo {
    pub language: String,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerHealth {
    #[serde(default)]
    pub isolated: bool,
    #[serde(default)]
    pub toolchains: Vec<RunnerToolchainInfo>,
}

/// A discovered runner service. `health` is `None` when `GET /health` failed —
/// the service row says running but the process is not answering.
#[derive(Debug, Clone)]
pub struct DiscoveredRunner {
    pub service_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub endpoint_url: String,
    pub status: String,
    pub health: Option<RunnerHealth>,
}

fn health_cache() -> &'static DashMap<String, (i64, Option<RunnerHealth>)> {
    static CACHE: OnceLock<DashMap<String, (i64, Option<RunnerHealth>)>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

/// `GET /health` with a 30 s cache keyed by endpoint. The runners list is
/// polled by the UI, so an uncached probe per render would hammer the service.
pub fn runner_health(endpoint_url: &str) -> Option<RunnerHealth> {
    let now = now_ms();
    if let Some(entry) = health_cache().get(endpoint_url) {
        let (ts, cached) = entry.value();
        if now - ts < HEALTH_TTL_MS {
            return cached.clone();
        }
    }
    let url = format!("{}/health", endpoint_url.trim_end_matches('/'));
    let fetched = http_agent()
        .get(&url)
        .call()
        .ok()
        .and_then(|mut resp| resp.body_mut().read_json::<RunnerHealth>().ok());
    health_cache().insert(endpoint_url.to_string(), (now, fetched.clone()));
    fetched
}

/// Lists running test-runner services with their advertised capabilities.
/// Blocking (SQLite + HTTP) — call from `spawn_blocking`.
pub fn list_runners(core_db: &DbPool) -> Result<Vec<DiscoveredRunner>> {
    let rows = {
        let conn = core_db.read().map_err(read_err)?;
        crate::services_repo::services::list_by_category(
            &conn,
            RUNNER_CATEGORY,
            Some(RUNNER_ENGINE_ID),
        )?
    };
    let mut out = Vec::with_capacity(rows.len());
    let mut live_endpoints: HashSet<String> = HashSet::new();
    for row in rows {
        let Some(endpoint_url) = row.endpoint_url.clone().filter(|u| !u.is_empty()) else {
            continue;
        };
        let health = runner_health(&endpoint_url);
        live_endpoints.insert(endpoint_url.clone());
        out.push(DiscoveredRunner {
            service_id: row.id.to_string(),
            engine_id: row.engine_id,
            display_name: row.display_name,
            status: row.status.as_db_tag().to_string(),
            endpoint_url,
            health,
        });
    }
    // Drop cached health of endpoints that no longer exist (service deleted or
    // redeployed on another port) — the cache is keyed by url and would grow
    // for the lifetime of the process.
    health_cache().retain(|endpoint, _| live_endpoints.contains(endpoint));
    Ok(out)
}

/// Picks the runner for a run: the explicitly requested service, otherwise the
/// first healthy one that advertises `language`. A runner without a live
/// `/health` answer is never selected — submitting to it would only produce a
/// watchdog error two minutes later.
pub fn select_runner(
    runners: Vec<DiscoveredRunner>,
    requested_service_id: &str,
    language: &str,
) -> Result<DiscoveredRunner> {
    if !requested_service_id.is_empty() {
        let chosen = runners
            .into_iter()
            .find(|r| r.service_id == requested_service_id)
            .ok_or_else(|| anyhow!("the selected test runner is not running"))?;
        if chosen.health.is_none() {
            bail!("the selected test runner is not answering /health");
        }
        return Ok(chosen);
    }
    runners
        .into_iter()
        .find(|r| {
            r.health.as_ref().is_some_and(|h| {
                h.toolchains
                    .iter()
                    .any(|t| t.language.eq_ignore_ascii_case(language))
            })
        })
        .ok_or_else(|| {
            anyhow!("no running test runner advertises the '{language}' toolchain")
        })
}

/// Whether a runner may execute untrusted scripts on this node: an isolated
/// (container) runner always may, a native one only with the explicit org
/// setting. Returns the refusal reason when it may not.
pub fn isolation_refusal(core_db: &DbPool, runner: &DiscoveredRunner) -> Option<String> {
    let isolated = runner.health.as_ref().is_some_and(|h| h.isolated);
    if isolated {
        return None;
    }
    let allowed = crate::db::repository::get_setting(core_db, ALLOW_UNISOLATED_SETTING)
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if allowed {
        None
    } else {
        Some(format!(
            "runner '{}' runs without container isolation; an administrator must enable \
             '{ALLOW_UNISOLATED_SETTING}' before untrusted test code may run on it",
            runner.display_name
        ))
    }
}

// =============================================================================
// Runner protocol
// =============================================================================

/// One item of the submission body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubmitItem {
    pub item_id: String,
    pub kind: String,
    pub language: String,
    pub content: serde_json::Value,
    pub config: serde_json::Value,
}

/// Environment block of the submission body. `secret` is plaintext — this
/// struct exists only for the duration of one request and must never be logged.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubmitEnvironment {
    pub base_url: String,
    pub auth_type: String,
    pub secret: String,
    pub extra_headers: serde_json::Value,
    pub host_allowlist: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SubmitResponse {
    job_id: String,
}

/// `POST /runs`. Returns the runner-side job id.
pub fn submit_run(
    endpoint_url: &str,
    run_id: &str,
    items: &[SubmitItem],
    environment: &SubmitEnvironment,
) -> Result<String> {
    let url = format!("{}/runs", endpoint_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "run_id": run_id,
        "items": items,
        "environment": environment,
    });
    let mut resp = http_agent().post(&url).send_json(&body).map_err(|e| {
        // The error message must not echo the body — it carries the secret.
        anyhow!("POST {url} failed: {}", short_http_error(e))
    })?;
    let parsed: SubmitResponse = resp
        .body_mut()
        .read_json()
        .map_err(|e| anyhow!("decode /runs response: {e}"))?;
    Ok(parsed.job_id)
}

fn short_http_error(err: ureq::Error) -> String {
    match err {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        other => other.to_string(),
    }
}

/// `POST /runs/{job}/cancel` — best effort, a dead runner simply stays dead.
pub fn cancel_runner_job(endpoint_url: &str, job_id: &str) {
    let url = format!(
        "{}/runs/{}/cancel",
        endpoint_url.trim_end_matches('/'),
        job_id
    );
    let _ = http_agent().post(&url).send_empty();
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotStep {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotArtifact {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    pub rel_path: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub mime: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotItem {
    pub item_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub steps: Vec<SnapshotStep>,
    #[serde(default)]
    pub artifacts: Vec<SnapshotArtifact>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SnapshotPerf {
    #[serde(default)]
    pub summary: Vec<serde_json::Value>,
    #[serde(default)]
    pub timeline: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerSnapshot {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub items: Vec<SnapshotItem>,
    #[serde(default)]
    pub perf: SnapshotPerf,
}

/// `GET /runs/{job}/status`.
pub fn poll_snapshot(endpoint_url: &str, job_id: &str) -> Result<RunnerSnapshot> {
    let url = format!(
        "{}/runs/{}/status",
        endpoint_url.trim_end_matches('/'),
        job_id
    );
    let mut resp = http_agent()
        .get(&url)
        .call()
        .map_err(|e| anyhow!("GET {url} failed: {}", short_http_error(e)))?;
    resp.body_mut()
        .with_config()
        .limit(MAX_SNAPSHOT_BYTES)
        .read_json()
        .map_err(|e| anyhow!("decode /status response: {e}"))
}

/// Downloads one artifact into `<dir_path>/runs/<run_id>/<rel_path>` and
/// returns `(absolute path, sha256, size)`. `rel_path` comes from the runner,
/// so it is re-validated here: no absolute path, no `..`, no empty segment.
/// `secret` is the plaintext environment secret and is redacted out of textual
/// artifacts before they hit the disk (see `redact_secret`).
pub fn download_artifact(
    endpoint_url: &str,
    job_id: &str,
    run_dir: &Path,
    rel_path: &str,
    mime: &str,
    secret: &str,
) -> Result<(PathBuf, String, u64)> {
    let relative = safe_rel_path(rel_path)?;
    // Each segment is percent-encoded on its own: a space or a '#' in a
    // Playwright screenshot name would otherwise truncate or corrupt the path.
    let encoded: Vec<String> = relative
        .iter()
        .map(|segment| urlencoding::encode(&segment.to_string_lossy()).into_owned())
        .collect();
    let url = format!(
        "{}/runs/{}/artifacts/{}",
        endpoint_url.trim_end_matches('/'),
        urlencoding::encode(job_id),
        encoded.join("/")
    );
    let mut resp = http_agent()
        .get(&url)
        .call()
        .map_err(|e| anyhow!("GET artifact failed: {}", short_http_error(e)))?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(MAX_ARTIFACT_BYTES)
        .read_to_vec()
        .map_err(|e| anyhow!("read artifact: {e}"))?;
    let bytes = redact_secret(bytes, mime, secret);
    let target = run_dir.join(&relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, &bytes)?;
    Ok((
        target,
        hex::encode(Sha256::digest(&bytes)),
        bytes.len() as u64,
    ))
}

/// True for artifact types a script's traceback can end up in as readable text
/// (console logs, junit xml, har/json reports). An empty mime counts as
/// textual: the redaction below is a no-op on bytes that are not valid UTF-8.
fn is_textual_mime(mime: &str) -> bool {
    let mime = mime.trim().to_ascii_lowercase();
    mime.is_empty()
        || mime.starts_with("text/")
        || mime.starts_with("application/json")
        || mime.starts_with("application/xml")
        || mime.starts_with("application/xhtml+xml")
}

/// Replaces the environment secret with `***` in a textual artifact. The
/// runner hands the secret to the test script, and an `httpx` traceback prints
/// the whole request including its Authorization header — artifacts are
/// readable by testers, the secret is not. Very short secrets are left alone:
/// replacing a 1-3 character string would corrupt unrelated content.
fn redact_secret(bytes: Vec<u8>, mime: &str, secret: &str) -> Vec<u8> {
    if secret.len() < 4 || !is_textual_mime(mime) {
        return bytes;
    }
    match String::from_utf8(bytes) {
        Ok(text) if text.contains(secret) => text.replace(secret, "***").into_bytes(),
        Ok(text) => text.into_bytes(),
        Err(e) => e.into_bytes(),
    }
}

fn safe_rel_path(rel_path: &str) -> Result<PathBuf> {
    if rel_path.is_empty() || rel_path.contains('\0') {
        bail!("invalid artifact path");
    }
    let candidate = Path::new(rel_path);
    if candidate.is_absolute() {
        bail!("invalid artifact path");
    }
    let mut out = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => bail!("invalid artifact path"),
        }
    }
    if out.as_os_str().is_empty() {
        bail!("invalid artifact path");
    }
    Ok(out)
}

/// Directory holding the artifacts of one run.
pub fn run_artifact_dir(dir_path: &Path, run_id: &str) -> PathBuf {
    dir_path.join("runs").join(run_id)
}

// =============================================================================
// auto_run_meta / run_artifacts / automated items — SQL
// =============================================================================

const META_COLS: &str = "run_id, environment_id, runner_service_id, runner_endpoint, \
     runner_job_id, perf_profile_json, perf_summary_json, perf_timeline_json, last_poll_at, \
     failed_polls, watchdog_deadline_ms";

fn read_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutoRunMetaRecord> {
    Ok(AutoRunMetaRecord {
        run_id: row.get(0)?,
        environment_id: row.get(1)?,
        runner_service_id: row.get(2)?,
        runner_endpoint: row.get(3)?,
        runner_job_id: row.get(4)?,
        perf_profile_json: row.get(5)?,
        perf_summary_json: row.get(6)?,
        perf_timeline_json: row.get(7)?,
        last_poll_at: row.get(8)?,
        failed_polls: row.get::<_, i64>(9)? as u32,
        watchdog_deadline_ms: row.get(10)?,
    })
}

pub fn get_meta(pool: &DbPool, run_id: &str) -> Result<Option<AutoRunMetaRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {META_COLS} FROM auto_run_meta WHERE run_id = ?1"),
        params![run_id],
        read_meta,
    )
    .optional()
    .map_err(Into::into)
}

/// Automated items of a run: the shared item row joined with the case's
/// kind/language (the runner-facing dimensions manual items do not have).
/// `duration_ms` is read from the runner metadata JSON stored in
/// `tester_config` — schema v3 has only a whole-second column, and reporting a
/// 240 ms Playwright step as "0 s" would make the live view useless.
pub fn list_auto_items(pool: &DbPool, run_id: &str) -> Result<Vec<AutoRunItemRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT i.item_id, i.case_id, i.case_title, COALESCE(c.kind, 'manual'), \
                COALESCE(c.language, 'python'), i.position, i.status, i.tester_config, \
                i.duration_secs, i.result_note, \
                (SELECT COUNT(*) FROM test_run_steps s WHERE s.item_id = i.item_id), \
                (SELECT COUNT(*) FROM test_run_steps s WHERE s.item_id = i.item_id \
                    AND s.status <> '') \
         FROM test_run_items i LEFT JOIN test_cases c ON c.case_id = i.case_id \
         WHERE i.run_id = ?1 ORDER BY i.position",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        let tester_config: String = row.get(7)?;
        let duration_secs: i64 = row.get(8)?;
        let duration_ms = serde_json::from_str::<serde_json::Value>(&tester_config)
            .ok()
            .and_then(|v| v.get("duration_ms").and_then(|d| d.as_u64()))
            .unwrap_or((duration_secs.max(0) as u64) * 1000);
        Ok(AutoRunItemRecord {
            item_id: row.get(0)?,
            case_id: row.get(1)?,
            case_title: row.get(2)?,
            kind: row.get(3)?,
            language: row.get(4)?,
            position: row.get::<_, i64>(5)? as u32,
            status: row.get(6)?,
            duration_ms,
            message: row.get(9)?,
            steps_total: row.get::<_, i64>(10)? as u32,
            steps_done: row.get::<_, i64>(11)? as u32,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

const ARTIFACT_COLS: &str =
    "artifact_id, run_id, item_id, name, kind, rel_path, sha256, size_bytes, mime";

fn read_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunArtifactRecord> {
    Ok(RunArtifactRecord {
        artifact_id: row.get(0)?,
        run_id: row.get(1)?,
        item_id: row.get(2)?,
        name: row.get(3)?,
        kind: row.get(4)?,
        rel_path: row.get(5)?,
        sha256: row.get(6)?,
        size_bytes: row.get::<_, i64>(7)? as u64,
        mime: row.get(8)?,
    })
}

pub fn list_artifacts(pool: &DbPool, run_id: &str) -> Result<Vec<RunArtifactRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ARTIFACT_COLS} FROM run_artifacts WHERE run_id = ?1 ORDER BY item_id, name"
    ))?;
    let rows = stmt.query_map(params![run_id], read_artifact)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_artifact(pool: &DbPool, artifact_id: &str) -> Result<Option<RunArtifactRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {ARTIFACT_COLS} FROM run_artifacts WHERE artifact_id = ?1"),
        params![artifact_id],
        read_artifact,
    )
    .optional()
    .map_err(Into::into)
}

/// Deletes the artifact rows of a run (the files are removed by the caller).
pub fn delete_run_artifacts(pool: &DbPool, run_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM run_artifacts WHERE run_id = ?1", params![run_id])?;
    tx.execute("DELETE FROM auto_run_meta WHERE run_id = ?1", params![run_id])?;
    tx.commit()?;
    Ok(())
}

// =============================================================================
// Run creation
// =============================================================================

/// One case selected into an automated run.
pub struct AutoCase {
    pub case_id: String,
    pub case_title: String,
    pub case_version: u32,
    pub kind: String,
    pub language: String,
    pub content_json: String,
}

/// Creates the run header, its items and the runner binding in ONE transaction.
/// Items start 'pending'; the runner flips them to 'running'.
#[allow(clippy::too_many_arguments)]
pub fn create_auto_run(
    pool: &DbPool,
    name: &str,
    suite_id: &str,
    run_type: &str,
    environment_id: &str,
    cases: &[AutoCase],
    runner_service_id: &str,
    runner_endpoint: &str,
    perf_profile_json: &str,
    created_by: &str,
) -> Result<(String, u32, Vec<String>)> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let run_no: i64 = tx.query_row(
        "SELECT COALESCE(MAX(run_no), 0) + 1 FROM test_runs",
        [],
        |row| row.get(0),
    )?;
    let run_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO test_runs (run_id, run_no, name, suite_id, run_type, environment_id, \
            assignment_mode, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pool', ?7)",
        params![
            run_id,
            run_no,
            name,
            suite_id,
            run_type,
            environment_id,
            created_by
        ],
    )?;
    let mut item_ids = Vec::with_capacity(cases.len());
    for (position, case) in cases.iter().enumerate() {
        let item_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, case_version, \
                position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                item_id,
                run_id,
                case.case_id,
                case.case_title,
                case.case_version as i64,
                position as i64
            ],
        )?;
        item_ids.push(item_id);
    }
    tx.execute(
        "INSERT INTO auto_run_meta (run_id, environment_id, runner_service_id, runner_endpoint, \
            perf_profile_json, watchdog_deadline_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            run_id,
            environment_id,
            runner_service_id,
            runner_endpoint,
            perf_profile_json,
            now_ms() + MAX_RUN_SECS * 1000
        ],
    )?;
    tx.commit()?;
    Ok((run_id, run_no as u32, item_ids))
}

/// Resolves the cases of an automated run from the XOR selector, preserving the
/// requested order. `require_all_approved` decides what happens to a case that
/// is no longer approved: a user-initiated run refuses loudly (the tester picked
/// it), a scheduled one drops it silently — a nightly job must not stop firing
/// because a single case went back to draft.
pub fn resolve_cases(
    pool: &DbPool,
    suite_id: &str,
    case_ids: &[String],
    from_run_id: &str,
    max_cases: usize,
    require_all_approved: bool,
) -> Result<Vec<AutoCase>> {
    let selectors = [
        !suite_id.is_empty(),
        !case_ids.is_empty(),
        !from_run_id.is_empty(),
    ];
    if selectors.iter().filter(|s| **s).count() != 1 {
        bail!("provide exactly one of suite_id / case_ids / from_run_id");
    }
    let ids: Vec<String> = if !suite_id.is_empty() {
        super::tests::suite_case_rows(pool, suite_id)?
            .into_iter()
            .map(|c| c.case_id)
            .collect()
    } else if !case_ids.is_empty() {
        case_ids.to_vec()
    } else {
        super::runs::failed_case_ids(pool, from_run_id)?
    };
    if ids.is_empty() {
        bail!("the selection has no cases");
    }
    if ids.len() > max_cases {
        bail!("a run accepts at most {max_cases} cases");
    }
    let mut out = Vec::with_capacity(ids.len());
    for case_id in &ids {
        let item = super::tests::get_case(pool, case_id)?;
        let Some(item) = item else {
            if require_all_approved {
                bail!("case '{case_id}' is not available");
            }
            continue;
        };
        if item.record.status != "approved" {
            if require_all_approved {
                bail!("case '{}' is not approved", item.record.title);
            }
            continue;
        }
        out.push(AutoCase {
            case_id: item.record.case_id,
            case_title: item.record.title,
            case_version: item.record.current_version,
            kind: item.record.kind,
            language: item.record.language,
            content_json: item.record.content_json,
        });
    }
    Ok(out)
}

/// A created run plus the items that may actually be submitted to the runner.
/// `submit_items` is empty when nothing in the selection is executable there —
/// the caller finishes the run instead of submitting it.
pub struct PreparedRun {
    pub run_id: String,
    pub run_no: u32,
    pub submit_items: Vec<SubmitItem>,
    /// Items marked non-executable up front (wrong kind/language for this
    /// runner, or a build profile that needs a sandbox Core cannot build yet).
    pub skipped: usize,
}

/// Creates the run and decides, per case, whether the chosen runner can execute
/// it: cases of a kind/language the runner does not advertise are marked
/// 'skipped', unit cases bound to a build profile 'blocked'. The run-level perf
/// profile overrides the one stored on the case, so the MERGED content is what
/// gets validated — validating the case alone would leave the override's
/// users/spawn_rate/duration unbounded.
#[allow(clippy::too_many_arguments)]
pub fn create_and_prepare_run(
    pool: &DbPool,
    name: &str,
    suite_id: &str,
    run_type: &str,
    environment_id: &str,
    cases: &[AutoCase],
    runner: &DiscoveredRunner,
    perf_profile_json: &str,
    created_by: &str,
) -> Result<PreparedRun> {
    let advertised: HashSet<String> = runner
        .health
        .as_ref()
        .map(|h| {
            h.toolchains
                .iter()
                .map(|t| t.language.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let (run_id, run_no, item_ids) = create_auto_run(
        pool,
        name,
        suite_id,
        run_type,
        environment_id,
        cases,
        &runner.service_id,
        &runner.endpoint_url,
        perf_profile_json,
        created_by,
    )?;

    let mut submit_items = Vec::with_capacity(cases.len());
    let mut skipped = Vec::new();
    let mut blocked = Vec::new();
    for (case, item_id) in cases.iter().zip(item_ids.iter()) {
        let executable = super::generation::is_code_kind(&case.kind)
            && advertised.contains(&case.language.to_ascii_lowercase());
        if !executable {
            skipped.push((
                item_id.clone(),
                format!(
                    "kind '{}' / language '{}' is not executable by this runner",
                    case.kind, case.language
                ),
            ));
            continue;
        }
        let content: serde_json::Value =
            serde_json::from_str(&case.content_json).unwrap_or_else(|_| serde_json::json!({}));
        // A unit case bound to a build profile needs the repository checked out
        // and the profile's install/test commands run inside a per-run sandbox.
        // The runner only understands an inline `build_profile` object with an
        // absolute, mounted workdir, which Core cannot produce yet — submitting
        // the reference anyway would silently degrade to plain pytest in an
        // empty directory and report a green run that executed nothing.
        if case.kind == "unit"
            && content
                .get("build_profile_ref")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.trim().is_empty())
        {
            blocked.push((
                item_id.clone(),
                "running a build profile requires a per-run sandbox (planned) — the case \
                 was not executed"
                    .to_string(),
            ));
            continue;
        }
        let mut content = content;
        if case.kind == "perf" && !perf_profile_json.trim().is_empty() {
            if let Ok(profile) = serde_json::from_str::<serde_json::Value>(perf_profile_json) {
                if let Some(map) = content.as_object_mut() {
                    map.insert("profile".to_string(), profile);
                }
            }
            if let Err(message) = super::generation::validate_case_content(
                &case.kind,
                &case.language,
                &content.to_string(),
            ) {
                let _ = finish_run(pool, &run_id, "error", &message);
                bail!("{message}");
            }
        }
        submit_items.push(SubmitItem {
            item_id: item_id.clone(),
            kind: case.kind.clone(),
            language: case.language.clone(),
            config: content
                .get("config")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            content,
        });
    }
    if !skipped.is_empty() || !blocked.is_empty() {
        if let Ok(conn) = pool.write() {
            for (item_id, reason) in &skipped {
                let _ = conn.execute(
                    "UPDATE test_run_items SET status = 'skipped', result_note = ?1, \
                        finished_at = datetime('now') WHERE item_id = ?2",
                    params![reason, item_id],
                );
            }
            for (item_id, reason) in &blocked {
                let _ = conn.execute(
                    "UPDATE test_run_items SET status = 'blocked', result_note = ?1, \
                        finished_at = datetime('now') WHERE item_id = ?2",
                    params![reason, item_id],
                );
            }
        }
    }
    Ok(PreparedRun {
        run_id,
        run_no,
        submit_items,
        skipped: skipped.len() + blocked.len(),
    })
}

fn set_runner_job_id(pool: &DbPool, run_id: &str, job_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE auto_run_meta SET runner_job_id = ?1 WHERE run_id = ?2",
        params![job_id, run_id],
    )?;
    Ok(())
}

/// Ends a run terminally: header status plus every non-terminal item. Guarded
/// on `status = 'running'`, so the watcher and a concurrent reconcile settle it
/// exactly once. This is also the ONE place a scheduled run reports back, so
/// the watchdog, cancel and reconcile paths all settle a schedule identically.
pub fn finish_run(pool: &DbPool, run_id: &str, status: &str, message: &str) -> Result<bool> {
    // The write lock is released before `settle` runs: the writer is a plain
    // (non-reentrant) mutex, so taking it twice on this thread would deadlock.
    let closed = {
        let conn = pool.write().map_err(write_err)?;
        let tx = conn.unchecked_transaction()?;
        let updated = tx.execute(
            "UPDATE test_runs SET status = ?1, finished_at = datetime('now') \
             WHERE run_id = ?2 AND status = 'running'",
            params![status, run_id],
        )?;
        if updated > 0 && status != "completed" {
            let item_status = if status == "cancelled" {
                "skipped"
            } else {
                "error"
            };
            tx.execute(
                "UPDATE test_run_items SET status = ?1, result_note = ?2, \
                    finished_at = datetime('now') \
                 WHERE run_id = ?3 AND status IN ('pending', 'running', 'in_progress')",
                params![item_status, message, run_id],
            )?;
        }
        tx.commit()?;
        updated > 0
    };
    if !closed {
        return Ok(false);
    }
    super::schedules::settle(pool, run_id, status);
    Ok(true)
}

// =============================================================================
// Snapshot application (ONE transaction per poll)
// =============================================================================

/// What a poll changed, for the live stream.
#[derive(Debug, Default)]
pub struct AppliedSnapshot {
    /// Items whose status/duration/message changed.
    pub changed_items: Vec<String>,
    /// Artifact rows created by this poll.
    pub new_artifacts: Vec<RunArtifactRecord>,
    pub perf_changed: bool,
}

/// Mirrors one runner snapshot into the project database: items, their steps,
/// the artifact rows and the perf aggregates — all in ONE transaction, so a
/// crash mid-poll never leaves an item marked passed without its steps.
/// `downloaded` carries the artifacts already fetched to disk by the caller.
fn apply_snapshot(
    pool: &DbPool,
    run_id: &str,
    snapshot: &RunnerSnapshot,
    downloaded: &[(String, SnapshotArtifact, String, u64)],
) -> Result<AppliedSnapshot> {
    let mut applied = AppliedSnapshot::default();
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;

    for item in &snapshot.items {
        let previous: Option<(String, String, String)> = tx
            .query_row(
                "SELECT status, result_note, tester_config FROM test_run_items \
                 WHERE item_id = ?1 AND run_id = ?2",
                params![item.item_id, run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((old_status, old_note, old_config)) = previous else {
            // The runner echoed an item id we never created — ignore it rather
            // than writing a row that belongs to no case.
            continue;
        };
        let runner_meta = serde_json::json!({ "duration_ms": item.duration_ms }).to_string();
        let terminal = matches!(
            item.status.as_str(),
            "passed" | "failed" | "blocked" | "skipped" | "error"
        );
        let changed =
            old_status != item.status || old_note != item.message || old_config != runner_meta;
        if changed {
            tx.execute(
                "UPDATE test_run_items SET status = ?1, result_note = ?2, tester_config = ?3, \
                    duration_secs = ?4, finished_at = CASE WHEN ?5 = 1 THEN datetime('now') \
                    ELSE finished_at END \
                 WHERE item_id = ?6",
                params![
                    item.status,
                    item.message,
                    runner_meta,
                    (item.duration_ms / 1000) as i64,
                    terminal as i64,
                    item.item_id
                ],
            )?;
            applied.changed_items.push(item.item_id.clone());
        }
        if !item.steps.is_empty() {
            // Steps are runner-owned: replace wholesale so a re-run of the same
            // item never leaves stale rows behind.
            tx.execute(
                "DELETE FROM test_run_steps WHERE item_id = ?1",
                params![item.item_id],
            )?;
            for step in &item.steps {
                tx.execute(
                    "INSERT INTO test_run_steps (item_id, step_index, action, expected, status, note) \
                     VALUES (?1, ?2, ?3, '', ?4, ?5)",
                    params![
                        item.item_id,
                        step.index as i64,
                        step.name,
                        step.status,
                        step.message
                    ],
                )?;
            }
        }
    }

    for (item_id, artifact, sha256, size) in downloaded {
        let artifact_id = uuid::Uuid::new_v4().to_string();
        let inserted = tx.execute(
            "INSERT INTO run_artifacts (artifact_id, run_id, item_id, name, kind, rel_path, \
                sha256, size_bytes, mime) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
             WHERE NOT EXISTS (SELECT 1 FROM run_artifacts \
                               WHERE run_id = ?2 AND rel_path = ?6)",
            params![
                artifact_id,
                run_id,
                item_id,
                artifact.name,
                normalize_artifact_kind(&artifact.kind),
                artifact.rel_path,
                sha256,
                *size as i64,
                artifact.mime
            ],
        )?;
        if inserted > 0 {
            applied.new_artifacts.push(RunArtifactRecord {
                artifact_id,
                run_id: run_id.to_string(),
                item_id: item_id.clone(),
                name: artifact.name.clone(),
                kind: normalize_artifact_kind(&artifact.kind).to_string(),
                rel_path: artifact.rel_path.clone(),
                sha256: sha256.clone(),
                size_bytes: *size,
                mime: artifact.mime.clone(),
            });
        }
    }

    let perf_summary = serde_json::to_string(&snapshot.perf.summary)?;
    let perf_timeline = serde_json::to_string(&snapshot.perf.timeline)?;
    let previous_perf: (String, String) = tx.query_row(
        "SELECT perf_summary_json, perf_timeline_json FROM auto_run_meta WHERE run_id = ?1",
        params![run_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    applied.perf_changed = previous_perf.0 != perf_summary || previous_perf.1 != perf_timeline;
    tx.execute(
        "UPDATE auto_run_meta SET perf_summary_json = ?1, perf_timeline_json = ?2, \
            last_poll_at = datetime('now'), failed_polls = 0 WHERE run_id = ?3",
        params![perf_summary, perf_timeline, run_id],
    )?;
    tx.commit()?;
    Ok(applied)
}

/// The DDL CHECK on `run_artifacts.kind` is a closed list; an unknown kind from
/// a newer runner must degrade to 'other', never abort the poll transaction.
fn normalize_artifact_kind(kind: &str) -> &str {
    match kind {
        "log" | "screenshot" | "trace" | "junit" | "perf_stats" | "har" => kind,
        _ => "other",
    }
}

fn bump_failed_polls(pool: &DbPool, run_id: &str) -> Result<u32> {
    let conn = pool.write().map_err(write_err)?;
    let count: i64 = conn.query_row(
        "UPDATE auto_run_meta SET failed_polls = failed_polls + 1, \
            last_poll_at = datetime('now') WHERE run_id = ?1 RETURNING failed_polls",
        params![run_id],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

// =============================================================================
// Watcher
// =============================================================================

/// Everything the watcher needs, captured before the spawn.
pub struct WatchTask {
    pub pool: DbPool,
    pub run_id: String,
    pub dir_path: PathBuf,
    pub endpoint_url: String,
    pub job_id: String,
    pub watchdog_deadline_ms: i64,
    /// Plaintext environment secret, kept only to redact it out of the
    /// artifacts the runner produces. Never logged, never persisted.
    pub secret: String,
}

/// Artifact budget of one run, carried across polls.
#[derive(Debug, Clone, Copy, Default)]
struct ArtifactBudget {
    count: usize,
    bytes: u64,
    exhausted: bool,
}

fn emit(tx: &tokio::sync::broadcast::Sender<BusMessage>, run_id: &str, kind: &str, phase: &str, line: String) {
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: run_id.to_string(),
        kind: kind.to_string(),
        line,
        phase: phase.to_string(),
        progress_pct: 0,
        ts_ms: log_bus::now_ms(),
    }));
}

/// Starts the poller for an already-submitted run. Opens the log_bus channel
/// BEFORE spawning (the response reaches the frontend first — an immediate
/// stream subscribe must find the channel) and finalizes the run row under a
/// panic guard.
pub fn start_watcher(task: WatchTask) {
    let run_id = task.run_id.clone();
    let cancel = register_cancel(&run_id);
    let tx = log_bus::sender_for(&run_id);

    tokio::spawn(async move {
        let pool = task.pool.clone();
        let run_id = task.run_id.clone();
        let endpoint = task.endpoint_url.clone();
        let job_id = task.job_id.clone();
        let watch = {
            let tx = tx.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { watch_loop(task, tx, cancel).await })
        };
        if let Err(join_err) = watch.await {
            if join_err.is_panic() {
                let _ = finish_run(&pool, &run_id, "error", "watcher task panicked");
            }
        }

        let (status, error) = match super::runs::get_run(&pool, &run_id) {
            Ok(Some((record, _))) => {
                let error = if record.status == "error" {
                    "run ended with an execution error".to_string()
                } else {
                    String::new()
                };
                (record.status, error)
            }
            _ => ("error".to_string(), "run record missing".to_string()),
        };
        let _ = tx.send(BusMessage::End {
            deploy_id: run_id.clone(),
            final_status: status,
            image_tag: String::new(),
            container_name: String::new(),
            error_message: error,
            duration_ms: 0,
        });
        // Let live subscribers drain End before the channel dies.
        tokio::time::sleep(Duration::from_millis(100)).await;
        log_bus::close(&run_id);
        unregister_cancel(&run_id);
        // Best-effort cleanup: a job the watcher stopped following (panic,
        // watchdog) would otherwise keep running on the runner.
        let _ = tokio::task::spawn_blocking(move || cancel_runner_job(&endpoint, &job_id)).await;
    });
}

async fn watch_loop(
    task: WatchTask,
    tx: tokio::sync::broadcast::Sender<BusMessage>,
    cancel: Arc<AtomicBool>,
) {
    let WatchTask {
        pool,
        run_id,
        dir_path,
        endpoint_url,
        job_id,
        watchdog_deadline_ms,
        secret,
    } = task;
    emit(&tx, &run_id, "phase", "queued", "run submitted to the runner".into());
    let mut budget = ArtifactBudget::default();

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if cancel.load(Ordering::Relaxed) {
            let endpoint = endpoint_url.clone();
            let job = job_id.clone();
            let _ = tokio::task::spawn_blocking(move || cancel_runner_job(&endpoint, &job)).await;
            let _ = finish_run(&pool, &run_id, "cancelled", "cancelled by the user");
            emit(&tx, &run_id, "phase", "cancelled", "run cancelled".into());
            return;
        }
        if now_ms() > watchdog_deadline_ms {
            let endpoint = endpoint_url.clone();
            let job = job_id.clone();
            let _ = tokio::task::spawn_blocking(move || cancel_runner_job(&endpoint, &job)).await;
            let _ = finish_run(
                &pool,
                &run_id,
                "error",
                "run exceeded the maximum execution time",
            );
            emit(
                &tx,
                &run_id,
                "log",
                "watchdog",
                "run exceeded the maximum execution time".into(),
            );
            return;
        }

        let endpoint = endpoint_url.clone();
        let job = job_id.clone();
        let dir = dir_path.clone();
        let run = run_id.clone();
        let pool_for_poll = pool.clone();
        let secret_for_poll = secret.clone();
        let budget_for_poll = budget;
        let polled = tokio::task::spawn_blocking(move || {
            poll_once(
                &pool_for_poll,
                &run,
                &dir,
                &endpoint,
                &job,
                &secret_for_poll,
                budget_for_poll,
            )
        })
        .await;

        let outcome = match polled {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => {
                let failed = bump_failed_polls(&pool, &run_id).unwrap_or(MAX_FAILED_POLLS);
                emit(
                    &tx,
                    &run_id,
                    "log",
                    "watchdog",
                    format!("poll {failed}/{MAX_FAILED_POLLS} failed: {e}"),
                );
                if failed >= MAX_FAILED_POLLS {
                    let endpoint = endpoint_url.clone();
                    let job = job_id.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || cancel_runner_job(&endpoint, &job)).await;
                    let _ = finish_run(
                        &pool,
                        &run_id,
                        "error",
                        "the test runner stopped answering",
                    );
                    return;
                }
                continue;
            }
            Err(_) => {
                let _ = finish_run(&pool, &run_id, "error", "poll task panicked");
                return;
            }
        };

        budget = outcome.budget;
        if let Some(notice) = outcome.budget_notice {
            emit(&tx, &run_id, "log", "artifacts", notice);
        }
        for item in &outcome.applied.changed_items {
            emit(&tx, &run_id, "item", "execute", item.clone());
        }
        for artifact in &outcome.applied.new_artifacts {
            emit(&tx, &run_id, "artifact", "execute", artifact.artifact_id.clone());
        }
        if outcome.applied.perf_changed {
            emit(&tx, &run_id, "perf", "execute", String::new());
        }

        match outcome.runner_status.as_str() {
            "completed" => {
                let _ = finish_run(&pool, &run_id, "completed", "");
                return;
            }
            "cancelled" => {
                let _ = finish_run(&pool, &run_id, "cancelled", "cancelled on the runner");
                return;
            }
            "error" => {
                let _ = finish_run(&pool, &run_id, "error", "the runner reported a job error");
                return;
            }
            _ => {}
        }
    }
}

struct PollOutcome {
    applied: AppliedSnapshot,
    runner_status: String,
    budget: ArtifactBudget,
    /// Set on the poll that exhausted the budget, so the live view says why the
    /// evidence stops.
    budget_notice: Option<String>,
}

/// One poll: fetch the snapshot, download the artifacts that are new, then
/// commit everything. Artifacts land on disk BEFORE their row exists, so a row
/// never points at a missing file. Blocking.
///
/// Artifact collection is budgeted (`MAX_ARTIFACTS_PER_RUN` /
/// `MAX_ARTIFACT_TOTAL_BYTES`). Exceeding it stops the DOWNLOADS only — the run
/// keeps polling to its real verdict, because the test results are complete
/// without the evidence files and killing the run would throw away work that
/// already passed.
fn poll_once(
    pool: &DbPool,
    run_id: &str,
    dir_path: &Path,
    endpoint_url: &str,
    job_id: &str,
    secret: &str,
    mut budget: ArtifactBudget,
) -> Result<PollOutcome> {
    let snapshot = poll_snapshot(endpoint_url, job_id)?;
    let known: HashSet<String> = list_artifacts(pool, run_id)?
        .into_iter()
        .map(|a| a.rel_path)
        .collect();
    let run_dir = run_artifact_dir(dir_path, run_id);
    let mut downloaded = Vec::new();
    let mut budget_notice = None;
    for item in &snapshot.items {
        for artifact in &item.artifacts {
            if known.contains(&artifact.rel_path) {
                continue;
            }
            if budget.exhausted {
                continue;
            }
            if budget.count >= MAX_ARTIFACTS_PER_RUN
                || budget.bytes >= MAX_ARTIFACT_TOTAL_BYTES
            {
                budget.exhausted = true;
                budget_notice = Some(format!(
                    "artifact budget reached ({} files / {} MiB) — the remaining \
                     artifacts of this run are not collected",
                    budget.count,
                    budget.bytes / (1024 * 1024)
                ));
                continue;
            }
            match download_artifact(
                endpoint_url,
                job_id,
                &run_dir,
                &artifact.rel_path,
                &artifact.mime,
                secret,
            ) {
                Ok((_, sha256, size)) => {
                    budget.count += 1;
                    budget.bytes += size;
                    downloaded.push((item.item_id.clone(), artifact.clone(), sha256, size))
                }
                Err(e) => tracing::warn!(
                    run_id,
                    rel_path = %artifact.rel_path,
                    "artifact download failed: {e}"
                ),
            }
        }
    }
    let applied = apply_snapshot(pool, run_id, &snapshot, &downloaded)?;
    Ok(PollOutcome {
        applied,
        runner_status: snapshot.status,
        budget,
        budget_notice,
    })
}

/// Submits the run to the runner and starts its watcher. Blocking submit runs
/// on a blocking worker; a submit failure ends the run row immediately (nothing
/// would ever poll it).
pub async fn submit_and_watch(
    pool: DbPool,
    run_id: String,
    dir_path: PathBuf,
    endpoint_url: String,
    items: Vec<SubmitItem>,
    environment: SubmitEnvironment,
    watchdog_deadline_ms: i64,
) -> Result<String> {
    let submit_endpoint = endpoint_url.clone();
    let submit_run_id = run_id.clone();
    // Kept for artifact redaction — the watcher needs the plaintext to scrub it
    // out of anything the runner echoed into a log or report.
    let secret = environment.secret.clone();
    let job_id = tokio::task::spawn_blocking(move || {
        submit_run(&submit_endpoint, &submit_run_id, &items, &environment)
    })
    .await
    .map_err(|_| anyhow!("runner submit task panicked"))?;
    let job_id = match job_id {
        Ok(job_id) => job_id,
        Err(e) => {
            let _ = finish_run(&pool, &run_id, "error", &format!("runner refused the run: {e}"));
            return Err(e);
        }
    };
    set_runner_job_id(&pool, &run_id, &job_id)?;
    start_watcher(WatchTask {
        pool,
        run_id,
        dir_path,
        endpoint_url,
        job_id: job_id.clone(),
        watchdog_deadline_ms,
        secret,
    });
    Ok(job_id)
}

// =============================================================================
// Reconciliation
// =============================================================================

/// Lazy recovery (mirrors `ingest::recover_orphaned_jobs` and
/// `generation::reconcile_running`): an automated run still marked 'running'
/// without a live watcher in THIS process lost its poller to a restart. The
/// runner keeps its jobs in memory only, so the job is gone too — the run is
/// closed as 'error' instead of hanging forever.
pub fn reconcile_running(pool: &DbPool) {
    let orphaned: Vec<String> = {
        let Ok(conn) = pool.read() else { return };
        let Ok(mut stmt) = conn.prepare(
            "SELECT run_id FROM test_runs WHERE status = 'running' \
             AND run_id IN (SELECT run_id FROM auto_run_meta)",
        ) else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
            return;
        };
        rows.filter_map(|r| r.ok())
            .filter(|run_id| !is_live(run_id))
            .collect()
    };
    for run_id in orphaned {
        tracing::warn!(run_id, "closing automated run orphaned by a restart");
        let _ = finish_run(pool, &run_id, "error", "interrupted by a restart");
    }
}

// =============================================================================
// Try-run registry (ephemeral, nothing persisted as a run)
// =============================================================================

struct TryRunState {
    cancel: Arc<AtomicBool>,
    started_ms: i64,
    user_id: String,
}

fn try_registry() -> &'static DashMap<String, TryRunState> {
    static REG: OnceLock<DashMap<String, TryRunState>> = OnceLock::new();
    REG.get_or_init(DashMap::new)
}

/// Drops try-runs older than the TTL. Called on every registration — the
/// registry is small and has no dedicated sweeper.
fn sweep_try_runs() {
    let cutoff = now_ms() - TRY_RUN_TTL.as_millis() as i64;
    let stale: Vec<String> = try_registry()
        .iter()
        .filter(|e| e.value().started_ms < cutoff)
        .map(|e| e.key().clone())
        .collect();
    for key in stale {
        if let Some((_, state)) = try_registry().remove(&key) {
            state.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// At most this many try runs execute at once. Each one occupies a runner slot
/// with untrusted code for up to `TRY_RUN_TTL`, so without a gate a handful of
/// open editors would starve the automated runs (mirrors the ingest gate).
const MAX_CONCURRENT_TRY_RUNS: usize = 2;

pub fn try_run_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_TRY_RUNS))
}

/// Registers a try-run. `None` = the id is already in flight (a duplicate
/// subscribe must not hijack the running execution).
pub fn register_try_run(try_id: &str, user_id: &str) -> Option<Arc<AtomicBool>> {
    sweep_try_runs();
    if try_registry().contains_key(try_id) {
        return None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    try_registry().insert(
        try_id.to_string(),
        TryRunState {
            cancel: cancel.clone(),
            started_ms: now_ms(),
            user_id: user_id.to_string(),
        },
    );
    Some(cancel)
}

pub fn unregister_try_run(try_id: &str) {
    try_registry().remove(try_id);
}

/// Cancels a try-run owned by `user_id`. Ownership is checked here: `try_id` is
/// client-minted, so without it any logged-in user could cancel someone else's
/// execution by guessing an id.
pub fn cancel_try_run(try_id: &str, user_id: &str) -> bool {
    match try_registry().get(try_id) {
        Some(state) if state.user_id == user_id => {
            state.cancel.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

/// Runs one ephemeral item to completion on the runner, polling until terminal.
/// Nothing is persisted: the caller streams the log lines and the final summary.
/// Everything that leaves this function is scrubbed of the environment secret —
/// a failing request prints its Authorization header into the step message.
/// Blocking — call from `spawn_blocking`.
pub fn run_try_item(
    endpoint_url: &str,
    try_id: &str,
    item: &SubmitItem,
    environment: &SubmitEnvironment,
    cancel: &Arc<AtomicBool>,
    deadline_ms: i64,
    mut on_event: impl FnMut(&str, &str),
) -> Result<serde_json::Value> {
    let job_id = submit_run(endpoint_url, try_id, std::slice::from_ref(item), environment)?;
    let scrub = |text: &str| -> String {
        if environment.secret.len() < 4 {
            return text.to_string();
        }
        text.replace(&environment.secret, "***")
    };
    let mut last_status = String::new();
    let mut failed_polls = 0u32;
    loop {
        if cancel.load(Ordering::Relaxed) {
            cancel_runner_job(endpoint_url, &job_id);
            bail!("cancelled");
        }
        if now_ms() > deadline_ms {
            cancel_runner_job(endpoint_url, &job_id);
            bail!("try run exceeded its time budget");
        }
        std::thread::sleep(POLL_INTERVAL);
        let snapshot = match poll_snapshot(endpoint_url, &job_id) {
            Ok(snapshot) => {
                failed_polls = 0;
                snapshot
            }
            Err(e) => {
                failed_polls += 1;
                on_event("log", &format!("poll failed: {e}"));
                if failed_polls >= MAX_FAILED_POLLS {
                    bail!("the test runner stopped answering");
                }
                continue;
            }
        };
        if let Some(state) = snapshot.items.first() {
            if state.status != last_status {
                last_status = state.status.clone();
                on_event("phase", &last_status);
            }
            for step in &state.steps {
                on_event(
                    "log",
                    scrub(format!("[{}] {} {}", step.status, step.name, step.message).trim())
                        .as_str(),
                );
            }
        }
        if matches!(snapshot.status.as_str(), "completed" | "cancelled" | "error") {
            let item_state = snapshot.items.first();
            return Ok(serde_json::json!({
                "job_status": snapshot.status,
                "status": item_state.map(|i| i.status.clone()).unwrap_or_default(),
                "duration_ms": item_state.map(|i| i.duration_ms).unwrap_or(0),
                "message": item_state.map(|i| scrub(&i.message)).unwrap_or_default(),
                "steps": item_state
                    .map(|i| {
                        i.steps
                            .iter()
                            .map(|s| serde_json::json!({
                                "index": s.index,
                                "name": s.name,
                                "status": s.status,
                                "message": scrub(&s.message),
                            }))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                "perf": {
                    "summary": snapshot.perf.summary,
                    "timeline": snapshot.perf.timeline,
                },
            }));
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn seed_case(pool: &DbPool, case_id: &str, kind: &str) {
        let conn = pool.write().expect("write");
        conn.execute(
            "INSERT INTO test_cases (case_id, kind, title, language, content_json, created_by, \
                status) VALUES (?1, ?2, 'Case', 'python', '{}', 'u1', 'approved')",
            params![case_id, kind],
        )
        .expect("insert case");
    }

    /// (d) A run whose watcher died with the process is closed on the next
    /// reconcile, with its unfinished items flipped to 'error'.
    #[test]
    fn reconcile_closes_runs_orphaned_by_a_restart() {
        let pool = pool();
        seed_case(&pool, "c1", "api");
        let (run_id, _no, item_ids) = create_auto_run(
            &pool,
            "Smoke",
            "",
            "auto",
            "env-1",
            &[AutoCase {
                case_id: "c1".to_string(),
                case_title: "Case".to_string(),
                case_version: 1,
                kind: "api".to_string(),
                language: "python".to_string(),
                content_json: "{}".to_string(),
            }],
            "7",
            "http://127.0.0.1:8093",
            "{}",
            "creator",
        )
        .expect("create run");
        assert_eq!(item_ids.len(), 1);

        // No cancel-registry entry = no live watcher in this process.
        reconcile_running(&pool);

        let (record, counts) = super::super::runs::get_run(&pool, &run_id)
            .expect("get run")
            .expect("run");
        assert_eq!(record.status, "error");
        assert_eq!(counts.pending, 0);
        let items = list_auto_items(&pool, &run_id).expect("items");
        assert_eq!(items[0].status, "error");
        assert_eq!(items[0].kind, "api");
        assert_eq!(items[0].language, "python");

        // A terminal run is never touched again.
        reconcile_running(&pool);
        let (record, _) = super::super::runs::get_run(&pool, &run_id)
            .expect("get run")
            .expect("run");
        assert_eq!(record.status, "error");
    }

    /// A poll snapshot lands atomically: item verdict + runner duration + steps
    /// + artifact rows, and a second identical poll reports no changes.
    #[test]
    fn apply_snapshot_mirrors_items_steps_and_artifacts() {
        let pool = pool();
        seed_case(&pool, "c1", "ui");
        let (run_id, _no, item_ids) = create_auto_run(
            &pool,
            "UI",
            "",
            "auto",
            "env-1",
            &[AutoCase {
                case_id: "c1".to_string(),
                case_title: "Case".to_string(),
                case_version: 1,
                kind: "ui".to_string(),
                language: "python".to_string(),
                content_json: "{}".to_string(),
            }],
            "7",
            "http://127.0.0.1:8093",
            "{}",
            "creator",
        )
        .expect("create run");
        let item_id = item_ids[0].clone();

        let snapshot = RunnerSnapshot {
            status: "running".to_string(),
            items: vec![SnapshotItem {
                item_id: item_id.clone(),
                status: "failed".to_string(),
                duration_ms: 1234,
                message: "assert failed".to_string(),
                steps: vec![
                    SnapshotStep {
                        index: 0,
                        name: "test_login".to_string(),
                        status: "passed".to_string(),
                        message: String::new(),
                    },
                    SnapshotStep {
                        index: 1,
                        name: "test_logout".to_string(),
                        status: "failed".to_string(),
                        message: "boom".to_string(),
                    },
                ],
                artifacts: vec![SnapshotArtifact {
                    name: "console.log".to_string(),
                    kind: "log".to_string(),
                    rel_path: format!("{item_id}/artifacts/console.log"),
                    size_bytes: 12,
                    mime: "text/plain".to_string(),
                }],
            }],
            perf: SnapshotPerf::default(),
        };
        let downloaded = vec![(
            item_id.clone(),
            snapshot.items[0].artifacts[0].clone(),
            "deadbeef".to_string(),
            12u64,
        )];
        let applied = apply_snapshot(&pool, &run_id, &snapshot, &downloaded).expect("apply");
        assert_eq!(applied.changed_items, vec![item_id.clone()]);
        assert_eq!(applied.new_artifacts.len(), 1);

        let items = list_auto_items(&pool, &run_id).expect("items");
        assert_eq!(items[0].status, "failed");
        assert_eq!(items[0].duration_ms, 1234, "sub-second duration survives");
        assert_eq!(items[0].message, "assert failed");
        assert_eq!(items[0].steps_total, 2);
        assert_eq!(items[0].steps_done, 2);

        // Idempotent: the same snapshot changes nothing and does not duplicate
        // the artifact row.
        let again = apply_snapshot(&pool, &run_id, &snapshot, &downloaded).expect("apply again");
        assert!(again.changed_items.is_empty());
        assert!(again.new_artifacts.is_empty());
        assert_eq!(list_artifacts(&pool, &run_id).expect("artifacts").len(), 1);
    }

    /// Minimal HTTP/1.1 stand-in for the runner: answers `/status` with the
    /// given snapshot and every other path with `body`. One request per
    /// connection, then close. Returns the base url.
    fn spawn_stub_runner(snapshot: String, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let payload = if request.contains("/status") {
                    snapshot.clone()
                } else {
                    body.to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    /// CR-007 + CR-004: a run may not pull down an unbounded number of
    /// artifacts, and whatever IS pulled down has the environment secret
    /// scrubbed out of it before it lands on disk (artifacts are readable by
    /// testers, the secret is not).
    #[test]
    fn artifact_budget_stops_downloads_and_the_secret_is_redacted() {
        let pool = pool();
        seed_case(&pool, "c1", "api");
        let (run_id, _no, item_ids) = create_auto_run(
            &pool,
            "Budget",
            "",
            "auto",
            "env-1",
            &[AutoCase {
                case_id: "c1".to_string(),
                case_title: "Case".to_string(),
                case_version: 1,
                kind: "api".to_string(),
                language: "python".to_string(),
                content_json: "{}".to_string(),
            }],
            "7",
            "http://127.0.0.1:1",
            "{}",
            "creator",
        )
        .expect("create run");
        let item_id = item_ids[0].clone();

        let artifacts: Vec<serde_json::Value> = (0..MAX_ARTIFACTS_PER_RUN + 5)
            .map(|i| {
                serde_json::json!({
                    "name": format!("console {i}.log"),
                    "kind": "log",
                    "rel_path": format!("{item_id}/artifacts/console {i}.log"),
                    "size_bytes": 32,
                    "mime": "text/plain",
                })
            })
            .collect();
        let snapshot = serde_json::json!({
            "status": "running",
            "items": [{
                "item_id": item_id,
                "status": "running",
                "duration_ms": 10,
                "message": "",
                "steps": [],
                "artifacts": artifacts,
            }],
            "perf": {"summary": [], "timeline": []},
        })
        .to_string();
        let endpoint = spawn_stub_runner(snapshot, "auth failed: Bearer SUPER-SECRET-TOKEN\n");

        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = poll_once(
            &pool,
            &run_id,
            dir.path(),
            &endpoint,
            "job-1",
            "SUPER-SECRET-TOKEN",
            ArtifactBudget::default(),
        )
        .expect("poll");

        assert_eq!(outcome.budget.count, MAX_ARTIFACTS_PER_RUN);
        assert!(outcome.budget.exhausted, "the budget must latch");
        assert!(
            outcome.budget_notice.is_some(),
            "exhausting the budget is reported to the live view"
        );
        assert_eq!(
            list_artifacts(&pool, &run_id).expect("artifacts").len(),
            MAX_ARTIFACTS_PER_RUN,
            "only the artifacts within the budget get rows"
        );

        // A second poll on the same (latched) budget downloads nothing more.
        let again = poll_once(
            &pool,
            &run_id,
            dir.path(),
            &endpoint,
            "job-1",
            "SUPER-SECRET-TOKEN",
            outcome.budget,
        )
        .expect("second poll");
        assert_eq!(again.budget.count, MAX_ARTIFACTS_PER_RUN);
        assert!(again.applied.new_artifacts.is_empty());

        let stored = std::fs::read_to_string(
            run_artifact_dir(dir.path(), &run_id)
                .join(&item_id)
                .join("artifacts")
                .join("console 0.log"),
        )
        .expect("artifact on disk");
        assert!(
            !stored.contains("SUPER-SECRET-TOKEN"),
            "the environment secret must be redacted out of artifacts"
        );
        assert!(stored.contains("***"));
    }

    #[test]
    fn artifact_paths_are_contained() {
        assert!(safe_rel_path("item/artifacts/console.log").is_ok());
        assert!(safe_rel_path("../../etc/passwd").is_err());
        assert!(safe_rel_path("/etc/passwd").is_err());
        assert!(safe_rel_path("").is_err());
        assert_eq!(normalize_artifact_kind("junit"), "junit");
        assert_eq!(normalize_artifact_kind("flamegraph"), "other");
    }

    #[test]
    fn try_run_registry_is_owner_scoped_and_single_use() {
        let try_id = uuid::Uuid::new_v4().to_string();
        let cancel = register_try_run(&try_id, "user-a").expect("register");
        assert!(register_try_run(&try_id, "user-a").is_none(), "no double use");
        assert!(!cancel_try_run(&try_id, "user-b"), "cancel is owner-scoped");
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(cancel_try_run(&try_id, "user-a"));
        assert!(cancel.load(Ordering::Relaxed));
        unregister_try_run(&try_id);
        assert!(!cancel_try_run(&try_id, "user-a"));
    }

    #[test]
    fn select_runner_requires_a_healthy_matching_toolchain() {
        let healthy = DiscoveredRunner {
            service_id: "1".to_string(),
            engine_id: RUNNER_ENGINE_ID.to_string(),
            display_name: "Runner".to_string(),
            endpoint_url: "http://127.0.0.1:8093".to_string(),
            status: "running".to_string(),
            health: Some(RunnerHealth {
                isolated: true,
                toolchains: vec![RunnerToolchainInfo {
                    language: "python".to_string(),
                    frameworks: vec!["pytest".to_string()],
                    version: "3.12".to_string(),
                }],
            }),
        };
        let dead = DiscoveredRunner {
            service_id: "2".to_string(),
            health: None,
            ..healthy.clone()
        };
        assert_eq!(
            select_runner(vec![dead.clone(), healthy.clone()], "", "python")
                .expect("match")
                .service_id,
            "1"
        );
        assert!(select_runner(vec![healthy.clone()], "", "node").is_err());
        assert!(select_runner(vec![dead.clone()], "2", "python").is_err());
        assert!(select_runner(vec![healthy], "99", "python").is_err());
    }
}
