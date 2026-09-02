// =============================================================================
// File: tentanas/jobs.rs — long-running work of one node (tab "Zadania").
//       A job is a row in tentanas.db plus a tokio task; the row is the
//       contract (status, progress, log) and survives the task, the task
//       may be cancelled through the registry below. Every job that needs
//       root receives a one-shot token or goes through the node's channel —
//       the password never outlives the task that used it.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::NasJob;
use tentanas_helper::{HelperCommand, PackageManager, SelfTestKind};
use tokio_util::sync::CancellationToken;

use super::db as store;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

fn running() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static REG: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle a job body uses to report. Log lines are persisted immediately so
/// a dashboard polling `JobGetRequest` sees them as they happen.
#[derive(Clone)]
pub struct JobHandle {
    db: DbPool,
    pub job_id: String,
    cancel: CancellationToken,
}

impl JobHandle {
    pub fn log(&self, line: impl AsRef<str>) {
        for l in line.as_ref().lines() {
            let l = l.trim_end();
            if l.is_empty() {
                continue;
            }
            if let Err(e) = store::append_job_log(&self.db, &self.job_id, l) {
                tracing::warn!("tentanas job {}: log write failed: {e}", self.job_id);
            }
        }
    }

    pub fn progress(&self, pct: u8) {
        let _ = store::set_job_progress(&self.db, &self.job_id, "running", Some(pct));
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn db(&self) -> &DbPool {
        &self.db
    }
}

/// Creates the row and spawns `body`. The returned job is the row as
/// queued; callers answer with it and the UI polls.
pub fn spawn<F, Fut>(db: &DbPool, kind: &str, subject: &str, started_by: &str, body: F) -> Result<NasJob>
where
    F: FnOnce(JobHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let job = NasJob {
        job_id: uuid::Uuid::now_v7().to_string(),
        kind: kind.to_string(),
        subject: subject.to_string(),
        status: "running".to_string(),
        progress_pct: None,
        started_by: started_by.to_string(),
        started_at: store::now(),
        finished_at: None,
        error: None,
        log: Vec::new(),
    };
    store::insert_job(db, &job)?;
    let cancel = CancellationToken::new();
    running()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(job.job_id.clone(), cancel.clone());
    let handle = JobHandle {
        db: db.clone(),
        job_id: job.job_id.clone(),
        cancel: cancel.clone(),
    };
    let db = db.clone();
    let job_id = job.job_id.clone();
    tokio::spawn(async move {
        let outcome = tokio::select! {
            r = body(handle) => r,
            _ = cancel.cancelled() => Err(anyhow!("cancelled")),
        };
        let (status, error) = match &outcome {
            Ok(()) => ("succeeded", None),
            Err(e) if e.to_string() == "cancelled" => ("cancelled", None),
            Err(e) => ("failed", Some(e.to_string())),
        };
        if let Err(e) = store::finish_job(&db, &job_id, status, error.as_deref()) {
            tracing::warn!("tentanas job {job_id}: finish write failed: {e}");
        }
        running()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&job_id);
    });
    Ok(job)
}

/// Cancels a running job; false when it is not running on this node (already
/// finished, or a job of another process lifetime).
pub fn cancel(job_id: &str) -> bool {
    match running()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(job_id)
    {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

// ----- job bodies ----------------------------------------------------------------

/// Mode A provisioning: stage the sudoers line, run the plan's commands with
/// the one-shot password, verify the chain end-to-end, record the mode.
pub async fn provision_helper(
    h: JobHandle,
    token: Arc<ElevationToken>,
    staging_dir: std::path::PathBuf,
) -> Result<()> {
    let plan = super::elevation::plan(&staging_dir).await;
    if !plan.helper_source_present {
        return Err(anyhow!(
            "helper binary not found at {} — the core package must ship tentanas-helper next to the core binary",
            plan.helper_source
        ));
    }
    std::fs::create_dir_all(&staging_dir)?;
    let staged = staging_dir.join("tentanas-sudoers.staged");
    std::fs::write(&staged, super::elevation::sudoers_line(&plan.core_user))?;
    h.log(format!("staged sudoers line for {}", plan.core_user));
    let total = plan.commands.len();
    for (i, argv) in plan.commands.iter().enumerate() {
        if h.cancelled() {
            return Err(anyhow!("cancelled"));
        }
        h.log(format!("$ {}", argv.join(" ")));
        match super::elevation::run_plan_step(&token, argv).await {
            Ok(out) => h.log(out),
            Err(e) => {
                // A sudoers file that visudo rejects must not stay in place.
                if argv.first().map(String::as_str) == Some("visudo") {
                    let _ = super::elevation::run_plan_step(
                        &token,
                        &["rm".into(), "-f".into(), plan.sudoers_path.clone()],
                    )
                    .await;
                    h.log("removed the rejected sudoers file");
                }
                let _ = std::fs::remove_file(&staged);
                return Err(e);
            }
        }
        h.progress(((i + 1) * 80 / total) as u8);
    }
    let _ = std::fs::remove_file(&staged);
    drop(token);
    let status = super::elevation::helper_status().await;
    h.log(format!("helper state after provisioning: {}", status.state));
    if status.state != "ok" {
        return Err(anyhow!("helper verification failed: {}", status.state));
    }
    super::elevation::set_mode(h.db(), super::elevation::Mode::Helper)?;
    super::disks::request_smart_refresh();
    h.progress(100);
    Ok(())
}

pub async fn remove_helper(h: JobHandle, token: Arc<ElevationToken>) -> Result<()> {
    for argv in super::elevation::removal_commands() {
        h.log(format!("$ {}", argv.join(" ")));
        h.log(super::elevation::run_plan_step(&token, &argv).await?);
    }
    drop(token);
    if super::elevation::mode(h.db()) == super::elevation::Mode::Helper {
        super::elevation::set_mode(h.db(), super::elevation::Mode::Unset)?;
    }
    Ok(())
}

/// Installs one feature's packages, then re-probes the environment so the
/// feature table reflects the result without a manual refresh.
pub async fn install_packages(
    h: JobHandle,
    manager: PackageManager,
    packages: Vec<String>,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    h.log(format!(
        "installing via {}: {}",
        manager.as_str(),
        packages.join(" ")
    ));
    let command = HelperCommand::PackageInstall { manager, packages };
    let (out, channel) = super::broker::run_privileged(
        h.db(),
        &command,
        explicit.as_deref(),
        Duration::from_secs(30 * 60),
    )
    .await?;
    drop(explicit);
    h.log(format!("channel: {}", channel.as_str()));
    h.log(&out.stdout);
    h.log(&out.stderr);
    if !out.success() {
        return Err(anyhow!("package manager exited with {}", out.code));
    }
    h.progress(90);
    super::environment::refresh(h.db()).await?;
    super::disks::request_smart_refresh();
    Ok(())
}

/// Starts a SMART self-test and follows it through `smartctl` polls until
/// the disk reports completion. Progress is what the disk reports.
pub async fn smart_self_test(
    h: JobHandle,
    device: String,
    kind: SelfTestKind,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    let start = HelperCommand::SmartctlSelfTest {
        device: device.clone(),
        kind,
    };
    let (out, channel) = super::broker::run_privileged(
        h.db(),
        &start,
        explicit.as_deref(),
        Duration::from_secs(60),
    )
    .await?;
    if out.code & 0b111 != 0 {
        return Err(anyhow!(
            "smartctl could not start the test ({}): {}",
            out.code,
            out.stderr.trim()
        ));
    }
    h.log(format!("self-test started via {}", channel.as_str()));
    // The one-shot password is consumed by the start; polling uses the
    // node's channel. An interactive node whose TTL expires mid-test leaves
    // the job "running" until the next arm — the disk keeps testing.
    drop(explicit);
    let poll = Duration::from_secs(if kind == SelfTestKind::Short { 20 } else { 120 });
    loop {
        tokio::time::sleep(poll).await;
        if h.cancelled() {
            return Err(anyhow!("cancelled"));
        }
        let doc = match super::disks::read_smart_document(h.db(), &device, None).await {
            Ok(d) => d,
            Err(e) => {
                h.log(format!("poll failed: {e}"));
                continue;
            }
        };
        let summary = super::disks::summarize_smart(&doc);
        match summary.self_test_running_pct {
            Some(pct) => h.progress(pct),
            None => {
                let latest = super::disks::smart_self_tests(&doc).into_iter().next();
                super::disks::request_smart_refresh();
                return match latest {
                    Some(t) if t.status == "failed" => Err(anyhow!("self-test failed: {}", t.detail)),
                    Some(t) => {
                        h.log(format!("result: {}", t.detail));
                        Ok(())
                    }
                    None => Ok(()),
                };
            }
        }
    }
}
