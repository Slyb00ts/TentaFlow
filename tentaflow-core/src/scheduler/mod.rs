// =============================================================================
// Plik: scheduler/mod.rs
// Opis: Trwaly scheduler administracyjny. Przechowuje harmonogramy w SQLite,
//       uruchamia akcje addonow i zapisuje historie wykonan dla dashboardu.
// =============================================================================

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::addon::AddonManager;
use crate::db::{repository, DbPool};

const DEFAULT_POLL_SECONDS: u64 = 30;
const DEFAULT_MAX_RUNTIME_SECONDS: i64 = 1800;
static SCHEDULER_STARTED: OnceLock<()> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub target_type: String,
    pub target_addon_id: String,
    pub target_action_id: String,
    pub payload_json: String,
    pub schedule_kind: String,
    pub schedule_expr: String,
    pub timezone: String,
    pub next_run_at: Option<String>,
    pub max_runtime_seconds: i64,
    pub retry_policy_json: String,
    pub concurrency_policy: String,
    pub created_by_user_id: Option<String>,
    // org_id instancji addona, ktorej dotyczy job. None => default org (joby sprzed R4).
    // Niesie tozsamosc najemcy do start_addon, by host-fns dzialaly na danych wlasciwej org.
    pub org_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRun {
    pub id: String,
    pub job_id: String,
    pub status: String,
    pub scheduled_for: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub result_json: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerAction {
    pub addon_id: String,
    pub action_id: String,
    pub display_name: String,
    pub description: String,
    pub parameters_schema: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertJobRequest {
    pub id: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub target_addon_id: String,
    pub target_action_id: String,
    pub payload_json: String,
    pub schedule_kind: String,
    pub schedule_expr: String,
    pub timezone: String,
    pub max_runtime_seconds: Option<i64>,
    pub retry_policy_json: Option<String>,
    pub concurrency_policy: Option<String>,
    // org_id instancji addona dla tego joba. None => default org (zgodnie z dotychczasowym
    // zachowaniem). Wymagane dla jobow multi-tenant (np. conflict_scan addonu RAG).
    // serde(default): dashboard wysyla job_json bez tego pola (admin UI nie ustawia org),
    // wiec brak pola = None, a nie blad deserializacji.
    #[serde(default)]
    pub org_id: Option<String>,
}

pub fn start(db: DbPool, addon_manager: Option<Arc<AddonManager>>) {
    let Some(addon_manager) = addon_manager else {
        warn!("scheduler: AddonManager unavailable, scheduled addon actions disabled");
        return;
    };
    if SCHEDULER_STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        loop {
            if let Err(e) = run_due_once(&db, &addon_manager).await {
                warn!("scheduler poll failed: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(DEFAULT_POLL_SECONDS)).await;
        }
    });
}

pub fn list_jobs(db: &DbPool) -> Result<Vec<ScheduledJob>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, name, enabled, target_type, target_addon_id, target_action_id, \
                payload_json, schedule_kind, schedule_expr, timezone, next_run_at, \
                max_runtime_seconds, retry_policy_json, concurrency_policy, created_by_user_id, \
                org_id, created_at, updated_at \
         FROM scheduled_jobs ORDER BY enabled DESC, next_run_at IS NULL, next_run_at, name",
    )?;
    let rows = stmt.query_map([], read_job)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_runs(db: &DbPool, job_id: &str, limit: i64) -> Result<Vec<ScheduledRun>> {
    let limit = limit.clamp(1, 200);
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, job_id, status, scheduled_for, started_at, finished_at, result_json, error \
         FROM scheduled_runs WHERE job_id = ?1 ORDER BY scheduled_for DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![job_id, limit], read_run)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_addon_actions(db: &DbPool) -> Result<Vec<SchedulerAction>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT addon_id, manifest_json FROM addons WHERE is_enabled = 1 ORDER BY name, addon_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (addon_id, manifest_raw) = row?;
        let manifest = match crate::addon::lifecycle::parse_manifest_toml(&manifest_raw) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    "scheduler: addon '{}' manifest parse failed: {}",
                    addon_id, e
                );
                continue;
            }
        };
        for tool in manifest.tools {
            let parameters_schema =
                serde_json::to_string(&tool.parameters_schema).unwrap_or_else(|_| "{}".to_string());
            let action_id = tool.name;
            out.push(SchedulerAction {
                addon_id: addon_id.clone(),
                display_name: action_id.clone(),
                action_id,
                description: tool.description,
                parameters_schema,
            });
        }
    }
    Ok(out)
}

/// Auto-rejestruje interwalowy job `ingest_drain` dla KAZDEGO wlaczonego addonu, ktory
/// deklaruje tool `ingest_drain` (kontrakt async-ingestu: upload enqueue'uje, a drain
/// mieli w tle). Bez tego kolejka ingestu stoi az admin recznie zalozy job, a po
/// resecie TentaFlow nic nie wznawia przetwarzania. Idempotentne: staly `id` per addon
/// => upsert nadpisuje ten sam wiersz (bez duplikatow), a wywolanie przy starcie odtwarza
/// harmonogram po kazdym restarcie. Interwal 30s + `concurrency=skip`: jedno firing mieli
/// caly backlog (batch-drain), a nastepne sa pomijane dopoki poprzednie trwa.
pub fn ensure_addon_ingest_drain_schedules(db: &DbPool) -> Result<usize> {
    let addons: Vec<(String, String)> = {
        let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT addon_id, manifest_json FROM addons WHERE is_enabled = 1 ORDER BY addon_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut ensured = 0usize;
    for (addon_id, manifest_raw) in addons {
        let manifest = match crate::addon::lifecycle::parse_manifest_toml(&manifest_raw) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !manifest.tools.iter().any(|t| t.name == "ingest_drain") {
            continue;
        }
        let req = UpsertJobRequest {
            id: Some(format!("{addon_id}-ingest-drain-auto")),
            name: format!("{addon_id}: ingest drain (auto)"),
            enabled: true,
            target_addon_id: addon_id.clone(),
            target_action_id: "ingest_drain".to_string(),
            payload_json: "{}".to_string(),
            schedule_kind: "interval".to_string(),
            schedule_expr: "30s".to_string(),
            timezone: "UTC".to_string(),
            // 1800s: duze PDF-y (setki stron -> tysiace chunkow do embeddingu) nie
            // moga byc przycinane w polowie. MUSI byc < reclaim STALE_RUNNING_SECS
            // w addonie, zeby osierocony po przycieciu job zostal odzyskany.
            max_runtime_seconds: Some(1800),
            retry_policy_json: None,
            concurrency_policy: Some("skip".to_string()),
            org_id: None,
        };
        match upsert_job(db, req, "system") {
            Ok(_) => ensured += 1,
            Err(e) => warn!("scheduler: auto ingest-drain job dla '{addon_id}' nieudany: {e}"),
        }
    }
    Ok(ensured)
}

pub fn upsert_job(db: &DbPool, req: UpsertJobRequest, user_id: &str) -> Result<ScheduledJob> {
    validate_job_request(&req)?;
    ensure_target_action_exists(db, &req.target_addon_id, &req.target_action_id)?;
    let id = req.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let next_run_at = if req.enabled {
        Some(compute_next_run(
            &req.schedule_kind,
            &req.schedule_expr,
            Utc::now(),
        )?)
    } else {
        None
    };
    let max_runtime = req
        .max_runtime_seconds
        .unwrap_or(DEFAULT_MAX_RUNTIME_SECONDS)
        .clamp(1, 86_400);
    let retry_policy = req
        .retry_policy_json
        .unwrap_or_else(|| json!({"max_attempts":1,"backoff_seconds":60}).to_string());
    let concurrency_policy = req.concurrency_policy.unwrap_or_else(|| "skip".to_string());

    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute(
        "INSERT INTO scheduled_jobs \
             (id, name, enabled, target_type, target_addon_id, target_action_id, payload_json, \
              schedule_kind, schedule_expr, timezone, next_run_at, max_runtime_seconds, \
              retry_policy_json, concurrency_policy, created_by_user_id, org_id, updated_at) \
         VALUES (?1, ?2, ?3, 'addon_action', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, datetime('now')) \
         ON CONFLICT(id) DO UPDATE SET \
              name = excluded.name, enabled = excluded.enabled, target_type = excluded.target_type, \
              target_addon_id = excluded.target_addon_id, target_action_id = excluded.target_action_id, \
              payload_json = excluded.payload_json, schedule_kind = excluded.schedule_kind, \
              schedule_expr = excluded.schedule_expr, timezone = excluded.timezone, \
              next_run_at = excluded.next_run_at, max_runtime_seconds = excluded.max_runtime_seconds, \
              retry_policy_json = excluded.retry_policy_json, concurrency_policy = excluded.concurrency_policy, \
              org_id = excluded.org_id, updated_at = datetime('now')",
        params![
            id,
            req.name,
            req.enabled as i64,
            req.target_addon_id,
            req.target_action_id,
            req.payload_json,
            req.schedule_kind,
            req.schedule_expr,
            req.timezone,
            next_run_at,
            max_runtime,
            retry_policy,
            concurrency_policy,
            user_id,
            req.org_id,
        ],
    )?;
    drop(conn);
    get_job(db, &id)?.ok_or_else(|| anyhow::anyhow!("scheduled job not found after upsert"))
}

pub fn delete_job(db: &DbPool, job_id: &str) -> Result<()> {
    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute("DELETE FROM scheduled_jobs WHERE id = ?1", params![job_id])?;
    Ok(())
}

pub async fn run_now(
    db: &DbPool,
    addon_manager: Arc<AddonManager>,
    job_id: &str,
    user_id: &str,
) -> Result<ScheduledRun> {
    let job = get_job(db, job_id)?.ok_or_else(|| anyhow::anyhow!("scheduled job not found"))?;
    execute_job(db, addon_manager, job, Utc::now(), user_id).await
}

async fn run_due_once(db: &DbPool, addon_manager: &Arc<AddonManager>) -> Result<()> {
    let due = due_jobs(db)?;
    for job in due {
        let db = db.clone();
        let addon_manager = addon_manager.clone();
        tokio::spawn(async move {
            if let Err(e) = execute_job(&db, addon_manager, job, Utc::now(), "").await {
                warn!("scheduler job execution failed: {}", e);
            }
        });
    }
    Ok(())
}

async fn execute_job(
    db: &DbPool,
    addon_manager: Arc<AddonManager>,
    job: ScheduledJob,
    scheduled_for: DateTime<Utc>,
    user_id: &str,
) -> Result<ScheduledRun> {
    if job.concurrency_policy == "skip" && has_running_run(db, &job.id)? {
        let run_id = insert_run(db, &job.id, "skipped", scheduled_for)?;
        finish_run(
            db,
            &run_id,
            "skipped",
            None,
            Some("previous run still active"),
        )?;
        bump_next_run(db, &job)?;
        return get_run(db, &run_id)?.ok_or_else(|| anyhow::anyhow!("scheduled run missing"));
    }

    let run_id = insert_run(db, &job.id, "running", scheduled_for)?;
    mark_run_started(db, &run_id)?;
    let params: Value = serde_json::from_str(&job.payload_json)
        .map_err(|e| anyhow::anyhow!("invalid payload_json: {e}"))?;
    let addon_id = job.target_addon_id.clone();
    let action_id = job.target_action_id.clone();
    // org_id instancji niesiony przez job (R4). None => default org (joby sprzed R4
    // zachowuja zachowanie). Przekazany do start_addon, by host-fns addonu (graf/SQL)
    // dzialaly na danych wlasciwego najemcy, a nie domyslnej organizacji.
    let job_org_id = job.org_id.clone();
    let timeout_seconds = job.max_runtime_seconds.max(1) as u64;
    let actor_user_id = if !user_id.is_empty() {
        user_id.to_string()
    } else {
        job.created_by_user_id.clone().unwrap_or_default()
    };

    let task = {
        let actor_user_id = actor_user_id.clone();
        tokio::task::spawn_blocking(move || {
            if actor_user_id.is_empty() {
                bail!("scheduler run requires an actor user id");
            }
            if !addon_manager.has_running_instance(&addon_id) {
                addon_manager
                    .start_addon(&addon_id, Some(actor_user_id.clone()), job_org_id.clone())
                    .map_err(|e| {
                        anyhow::anyhow!("nie udalo sie uruchomic addonu '{}': {e}", addon_id)
                    })?;
            } else if let Some(job_org) = job_org_id.as_deref() {
                // Izolacja multi-tenant (blocker 3). `addon_id` jest GLOBALNIE UNIKALNY per
                // zainstalowana instancja (lifecycle::install_instance -> unique_instance_id
                // dokleja uuid i przepisuje [addon].id), wiec pula `instances` keyowana po
                // addon_id NIGDY nie miesza organizacji — wszystkie workery danego addon_id
                // dziedzicza org instancji glownej (acquire_instance: instance_org_id z first).
                // Gdy instancja JUZ dziala (start_addon pominiety), jej org jest USTALONA przy
                // starcie i moze sie roznic od job_org_id, jesli wystartowal ja boot/system
                // (org=None) albo inny najemca. call_tool nie przyjmuje org_id, wiec wykonalby
                // sie na danych org dzialajacej instancji. Zamiast cichej rozbieznosci ASERTUJEMY
                // zgodnosc i przerywamy run bledem (audyt scheduled_runs.failed) — to wymusza
                // start instancji we wlasciwej org, a nie wykonanie na cudzych danych.
                match addon_manager.instance_org_id(&addon_id) {
                    Some(inst_org) if inst_org == job_org => {}
                    other => {
                        bail!(
                            "izolacja multi-tenant: addon '{}' dziala w org {:?}, a job zada org '{}' \
                             — przerywam (zatrzymaj instancje albo uruchom ja w org joba)",
                            addon_id,
                            other,
                            job_org
                        );
                    }
                }
            }
            // Joby systemowe (auto-harmonogram core, np. ingest_drain) nie maja realnego
            // principala — wolamy jako System (omija per-user ACL, jak inne core-internal
            // wywolania toolow). Joby uzytkownika (created_by=admin) ida normalna sciezka ACL.
            if actor_user_id == "system" {
                addon_manager.call_tool_system(&addon_id, &action_id, params)
            } else {
                addon_manager.call_tool(&addon_id, &action_id, params, &actor_user_id)
            }
        })
    };
    let result = tokio::time::timeout(Duration::from_secs(timeout_seconds), task).await;
    match result {
        Ok(Ok(Ok(value))) => {
            let result_json = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
            finish_run(db, &run_id, "success", Some(&result_json), None)?;
            bump_next_run(db, &job)?;
        }
        Ok(Ok(Err(e))) => {
            let err = e.to_string();
            finish_run(db, &run_id, "failed", None, Some(&err))?;
            bump_next_run(db, &job)?;
        }
        Ok(Err(e)) => {
            let err = e.to_string();
            finish_run(db, &run_id, "failed", None, Some(&err))?;
            bump_next_run(db, &job)?;
        }
        Err(_) => {
            finish_run(db, &run_id, "timeout", None, Some("runtime timeout"))?;
            bump_next_run(db, &job)?;
        }
    }

    let _ = repository::log_audit(
        db,
        if actor_user_id.is_empty() {
            None
        } else {
            Some(actor_user_id.as_str())
        },
        Some(&job.target_addon_id),
        "scheduler.run",
        Some(&job.id),
        Some(&format!(
            "job='{}' action='{}'",
            job.name, job.target_action_id
        )),
        None,
        None,
    );

    get_run(db, &run_id)?.ok_or_else(|| anyhow::anyhow!("scheduled run missing"))
}

fn validate_job_request(req: &UpsertJobRequest) -> Result<()> {
    if req.name.trim().is_empty() {
        bail!("name is required");
    }
    if req.name.chars().count() > 128 {
        bail!("name must be at most 128 characters");
    }
    if req.target_addon_id.trim().is_empty() || req.target_action_id.trim().is_empty() {
        bail!("target addon and action are required");
    }
    if !is_safe_identifier(&req.target_addon_id) || !is_safe_identifier(&req.target_action_id) {
        bail!("target addon and action must use letters, digits, dot, dash or underscore");
    }
    if !matches!(req.schedule_kind.as_str(), "once" | "interval" | "cron") {
        bail!("schedule_kind must be once, interval or cron");
    }
    let payload = serde_json::from_str::<Value>(&req.payload_json)
        .map_err(|e| anyhow::anyhow!("payload_json is invalid: {e}"))?;
    if !payload.is_object() {
        bail!("payload_json must be a JSON object");
    }
    if let Some(policy) = &req.concurrency_policy {
        if policy != "skip" {
            bail!("concurrency_policy must be 'skip'");
        }
    }
    let _ = compute_next_run(&req.schedule_kind, &req.schedule_expr, Utc::now())?;
    Ok(())
}

fn ensure_target_action_exists(db: &DbPool, addon_id: &str, action_id: &str) -> Result<()> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let manifest_raw: Option<String> = conn
        .query_row(
            "SELECT manifest_json FROM addons WHERE addon_id = ?1 AND is_enabled = 1",
            params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(manifest_raw) = manifest_raw else {
        bail!("target addon '{}' is not installed or disabled", addon_id);
    };
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&manifest_raw)
        .map_err(|e| anyhow::anyhow!("target addon manifest parse failed: {e}"))?;
    if manifest.tools.iter().any(|tool| tool.name == action_id) {
        Ok(())
    } else {
        bail!("target action '{}.{}' does not exist", addon_id, action_id);
    }
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

fn compute_next_run(kind: &str, expr: &str, now: DateTime<Utc>) -> Result<String> {
    let next = match kind {
        "once" => DateTime::parse_from_rfc3339(expr)
            .map_err(|e| anyhow::anyhow!("once schedule must be RFC3339: {e}"))?
            .with_timezone(&Utc),
        "interval" => now + chrono::Duration::seconds(parse_interval_seconds(expr)?),
        "cron" => next_daily_cron(expr, now)?,
        _ => bail!("unsupported schedule kind"),
    };
    Ok(next.to_rfc3339())
}

fn parse_interval_seconds(expr: &str) -> Result<i64> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        bail!("interval is required");
    }
    let (num, mult) = match trimmed.chars().last().unwrap_or('s') {
        's' => (&trimmed[..trimmed.len() - 1], 1),
        'm' => (&trimmed[..trimmed.len() - 1], 60),
        'h' => (&trimmed[..trimmed.len() - 1], 3600),
        'd' => (&trimmed[..trimmed.len() - 1], 86_400),
        c if c.is_ascii_digit() => (trimmed, 1),
        _ => bail!("interval must use s, m, h or d suffix"),
    };
    let value = num
        .parse::<i64>()
        .map_err(|e| anyhow::anyhow!("invalid interval: {e}"))?;
    Ok((value * mult).clamp(1, 31_536_000))
}

fn next_daily_cron(expr: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 || parts[2] != "*" || parts[3] != "*" || parts[4] != "*" {
        bail!("cron MVP supports only 'minute hour * * *'");
    }
    let minute = parts[0]
        .parse::<u32>()
        .map_err(|e| anyhow::anyhow!("invalid cron minute: {e}"))?;
    let hour = parts[1]
        .parse::<u32>()
        .map_err(|e| anyhow::anyhow!("invalid cron hour: {e}"))?;
    let time = NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid cron time"))?;
    let today: NaiveDate = now.date_naive();
    let mut next = today.and_time(time).and_utc();
    if next <= now {
        next += chrono::Duration::days(1);
    }
    Ok(next)
}

fn due_jobs(db: &DbPool) -> Result<Vec<ScheduledJob>> {
    let now = Utc::now().to_rfc3339();
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, name, enabled, target_type, target_addon_id, target_action_id, \
                payload_json, schedule_kind, schedule_expr, timezone, next_run_at, \
                max_runtime_seconds, retry_policy_json, concurrency_policy, created_by_user_id, \
                org_id, created_at, updated_at \
         FROM scheduled_jobs \
         WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
         ORDER BY next_run_at LIMIT 20",
    )?;
    let rows = stmt.query_map(params![now], read_job)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn get_job(db: &DbPool, job_id: &str) -> Result<Option<ScheduledJob>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT id, name, enabled, target_type, target_addon_id, target_action_id, \
                payload_json, schedule_kind, schedule_expr, timezone, next_run_at, \
                max_runtime_seconds, retry_policy_json, concurrency_policy, created_by_user_id, \
                org_id, created_at, updated_at \
         FROM scheduled_jobs WHERE id = ?1",
        params![job_id],
        read_job,
    )
    .optional()
    .map_err(Into::into)
}

fn get_run(db: &DbPool, run_id: &str) -> Result<Option<ScheduledRun>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT id, job_id, status, scheduled_for, started_at, finished_at, result_json, error \
         FROM scheduled_runs WHERE id = ?1",
        params![run_id],
        read_run,
    )
    .optional()
    .map_err(Into::into)
}

fn has_running_run(db: &DbPool, job_id: &str) -> Result<bool> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM scheduled_runs WHERE job_id = ?1 AND status = 'running')",
        params![job_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(Into::into)
}

fn insert_run(
    db: &DbPool,
    job_id: &str,
    status: &str,
    scheduled_for: DateTime<Utc>,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute(
        "INSERT INTO scheduled_runs (id, job_id, status, scheduled_for) VALUES (?1, ?2, ?3, ?4)",
        params![id, job_id, status, scheduled_for.to_rfc3339()],
    )?;
    Ok(id)
}

fn mark_run_started(db: &DbPool, run_id: &str) -> Result<()> {
    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute(
        "UPDATE scheduled_runs SET status = 'running', started_at = ?1 WHERE id = ?2",
        params![Utc::now().to_rfc3339(), run_id],
    )?;
    Ok(())
}

fn finish_run(
    db: &DbPool,
    run_id: &str,
    status: &str,
    result_json: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute(
        "UPDATE scheduled_runs \
         SET status = ?1, finished_at = ?2, result_json = ?3, error = ?4 \
         WHERE id = ?5",
        params![status, Utc::now().to_rfc3339(), result_json, error, run_id],
    )?;
    Ok(())
}

fn bump_next_run(db: &DbPool, job: &ScheduledJob) -> Result<()> {
    let next = if job.schedule_kind == "once" {
        None
    } else {
        Some(compute_next_run(
            &job.schedule_kind,
            &job.schedule_expr,
            Utc::now(),
        )?)
    };
    let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    conn.execute(
        "UPDATE scheduled_jobs SET next_run_at = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![next, job.id],
    )?;
    Ok(())
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledJob> {
    Ok(ScheduledJob {
        id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        target_type: row.get(3)?,
        target_addon_id: row.get(4)?,
        target_action_id: row.get(5)?,
        payload_json: row.get(6)?,
        schedule_kind: row.get(7)?,
        schedule_expr: row.get(8)?,
        timezone: row.get(9)?,
        next_run_at: row.get(10)?,
        max_runtime_seconds: row.get(11)?,
        retry_policy_json: row.get(12)?,
        concurrency_policy: row.get(13)?,
        created_by_user_id: row.get(14)?,
        org_id: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledRun> {
    Ok(ScheduledRun {
        id: row.get(0)?,
        job_id: row.get(1)?,
        status: row.get(2)?,
        scheduled_for: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        result_json: row.get(6)?,
        error: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fresh_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("fresh db")
    }

    fn install_eureka_metadata(db: &DbPool) {
        let conn = db.write().expect("db lock");
        conn.execute(
            "INSERT INTO addons (addon_id, name, version, manifest_json, platforms, is_enabled) \
             VALUES ('eureka', 'Eureka MF', '1.0.0', ?1, 'linux,macos,windows', 1)",
            params![include_str!("../../addons/eureka/manifest.toml")],
        )
        .expect("insert addon metadata");
    }

    fn eureka_job(action_id: &str) -> UpsertJobRequest {
        UpsertJobRequest {
            id: None,
            name: "Eureka daily".to_string(),
            enabled: true,
            target_addon_id: "eureka".to_string(),
            target_action_id: action_id.to_string(),
            payload_json: json!({"batch_size": 10}).to_string(),
            schedule_kind: "cron".to_string(),
            schedule_expr: "15 3 * * *".to_string(),
            timezone: "UTC".to_string(),
            max_runtime_seconds: Some(300),
            retry_policy_json: None,
            concurrency_policy: Some("skip".to_string()),
            org_id: None,
        }
    }

    #[test]
    fn scheduler_upsert_requires_existing_addon_tool() {
        let db = fresh_db();
        install_eureka_metadata(&db);

        assert!(upsert_job(&db, eureka_job("sync_new"), "1").is_ok());
        assert!(upsert_job(&db, eureka_job("missing_tool"), "1").is_err());
    }

    #[test]
    fn scheduler_rejects_non_object_payload_and_unsafe_ids() {
        let mut req = eureka_job("sync_new");
        req.payload_json = "[]".to_string();
        assert!(validate_job_request(&req).is_err());

        let mut req = eureka_job("sync_new");
        req.target_addon_id = "eureka;drop".to_string();
        assert!(validate_job_request(&req).is_err());
    }

    // org_id (R4): job niesie org_id instancji i jest on odczytywany przez read_job, a
    // execute_job przekazuje go do start_addon. Test buduje schemat scheduled_jobs +
    // migracje v89 (ALTER ADD org_id) WPROST na :memory: (bez pelnego init-chaina, ktory
    // w tym srodowisku ma niezalezny problem z tabela addons), i sprawdza ze:
    //   - job z org_id zapisany i odczytany zachowuje org_id (propagacja tozsamosci),
    //   - job bez org_id (sprzed R4 / dashboard) ma org_id=None (default org zachowany).
    #[test]
    fn scheduled_job_round_trips_org_id() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(crate::db::migrations::scheduled_jobs_schema_for_test())
            .expect("base scheduled_jobs schema");
        conn.execute_batch("ALTER TABLE scheduled_jobs ADD COLUMN org_id TEXT;")
            .expect("v89 org_id column");

        let insert = "INSERT INTO scheduled_jobs \
             (id, name, enabled, target_type, target_addon_id, target_action_id, payload_json, \
              schedule_kind, schedule_expr, timezone, next_run_at, max_runtime_seconds, \
              retry_policy_json, concurrency_policy, created_by_user_id, org_id, updated_at) \
             VALUES (?1, 'n', 1, 'addon_action', 'rag', 'conflict_scan', '{}', 'interval', '5m', \
              'UTC', NULL, 300, '{}', 'skip', 'u1', ?2, datetime('now'))";
        conn.execute(insert, params!["job-with-org", "org-tenant-7"])
            .expect("insert job with org");
        conn.execute(insert, params!["job-no-org", Option::<String>::None])
            .expect("insert legacy job without org");

        let select = "SELECT id, name, enabled, target_type, target_addon_id, target_action_id, \
                payload_json, schedule_kind, schedule_expr, timezone, next_run_at, \
                max_runtime_seconds, retry_policy_json, concurrency_policy, created_by_user_id, \
                org_id, created_at, updated_at FROM scheduled_jobs WHERE id = ?1";

        let with_org = conn
            .query_row(select, params!["job-with-org"], read_job)
            .expect("read job with org");
        assert_eq!(with_org.org_id.as_deref(), Some("org-tenant-7"));

        let no_org = conn
            .query_row(select, params!["job-no-org"], read_job)
            .expect("read legacy job");
        assert_eq!(no_org.org_id, None, "job bez org_id => None (default org)");
    }

    #[test]
    fn scheduler_computes_interval_and_daily_cron() {
        let now = DateTime::parse_from_rfc3339("2026-05-19T03:10:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            compute_next_run("interval", "30m", now).unwrap(),
            "2026-05-19T03:40:00+00:00"
        );
        assert_eq!(
            compute_next_run("cron", "15 3 * * *", now).unwrap(),
            "2026-05-19T03:15:00+00:00"
        );
        assert_eq!(
            compute_next_run("cron", "5 3 * * *", now).unwrap(),
            "2026-05-20T03:05:00+00:00"
        );
    }
}
